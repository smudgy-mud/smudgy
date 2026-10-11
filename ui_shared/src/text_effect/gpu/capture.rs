//! Exact raster inputs and per-occurrence shaped metadata are kept separate.
use super::{GlyphGeometry, GlyphTable, MAX_GLYPHS, mipmaps};
use iced::{Point, Rectangle, advanced::graphics::text};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RasterGlyph {
    // fontdb IDs include slot generations. Loading another font cannot change
    // this resolved source, and a replacement gets a different ID.
    key: text::cosmic_text::CacheKey,
    offset: [i32; 2],
    color: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Key {
    pub(super) width: u32,
    pub(super) height: u32,
    scale: u32,
    // The entire ordered raster recipe participates in equality, including glyphs
    // beyond the shader geometry table's limit. Source byte offsets do not.
    glyphs: Vec<RasterGlyph>,
}

impl Key {
    pub(super) fn bytes(&self) -> usize {
        // Long strings can have many glyphs even after capture resolution is
        // reduced. Their complete equality recipe also needs a retention bound.
        std::mem::size_of::<Self>() + self.glyphs.capacity() * std::mem::size_of::<RasterGlyph>()
    }
}

pub(super) struct Plan {
    pub(super) key: Key,
    geometry: Vec<GlyphGeometry>,
    pub(super) origin: [f32; 2],
}

/// Native physical paragraph origin and display scale, before capture supersampling.
#[derive(Clone, Copy)]
pub(super) struct Placement {
    pub(super) origin: Point,
    pub(super) scale: f32,
}

impl Plan {
    #[cfg(test)]
    pub(super) fn new(
        paragraph: &text::Paragraph,
        requested_scale: f32,
        region: Rectangle,
    ) -> Self {
        Self::at(
            paragraph,
            requested_scale,
            region,
            Placement {
                origin: Point::new(-region.x, -region.y),
                scale: 1.0,
            },
        )
    }

    pub(super) fn at(
        paragraph: &text::Paragraph,
        requested_scale: f32,
        region: Rectangle,
        placement: Placement,
    ) -> Self {
        let native_scale = placement.scale;
        let anchor = Point::new(
            placement.origin.x + region.x * native_scale,
            placement.origin.y + region.y * native_scale,
        );
        // Keep the texture on the native pixel grid. Its local origin may be
        // fractional; sampling must undo exactly this offset, not assume -2.
        let pixel_origin = Point::new(
            (anchor.x - 2.0 * native_scale).floor(),
            (anchor.y - 2.0 * native_scale).floor(),
        );
        let origin = [
            (pixel_origin.x - anchor.x) / native_scale,
            (pixel_origin.y - anchor.y) / native_scale,
        ];
        let width = ((anchor.x + (region.width + 2.0) * native_scale).ceil() - pixel_origin.x)
            / native_scale;
        let height = ((anchor.y + (region.height + 2.0) * native_scale).ceil() - pixel_origin.y)
            / native_scale;
        // Overflow never enlarges the glyph input. Include all mip levels in its budget.
        let mut scale = requested_scale
            .min(2048.0 / width)
            .min(2048.0 / height)
            .min((262_144.0 / (width * height)).sqrt());
        let (width, height) = loop {
            let w = (width * scale).ceil().clamp(1.0, 2048.0) as u32;
            let h = (height * scale).ceil().clamp(1.0, 2048.0) as u32;
            let bytes = mipmaps::bytes(w, h);
            if bytes <= 1024 * 1024 {
                break (w, h);
            }
            scale *= (1_048_576.0 / bytes as f32).sqrt() * 0.999;
        };
        let mut key = Key {
            width,
            height,
            scale: scale.to_bits(),
            glyphs: Vec::new(),
        };
        let mut geometry = Vec::new();
        for run in paragraph.buffer().layout_runs() {
            for glyph in run.glyphs {
                // An adjacent italic glyph's overhang does not belong to this fragment.
                if !region.contains(Point::new(
                    glyph.x + glyph.w / 2.0,
                    run.line_top + run.line_height / 2.0,
                )) {
                    continue;
                }
                // Match cryoglyph's physical placement first, including quarter-
                // pixel X bins and separately rounded line baselines. Re-hinting
                // at a larger capture size must not choose a different origin.
                let native = glyph.physical((placement.origin.x, placement.origin.y), native_scale);
                let ratio = scale / native_scale;
                let (cache_key, x, y) = text::cosmic_text::CacheKey::new(
                    glyph.font_id,
                    glyph.glyph_id,
                    glyph.font_size * scale,
                    (
                        (native.x as f32 + native.cache_key.x_bin.as_float() - pixel_origin.x)
                            * ratio,
                        (native.y as f32 + (run.line_y * native_scale).round() - pixel_origin.y)
                            * ratio,
                    ),
                    glyph.font_weight,
                    glyph.cache_key_flags,
                );
                let base = glyph
                    .color_opt
                    .unwrap_or(text::cosmic_text::Color::rgb(255, 255, 255));
                key.glyphs.push(RasterGlyph {
                    key: cache_key,
                    offset: [x, y],
                    color: base.0,
                });
                geometry.push(GlyphGeometry {
                    advance: [
                        glyph.x - region.x,
                        run.line_top - region.y,
                        glyph.w,
                        run.line_height,
                    ],
                    ink: [0.0; 4],
                    cluster: [
                        glyph.start as u32,
                        glyph.end as u32,
                        run.line_i as u32,
                        (((run.line_y * native_scale).round() + placement.origin.y.trunc()
                            - anchor.y)
                            / native_scale)
                            .to_bits(),
                    ],
                });
            }
        }
        Self {
            key,
            geometry,
            origin,
        }
    }

    pub(super) fn scale(&self) -> f32 {
        f32::from_bits(self.key.scale)
    }

    pub(super) fn pixels(
        &self,
        swash: &mut text::cosmic_text::SwashCache,
    ) -> (Vec<u8>, Vec<[f32; 4]>) {
        let (w, h) = (self.key.width, self.key.height);
        let mut pixels = vec![0_u8; (w * h * 4) as usize];
        let mut inks = Vec::with_capacity(self.key.glyphs.len());
        let mut fonts = text::font_system().write().unwrap();
        for glyph in &self.key.glyphs {
            let base = text::cosmic_text::Color(glyph.color);
            let mut ink = [
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ];
            swash.with_pixels(fonts.raw(), glyph.key, base, |x, y, color| {
                let x = x + glyph.offset[0];
                let y = y + glyph.offset[1];
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    return;
                }
                let i = ((y as u32 * w + x as u32) * 4) as usize;
                // Swash supplies coverage but ignores the base alpha.
                let alpha = f32::from(color.a()) / 255.0 * f32::from(base.a()) / 255.0;
                if alpha > 0.02 {
                    // Image metadata stays in texels; local placement belongs to
                    // each occurrence, so identical pixels can still be shared.
                    ink[0] = ink[0].min(x as f32);
                    ink[1] = ink[1].min(y as f32);
                    ink[2] = ink[2].max((x + 1) as f32);
                    ink[3] = ink[3].max((y + 1) as f32);
                }
                for (channel, value) in [color.r(), color.g(), color.b()].into_iter().enumerate() {
                    pixels[i + channel] = (f32::from(value) * alpha
                        + f32::from(pixels[i + channel]) * (1.0 - alpha))
                        .round() as u8;
                }
                pixels[i + 3] =
                    (255.0 * alpha + f32::from(pixels[i + 3]) * (1.0 - alpha)).round() as u8;
            });
            inks.push(if ink[0].is_finite() {
                [ink[0], ink[1], ink[2] - ink[0], ink[3] - ink[1]]
            } else {
                [0.0; 4]
            });
        }
        let bytes: usize = swash
            .image_cache
            .values()
            .filter_map(Option::as_ref)
            .map(|image| image.data.len())
            .sum();
        if swash.image_cache.len() > 4096 || bytes > 8 * 1024 * 1024 {
            swash.image_cache.clear();
        }
        (pixels, inks)
    }

    pub(super) fn table(&self, inks: &[[f32; 4]]) -> GlyphTable {
        assert_eq!(self.geometry.len(), inks.len());
        let mut geometry = self.geometry.clone();
        for (glyph, ink) in geometry.iter_mut().zip(inks) {
            glyph.ink = if ink[2] > 0.0 {
                [
                    ink[0] / self.scale() + self.origin[0],
                    ink[1] / self.scale() + self.origin[1],
                    ink[2] / self.scale(),
                    ink[3] / self.scale(),
                ]
            } else {
                [0.0; 4]
            };
        }
        // Visual order supports RTL runs as well as ordinary terminal output.
        geometry.sort_by(|a, b| {
            a.advance[1]
                .total_cmp(&b.advance[1])
                .then(a.advance[0].total_cmp(&b.advance[0]))
        });
        let mut table = GlyphTable {
            info: [
                geometry.len().min(MAX_GLYPHS) as u32,
                u32::from(geometry.len() > MAX_GLYPHS),
                geometry.first().map_or(0, |glyph| glyph.cluster[3]),
                0,
            ],
            items: [GlyphGeometry {
                advance: [0.0; 4],
                ink: [0.0; 4],
                cluster: [0; 4],
            }; MAX_GLYPHS],
        };
        for (to, from) in table.items.iter_mut().zip(geometry) {
            *to = from;
        }
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{
        Font, Pixels, Size,
        advanced::text::{self as api, Paragraph as _},
    };

    fn paragraph(content: &str, size: f32, font: Font) -> text::Paragraph {
        text::Paragraph::with_text(api::Text {
            content,
            bounds: Size::new(10000.0, 200.0),
            size: Pixels(size),
            line_height: api::LineHeight::Absolute(Pixels(32.0)),
            font,
            align_x: api::Alignment::Left,
            align_y: iced::alignment::Vertical::Top,
            shaping: api::Shaping::Advanced,
            wrapping: api::Wrapping::None,
        })
    }

    #[test]
    fn matching_pixels_keep_occurrence_offsets_and_rows_independent() {
        let first = paragraph("office café", 24.0, Font::MONOSPACE);
        let prefix = paragraph(" office café", 24.0, Font::MONOSPACE);
        let later = paragraph("\noffice café", 24.0, Font::MONOSPACE);
        let region = Rectangle::with_size(Size::new(300.0, 32.0));
        let x = prefix.buffer().layout_runs().next().unwrap().glyphs[1].x;
        let plans = [
            Plan::new(&first, 4.0, region),
            Plan::new(&prefix, 4.0, Rectangle { x, ..region }),
            Plan::new(&later, 4.0, Rectangle { y: 32.0, ..region }),
        ];
        assert_eq!(plans[0].key, plans[1].key);
        assert_eq!(plans[0].key, plans[2].key);
        let mut swash = text::cosmic_text::SwashCache::new();
        let (pixels, ink) = plans[0].pixels(&mut swash);
        let first = plans[0].table(&ink);
        for (index, plan) in plans.iter().enumerate().skip(1) {
            assert_eq!(pixels, plan.pixels(&mut swash).0);
            let other = plan.table(&ink);
            assert_eq!(first.info[0], other.info[0]);
            for (a, b) in first
                .items
                .iter()
                .zip(&other.items)
                .take(first.info[0] as usize)
            {
                assert_eq!(a.ink, b.ink);
                assert_eq!(a.cluster[3], b.cluster[3]);
                if index == 1 {
                    assert_eq!(a.cluster[0] + 1, b.cluster[0]);
                } else {
                    assert_eq!(a.cluster[2] + 1, b.cluster[2]);
                }
            }
        }
    }

    #[test]
    fn shared_pixels_keep_fractional_origins_in_occurrence_geometry() {
        let paragraph = paragraph("Hollow ink", 24.0, Font::MONOSPACE);
        let region = Rectangle::with_size(paragraph.min_bounds());
        let placements = [
            Point::new(40.25, 40.25),
            Point::new(40.3, 40.3),
            Point::new(940.25, 760.25),
        ];
        let plans = placements
            .map(|origin| Plan::at(&paragraph, 4.0, region, Placement { origin, scale: 1.0 }));
        let mut swash = text::cosmic_text::SwashCache::new();
        let (pixels, ink) = plans[0].pixels(&mut swash);
        let reference = plans[0].table(&ink);
        for (plan, at) in plans.iter().zip(placements).skip(1) {
            assert_eq!(plans[0].key, plan.key, "identical native bins share pixels");
            assert_eq!(pixels, plan.pixels(&mut swash).0);
            let table = plan.table(&ink);
            for (a, b) in reference
                .items
                .iter()
                .zip(&table.items)
                .take(table.info[0] as usize)
            {
                assert_eq!(a.advance, b.advance, "layout stays in source coordinates");
                if a.ink[2] == 0.0 {
                    continue;
                }
                let expected = iced::Vector::new(
                    placements[0].x.fract() - at.x.fract(),
                    placements[0].y.fract() - at.y.fract(),
                );
                assert!((a.ink[0] + expected.x - b.ink[0]).abs() < 0.001);
                assert!((a.ink[1] + expected.y - b.ink[1]).abs() < 0.001);
            }
        }
    }

    #[test]
    fn exact_raster_inputs_distinguish_typography_color_resolution_and_placement() {
        let p = paragraph("Ink", 24.0, Font::MONOSPACE);
        let region = Rectangle::with_size(Size::new(100.0, 32.0));
        let plan = Plan::new(&p, 2.0, region);
        for (size, font) in [
            (25.0, Font::MONOSPACE),
            (24.0, Font::DEFAULT),
            (
                24.0,
                Font {
                    weight: iced::font::Weight::Bold,
                    ..Font::MONOSPACE
                },
            ),
            (
                24.0,
                Font {
                    style: iced::font::Style::Italic,
                    ..Font::MONOSPACE
                },
            ),
        ] {
            assert_ne!(
                plan.key,
                Plan::new(&paragraph("Ink", size, font), 2.0, region).key
            );
        }
        assert_ne!(plan.key, Plan::new(&p, 4.0, region).key);
        assert_ne!(
            plan.key,
            Plan::new(&p, 2.0, Rectangle { x: 0.2, ..region }).key
        );
        assert_ne!(
            plan.key,
            Plan::new(
                &p,
                2.0,
                Rectangle {
                    width: 101.0,
                    ..region
                }
            )
            .key
        );
        for color in [0xffff0000, 0x80ffffff] {
            let mut other = plan.key.clone();
            other.glyphs[0].color = color;
            assert_ne!(plan.key, other, "RGB and alpha both participate");
        }
        let mut swapped = plan.key.clone();
        swapped.glyphs.swap(0, 1);
        assert_ne!(
            plan.key, swapped,
            "overlapping ink must blend in the same order"
        );
        let oversized = paragraph(&"W".repeat(80), 24.0, Font::MONOSPACE);
        let region = Rectangle::with_size(oversized.min_bounds());
        assert_eq!(
            Plan::new(&oversized, 8.0, region).key,
            Plan::new(&oversized, 16.0, region).key,
            "use effective, budget-limited resolution"
        );
    }

    #[test]
    fn key_includes_glyphs_beyond_geometry_limit_and_preserves_rtl_and_combining_ink() {
        let region = Rectangle::with_size(Size::new(10000.0, 32.0));
        let a = paragraph(&format!("{}i", "a".repeat(299)), 24.0, Font::MONOSPACE);
        let b = paragraph(&format!("{}l", "a".repeat(299)), 24.0, Font::MONOSPACE);
        let a = Plan::new(&a, 1.0, region);
        let b = Plan::new(&b, 1.0, region);
        assert!(a.key.glyphs.len() > MAX_GLYPHS);
        assert_ne!(a.key, b.key);
        for content in ["e\u{301} café", "שלום", "ffi 東京"] {
            let p = paragraph(content, 24.0, Font::MONOSPACE);
            let q = paragraph(content, 24.0, Font::MONOSPACE);
            let a = Plan::new(&p, 2.0, region);
            let b = Plan::new(&q, 2.0, region);
            assert_eq!(a.key, b.key);
            let mut swash = text::cosmic_text::SwashCache::new();
            assert_eq!(a.pixels(&mut swash), b.pixels(&mut swash));
        }
    }
}
