//! The terminal graphics decoder: kitty graphics transmits, decoded off the socket thread.
//!
//! Every session's connections hand their pixel payloads to the process-wide
//! [`UiGraphicsDecoder`], core's [`GraphicsDecoder`]. A request arrives checked as far as
//! its bytes allow without inflating or decoding them; the rest runs here, on the image
//! store's `smudgy-images` runtime, at most [`MAX_CONCURRENT_GRAPHICS_DECODES`] at a time:
//! inflate `o=z` data (packed formats capped at their exact length, PNG at
//! [`png_inflate_cap`]), expand RGB to RGBA, check packed lengths exactly, and decode PNG
//! under the same limits as the `<Image>` fetcher, and compose an animation frame onto
//! its canvas (an `a=c` carries no data: its rectangle is read from the source frame).
//! What comes back is straight RGBA8 of exactly the requested size — the canvas, for a
//! frame — or an `EINVAL` that fails the transmit.

use std::sync::{Arc, OnceLock};

use smudgy_core::session::connection::GraphicsDecoder;
use smudgy_protocol::graphics::{
    Compression, DecodeRequest, DecodeResult, DecodedPixels, ErrorCode, Format, GraphicsError,
    MAX_IMAGE_BYTES,
};
use tokio::sync::Semaphore;

/// Concurrent decode bound, like the `<Image>` fetcher's: decodes are CPU work on the
/// store's two-worker runtime, and more than this only delays the first image.
const MAX_CONCURRENT_GRAPHICS_DECODES: usize = 2;

/// How far a compressed PNG payload may inflate: as far as the processor admits an
/// image's pixels ([`MAX_IMAGE_BYTES`]), so a PNG that was admitted is never refused
/// here for its file size alone. A PNG is itself compressed, so a zlib-wrapped one this
/// large is far beyond anything a server sends over a terminal stream; the decoded size
/// is bounded separately by the request's dimensions.
fn png_inflate_cap() -> usize {
    usize::try_from(MAX_IMAGE_BYTES).unwrap_or(usize::MAX)
}

/// The decoder every session hands its connections, built on first use over the
/// process-global image store's runtime.
pub fn graphics_decoder() -> Arc<UiGraphicsDecoder> {
    static DECODER: OnceLock<Arc<UiGraphicsDecoder>> = OnceLock::new();
    DECODER
        .get_or_init(|| {
            Arc::new(UiGraphicsDecoder::new(
                super::image_store().runtime_handle(),
            ))
        })
        .clone()
}

/// Decodes kitty graphics transmits on a multi-threaded runtime, a bounded number at a
/// time, and reports each result through the callback core hands it.
pub struct UiGraphicsDecoder {
    runtime: tokio::runtime::Handle,
    permits: Arc<Semaphore>,
}

impl UiGraphicsDecoder {
    /// A decoder spawning onto `runtime`, which must be multi-threaded: each decode
    /// blocks its worker in place.
    pub fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            runtime,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_GRAPHICS_DECODES)),
        }
    }
}

impl GraphicsDecoder for UiGraphicsDecoder {
    fn decode(&self, request: DecodeRequest, done: Box<dyn FnOnce(DecodeResult) + Send + 'static>) {
        let permits = self.permits.clone();
        // A task that never runs `done` (a shut-down runtime, a panicking decode) drops
        // it instead, and core reports that drop as a failed decode.
        self.runtime.spawn(async move {
            let result = match permits.acquire_owned().await {
                // The permit spans only the CPU-bound decode, not the callback.
                Ok(_permit) => tokio::task::block_in_place(|| decode(request)),
                Err(_) => Err(GraphicsError::invalid("decoder shut down")),
            };
            done(result);
        });
    }
}

/// Decode one request to straight RGBA8 of exactly its declared size — composed onto
/// its canvas, and then the canvas's size, for an animation frame.
fn decode(request: DecodeRequest) -> DecodeResult {
    let DecodeRequest {
        format,
        compression,
        width,
        height,
        data,
        compose,
    } = request;
    // The processor checked these already; a decoder that trusted them blindly would
    // allocate whatever a bad request asked for. A frame's canvas is held to the same
    // limits: with no base frame it is allocated here, before anything is composed.
    if !size_in_range(width, height) {
        return Err(GraphicsError::invalid(format!(
            "image size {width}x{height} is out of range"
        )));
    }
    if let Some((canvas_width, canvas_height)) = compose.as_ref().map(|compose| compose.canvas)
        && !size_in_range(canvas_width, canvas_height)
    {
        return Err(GraphicsError::invalid(format!(
            "frame canvas {canvas_width}x{canvas_height} is out of range"
        )));
    }
    // An `a=c` carries no data: its rectangle is read from the source frame when it is
    // composed.
    let from_source = compose
        .as_ref()
        .is_some_and(|compose| compose.source.is_some());
    let rgba = if from_source {
        if !data.is_empty() {
            return Err(GraphicsError::invalid(
                "a compose from a frame carries no data",
            ));
        }
        Vec::new()
    } else {
        match format {
            Format::Rgb | Format::Rgba => decode_packed(format, compression, width, height, data)?,
            Format::Png => decode_png(compression, width, height, &data)?,
            Format::Unsupported(number) => {
                return Err(GraphicsError::invalid(format!(
                    "Unknown image format: {number}"
                )));
            }
        }
    };
    match compose {
        Some(compose) => {
            let rgba = compose.apply(&rgba, width, height)?;
            Ok(DecodedPixels {
                width: compose.canvas.0,
                height: compose.canvas.1,
                rgba: rgba.into(),
            })
        }
        None => Ok(DecodedPixels {
            width,
            height,
            rgba: rgba.into(),
        }),
    }
}

/// Whether a `width` x `height` image is one kitty may hold: neither side empty or past
/// [`MAX_IMAGE_DIMENSION`](smudgy_protocol::graphics::MAX_IMAGE_DIMENSION), and its RGBA
/// within [`MAX_IMAGE_BYTES`](smudgy_protocol::graphics::MAX_IMAGE_BYTES).
fn size_in_range(width: u32, height: u32) -> bool {
    let max_dimension = smudgy_protocol::graphics::MAX_IMAGE_DIMENSION;
    width != 0
        && height != 0
        && width <= max_dimension
        && height <= max_dimension
        && u64::from(width) * u64::from(height) * 4 <= smudgy_protocol::graphics::MAX_IMAGE_BYTES
}

/// Inflate a zlib payload to at most `cap` bytes; a longer stream is `EINVAL`, like a
/// corrupt one.
fn inflate(data: &[u8], cap: usize) -> Result<Vec<u8>, GraphicsError> {
    smudgy_protocol::graphics::inflate(data, cap).map_err(|error| {
        if error.code == ErrorCode::Invalid {
            error
        } else {
            GraphicsError::invalid("Too much image data")
        }
    })
}

/// Packed RGB or RGBA: inflated within the exact length the size demands, then exactly
/// that long, RGB widened to opaque RGBA.
fn decode_packed(
    format: Format,
    compression: Compression,
    width: u32,
    height: u32,
    data: Vec<u8>,
) -> Result<Vec<u8>, GraphicsError> {
    let expected = format
        .expected_len(width, height)
        .and_then(|len| usize::try_from(len).ok())
        .ok_or_else(|| GraphicsError::invalid("Unknown image format"))?;
    let data = match compression {
        Compression::None => data,
        Compression::Zlib => inflate(&data, expected)?,
    };
    if data.len() != expected {
        return Err(GraphicsError::invalid(format!(
            "image data is {} bytes; {width}x{height} needs {expected}",
            data.len()
        )));
    }
    Ok(if format == Format::Rgb {
        let mut rgba = Vec::with_capacity(expected / 3 * 4);
        for pixel in data.chunks_exact(3) {
            rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
        }
        rgba
    } else {
        data
    })
}

/// A whole PNG file, inflated first when it came compressed, decoded under the
/// fetcher's limits and required to be the size its header announced.
fn decode_png(
    compression: Compression,
    width: u32,
    height: u32,
    data: &[u8],
) -> Result<Vec<u8>, GraphicsError> {
    let inflated;
    let data = match compression {
        Compression::None => data,
        Compression::Zlib => {
            inflated = inflate(data, png_inflate_cap())?;
            &inflated
        }
    };
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(data), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(super::MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(super::MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(super::MAX_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|error| {
        log::debug!("kitty graphics: PNG decode failed: {error}");
        GraphicsError::invalid("PNG data could not be decoded")
    })?;
    if (decoded.width(), decoded.height()) != (width, height) {
        return Err(GraphicsError::invalid(format!(
            "PNG decoded to {}x{}, not the {width}x{height} its header announced",
            decoded.width(),
            decoded.height()
        )));
    }
    Ok(decoded.into_rgba8().into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` x `height` PNG whose pixel at (x, y) is `[x, y, x + y, 255]` scaled.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x * 50) as u8, (y * 50) as u8, ((x + y) * 20) as u8, 255])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    /// `png`'s pixels, as RGBA8.
    fn png_pixels(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..height {
            for x in 0..width {
                out.extend_from_slice(&[(x * 50) as u8, (y * 50) as u8, ((x + y) * 20) as u8, 255]);
            }
        }
        out
    }

    /// `data` as a zlib stream of stored deflate blocks: valid RFC 1950 without needing
    /// a compressor.
    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let chunks: Vec<&[u8]> = if data.is_empty() {
            vec![&[]]
        } else {
            data.chunks(0xffff).collect()
        };
        for (index, chunk) in chunks.iter().enumerate() {
            out.push(u8::from(index + 1 == chunks.len()));
            let len = u16::try_from(chunk.len()).unwrap();
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    fn request(
        format: Format,
        compression: Compression,
        width: u32,
        height: u32,
        data: Vec<u8>,
    ) -> DecodeRequest {
        DecodeRequest {
            format,
            compression,
            width,
            height,
            data,
            compose: None,
        }
    }

    fn assert_einval(result: DecodeResult) {
        let error = result.expect_err("the decode must fail");
        assert_eq!(error.code, ErrorCode::Invalid, "{error}");
    }

    #[test]
    fn decodes_a_png() {
        let pixels = decode(request(Format::Png, Compression::None, 2, 2, png(2, 2))).unwrap();
        assert_eq!((pixels.width, pixels.height), (2, 2));
        assert_eq!(&pixels.rgba[..], &png_pixels(2, 2)[..]);
    }

    #[test]
    fn inflates_a_compressed_png() {
        let pixels = decode(request(
            Format::Png,
            Compression::Zlib,
            3,
            2,
            zlib(&png(3, 2)),
        ))
        .unwrap();
        assert_eq!(&pixels.rgba[..], &png_pixels(3, 2)[..]);
    }

    /// `png` with an ancillary chunk of `padding` zero bytes before its `IEND`, which a
    /// decoder reads past: a valid PNG file as large as the chunk makes it.
    fn padded_png(png: &[u8], padding: usize) -> Vec<u8> {
        let mut table = [0u32; 256];
        for (n, slot) in table.iter_mut().enumerate() {
            let mut crc = u32::try_from(n).unwrap();
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xedb8_8320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
            *slot = crc;
        }
        let mut crc = 0xffff_ffffu32;
        let mut update = |bytes: &[u8]| {
            for &byte in bytes {
                let index = usize::try_from((crc ^ u32::from(byte)) & 0xff).unwrap();
                crc = table[index] ^ (crc >> 8);
            }
        };
        let kind = b"prIv";
        update(kind);
        let zeros = vec![0u8; padding];
        update(&zeros);
        let (body, iend) = png.split_at(png.len() - 12);
        assert_eq!(&iend[4..8], b"IEND");
        let mut out = body.to_vec();
        out.extend_from_slice(&u32::try_from(padding).unwrap().to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(&zeros);
        out.extend_from_slice(&(!crc).to_be_bytes());
        out.extend_from_slice(iend);
        out
    }

    #[test]
    fn a_compressed_png_inflates_as_far_as_the_processor_admits() {
        // Past the 16 MiB a PNG once had here and well within what the processor
        // admits ([`MAX_IMAGE_BYTES`]): admitted there, decoded here.
        let padded = padded_png(&png(3, 2), 17 * 1024 * 1024);
        let pixels = decode(request(Format::Png, Compression::Zlib, 3, 2, zlib(&padded))).unwrap();
        assert_eq!(&pixels.rgba[..], &png_pixels(3, 2)[..]);
        assert_eq!(png_inflate_cap(), 128 * 1024 * 1024);
        assert_eq!(
            u64::try_from(png_inflate_cap()).unwrap(),
            MAX_IMAGE_BYTES,
            "the cap is the admission's"
        );
        // Past the cap it is refused as too much data, like any oversized stream.
        let error = inflate(&zlib(&[0; 64]), 63).unwrap_err();
        assert_eq!(error.to_string(), "EINVAL:Too much image data");
    }

    #[test]
    fn expands_rgb_to_opaque_rgba() {
        let rgb = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let expected = vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        let pixels = decode(request(Format::Rgb, Compression::None, 2, 2, rgb.clone())).unwrap();
        assert_eq!(&pixels.rgba[..], &expected[..]);
        let pixels = decode(request(Format::Rgb, Compression::Zlib, 2, 2, zlib(&rgb))).unwrap();
        assert_eq!(&pixels.rgba[..], &expected[..]);
    }

    #[test]
    fn passes_rgba_through() {
        let rgba: Vec<u8> = (0..16).collect();
        let pixels = decode(request(Format::Rgba, Compression::None, 2, 2, rgba.clone())).unwrap();
        assert_eq!((pixels.width, pixels.height), (2, 2));
        assert_eq!(&pixels.rgba[..], &rgba[..]);
        let pixels = decode(request(Format::Rgba, Compression::Zlib, 1, 4, zlib(&rgba))).unwrap();
        assert_eq!(&pixels.rgba[..], &rgba[..]);
    }

    #[test]
    fn refuses_packed_data_of_the_wrong_length() {
        assert_einval(decode(request(
            Format::Rgba,
            Compression::None,
            2,
            2,
            vec![0; 15],
        )));
        assert_einval(decode(request(
            Format::Rgba,
            Compression::None,
            2,
            2,
            vec![0; 17],
        )));
        assert_einval(decode(request(
            Format::Rgb,
            Compression::None,
            2,
            2,
            vec![0; 16],
        )));
        // Compressed data inflating short, or past the size, is refused too.
        assert_einval(decode(request(
            Format::Rgba,
            Compression::Zlib,
            2,
            2,
            zlib(&[0; 12]),
        )));
        assert_einval(decode(request(
            Format::Rgba,
            Compression::Zlib,
            2,
            2,
            zlib(&[0; 20]),
        )));
        assert_einval(decode(request(
            Format::Rgba,
            Compression::Zlib,
            2,
            2,
            vec![1, 2, 3],
        )));
    }

    #[test]
    fn refuses_an_oversized_or_mismatched_size() {
        // Over kitty's dimension limit, though the length matches.
        assert_einval(decode(request(
            Format::Rgba,
            Compression::None,
            10_001,
            1,
            vec![0; 40_004],
        )));
        assert_einval(decode(request(
            Format::Rgba,
            Compression::None,
            0,
            1,
            Vec::new(),
        )));
        // A PNG must decode to the size its header announced.
        assert_einval(decode(request(
            Format::Png,
            Compression::None,
            3,
            3,
            png(2, 2),
        )));
    }

    #[test]
    fn refuses_corrupt_png_and_unknown_formats() {
        let mut broken = png(2, 2);
        broken.truncate(30);
        assert_einval(decode(request(
            Format::Png,
            Compression::None,
            2,
            2,
            broken,
        )));
        assert_einval(decode(request(
            Format::Unsupported(8),
            Compression::None,
            2,
            2,
            vec![0; 4],
        )));
    }

    // Animation frames: the decoded rectangle composed onto its canvas.

    use smudgy_protocol::graphics::{Compose, ComposeMode, ComposeSource, blend_over};

    /// `width` x `height` RGBA8 whose pixel at (x, y) is `pixel(x, y)`.
    fn canvas(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| pixel(x, y))
            .collect()
    }

    fn pixels(width: u32, height: u32, rgba: Vec<u8>) -> DecodedPixels {
        DecodedPixels {
            width,
            height,
            rgba: rgba.into(),
        }
    }

    /// The RGBA8 pixel at (x, y) of a `width`-wide buffer.
    fn at(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * width + x) * 4) as usize;
        rgba[index..index + 4].try_into().unwrap()
    }

    /// A frame request: `data`, `width` x `height` of `format`, landing at (`x`, `y`) on
    /// `compose`'s canvas.
    fn frame(
        format: Format,
        width: u32,
        height: u32,
        data: Vec<u8>,
        compose: Compose,
    ) -> DecodeRequest {
        DecodeRequest {
            compose: Some(compose),
            ..request(format, Compression::None, width, height, data)
        }
    }

    /// A 4x3 canvas over `base` (or the background `background`), the rectangle at (1, 1).
    fn compose_at(base: Option<DecodedPixels>, background: u32, mode: ComposeMode) -> Compose {
        Compose {
            base,
            source: None,
            background,
            x: 1,
            y: 1,
            mode,
            canvas: (4, 3),
        }
    }

    /// A 4x3 base frame, a distinct opaque colour per pixel.
    fn base_frame() -> DecodedPixels {
        pixels(
            4,
            3,
            canvas(4, 3, |x, y| [(x * 60) as u8, (y * 100) as u8, 200, 255]),
        )
    }

    /// The pixel `over` lands as on `under`, by kitty's straight-alpha blend.
    fn blended(under: [u8; 4], over: [u8; 4]) -> [u8; 4] {
        let mut under = under;
        blend_over(&mut under, &over);
        under
    }

    #[test]
    fn a_partial_frame_alpha_blends_onto_its_base_frame() {
        // A 2x1 rectangle at (1, 1): one translucent pixel, one fully transparent.
        let rectangle = vec![255, 0, 0, 128, 9, 9, 9, 0];
        let base = base_frame();
        let pixels = decode(frame(
            Format::Rgba,
            2,
            1,
            rectangle,
            compose_at(Some(base.clone()), 0, ComposeMode::AlphaBlend),
        ))
        .unwrap();
        assert_eq!((pixels.width, pixels.height), (4, 3), "the whole canvas");
        assert_eq!(pixels.rgba.len(), 4 * 3 * 4);
        for y in 0..3 {
            for x in 0..4 {
                let under = at(&base.rgba, 4, x, y);
                let expected = match (x, y) {
                    (1, 1) => blended(under, [255, 0, 0, 128]),
                    // A transparent pixel leaves the base as it was.
                    _ => under,
                };
                assert_eq!(at(&pixels.rgba, 4, x, y), expected, "pixel ({x}, {y})");
            }
        }
        assert_ne!(at(&pixels.rgba, 4, 1, 1), at(&base.rgba, 4, 1, 1));
    }

    #[test]
    fn overwrite_replaces_the_rectangle_alpha_and_all() {
        let rectangle = vec![255, 0, 0, 128, 9, 9, 9, 0];
        let base = base_frame();
        let pixels = decode(frame(
            Format::Rgba,
            2,
            1,
            rectangle,
            compose_at(Some(base.clone()), 0, ComposeMode::Overwrite),
        ))
        .unwrap();
        for y in 0..3 {
            for x in 0..4 {
                let expected = match (x, y) {
                    (1, 1) => [255, 0, 0, 128],
                    (2, 1) => [9, 9, 9, 0],
                    _ => at(&base.rgba, 4, x, y),
                };
                assert_eq!(at(&pixels.rgba, 4, x, y), expected, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_frame_without_a_base_lands_on_the_background_colour() {
        // Y=0xff0000ff, opaque red; the rectangle is RGB, widened to opaque first.
        let red = [0xff, 0, 0, 0xff];
        for mode in [ComposeMode::AlphaBlend, ComposeMode::Overwrite] {
            let pixels = decode(frame(
                Format::Rgb,
                2,
                2,
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
                compose_at(None, 0xff00_00ff, mode),
            ))
            .unwrap();
            let expected = canvas(4, 3, |x, y| match (x, y) {
                (1, 1) => [1, 2, 3, 255],
                (2, 1) => [4, 5, 6, 255],
                (1, 2) => [7, 8, 9, 255],
                (2, 2) => [10, 11, 12, 255],
                _ => red,
            });
            assert_eq!(&pixels.rgba[..], &expected[..], "{mode:?}");
        }
        // Without `Y` the canvas is transparent black, and blending a translucent
        // pixel onto it keeps the pixel's own colour.
        let pixels = decode(frame(
            Format::Rgba,
            1,
            1,
            vec![10, 20, 30, 40],
            compose_at(None, 0, ComposeMode::AlphaBlend),
        ))
        .unwrap();
        assert_eq!(at(&pixels.rgba, 4, 1, 1), [10, 20, 30, 40]);
        assert_eq!(at(&pixels.rgba, 4, 0, 0), [0, 0, 0, 0]);
        assert_eq!(at(&pixels.rgba, 4, 3, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn a_compressed_png_frame_decodes_before_it_composes() {
        let pixels = decode(DecodeRequest {
            compose: Some(Compose {
                x: 2,
                y: 0,
                ..compose_at(Some(base_frame()), 0, ComposeMode::Overwrite)
            }),
            ..request(Format::Png, Compression::Zlib, 2, 2, zlib(&png(2, 2)))
        })
        .unwrap();
        let png = png_pixels(2, 2);
        for (x, y) in [(2, 0), (3, 0), (2, 1), (3, 1)] {
            assert_eq!(at(&pixels.rgba, 4, x, y), at(&png, 2, x - 2, y));
        }
        assert_eq!(at(&pixels.rgba, 4, 1, 0), at(&base_frame().rgba, 4, 1, 0));
    }

    #[test]
    fn a_frame_compose_reads_its_rectangle_from_the_source_frame() {
        // a=c: a 2x2 rectangle from (1, 1) of a 4x3 source onto (0, 0) of the base,
        // with no data of its own.
        let source = pixels(4, 3, canvas(4, 3, |x, y| [x as u8, y as u8, 7, 128]));
        let base = base_frame();
        let compose = Compose {
            base: Some(base.clone()),
            source: Some(ComposeSource {
                pixels: source.clone(),
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }),
            background: 0,
            x: 0,
            y: 0,
            mode: ComposeMode::Overwrite,
            canvas: (4, 3),
        };
        let request = frame(Format::Rgba, 2, 2, Vec::new(), compose.clone());
        let pixels = decode(request).unwrap();
        assert_eq!((pixels.width, pixels.height), (4, 3));
        for y in 0..3 {
            for x in 0..4 {
                let expected = if x < 2 && y < 2 {
                    at(&source.rgba, 4, x + 1, y + 1)
                } else {
                    at(&base.rgba, 4, x, y)
                };
                assert_eq!(at(&pixels.rgba, 4, x, y), expected, "pixel ({x}, {y})");
            }
        }
        // Blended, the source's translucent pixels mix with the base's.
        let blend = decode(frame(
            Format::Rgba,
            2,
            2,
            Vec::new(),
            Compose {
                mode: ComposeMode::AlphaBlend,
                ..compose.clone()
            },
        ))
        .unwrap();
        assert_eq!(
            at(&blend.rgba, 4, 1, 0),
            blended(at(&base.rgba, 4, 1, 0), at(&source.rgba, 4, 2, 1))
        );
        // The rectangle must lie inside the source, and the request carries no data.
        let past_the_edge = Compose {
            source: Some(ComposeSource {
                x: 3,
                ..compose.source.clone().unwrap()
            }),
            ..compose.clone()
        };
        assert_einval(decode(frame(Format::Rgba, 2, 2, Vec::new(), past_the_edge)));
        assert_einval(decode(frame(Format::Rgba, 2, 2, vec![0; 16], compose)));
    }

    #[test]
    fn refuses_a_frame_that_does_not_fit_its_canvas() {
        let rectangle = || vec![0; 2 * 2 * 4];
        // The rectangle leaves the canvas: 2 wide at x = 3 of 4.
        let outside = Compose {
            x: 3,
            ..compose_at(None, 0, ComposeMode::Overwrite)
        };
        assert_einval(decode(frame(Format::Rgba, 2, 2, rectangle(), outside)));
        // A base frame that is not the canvas's size.
        let wrong_base = compose_at(Some(pixels(2, 2, vec![0; 16])), 0, ComposeMode::AlphaBlend);
        assert_einval(decode(frame(Format::Rgba, 2, 2, rectangle(), wrong_base)));
        // A canvas past kitty's limits is refused before anything is allocated for it.
        for canvas in [(10_001, 1), (0, 4), (10_000, 10_000)] {
            let huge = Compose {
                canvas,
                ..compose_at(None, 0xffff_ffff, ComposeMode::Overwrite)
            };
            assert_einval(decode(frame(Format::Rgba, 2, 2, rectangle(), huge)));
        }
        // Data of the wrong length still fails the decode itself.
        let fits = compose_at(None, 0, ComposeMode::Overwrite);
        assert_einval(decode(frame(Format::Rgba, 2, 2, vec![0; 15], fits)));
    }

    #[test]
    fn decodes_off_thread_and_calls_back_once() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .unwrap();
        let decoder = UiGraphicsDecoder::new(runtime.handle().clone());
        let (tx, rx) = std::sync::mpsc::channel();
        for (format, data) in [(Format::Png, png(2, 2)), (Format::Rgba, vec![0; 3])] {
            let tx = tx.clone();
            decoder.decode(
                request(format, Compression::None, 2, 2, data),
                Box::new(move |result| tx.send(result).unwrap()),
            );
        }
        drop(tx);
        let results: Vec<DecodeResult> = rx.iter().collect();
        assert_eq!(results.len(), 2, "one callback per request");
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let pixels = results.into_iter().find_map(Result::ok).unwrap();
        assert_eq!(&pixels.rgba[..], &png_pixels(2, 2)[..]);
    }
}
