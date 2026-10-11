mod abi;
mod cache;
mod capture;
mod mipmaps;
mod noise;
mod programs;
#[cfg(test)]
mod reuse_tests;
use super::Input;
use abi::{GlyphGeometry, GlyphTable, MAX_GLYPHS, Uniforms};
use iced::{Rectangle, advanced::graphics::text, wgpu, widget::shader};
use iced_wgpu::primitive::Renderer as _;
use rustc_hash::FxHashMap;
use smudgy_session_model::{inline_content::InlineDecoration, text_shader::ShaderEffect};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[cfg(test)]
use crate::profiling::capture_stats as global_stats;
#[cfg(any(test, feature = "profiling"))]
use crate::profiling::{
    CaptureCounters as Counters, TOTAL_BYTES, TOTAL_HITS, TOTAL_KEYS, TOTAL_UPLOADS,
};
#[derive(Debug)]
struct GlyphPixels {
    view: wgpu::TextureView,
    size: [f32; 2],
    scale: f32,
    // Texture-space texels; occurrence geometry applies its own local origin.
    ink: Vec<[f32; 4]>,
    bytes: usize,
}
#[cfg(any(test, feature = "profiling"))]
impl Drop for GlyphPixels {
    fn drop(&mut self) {
        TOTAL_BYTES.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
#[derive(Debug)]
struct GlyphTexture {
    // Holding a clone makes paragraph mutations copy-on-write; buffer identity is reliable.
    _paragraph: text::Paragraph,
    // Prevent allocator address reuse while this occurrence remains cached.
    _identity: Arc<()>,
    pixels: Arc<GlyphPixels>,
    geometry: wgpu::Buffer,
    glyph_count: u32,
    origin: [f32; 2],
    #[cfg(any(test, feature = "profiling"))]
    bytes: usize,
    #[cfg(any(test, feature = "profiling"))]
    counters: Arc<Counters>,
    used: std::sync::atomic::AtomicBool,
}
#[cfg(any(test, feature = "profiling"))]
impl Drop for GlyphTexture {
    fn drop(&mut self) {
        TOTAL_BYTES.fetch_sub(std::mem::size_of::<GlyphTable>(), Ordering::Relaxed);
        self.counters
            .resident_bytes
            .fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
#[derive(Debug)]
struct Slot {
    uniforms: wgpu::Buffer,
    group: wgpu::BindGroup,
    params: wgpu::Buffer,
    params_group: wgpu::BindGroup,
    glyphs: Arc<GlyphTexture>,
    program: Arc<wgpu::RenderPipeline>,
}
#[derive(Debug)]
pub(super) struct Pipeline {
    sampler: wgpu::Sampler,
    noise: noise::Noise,
    mipmaps: mipmaps::Mipmaps,
    glyphs: FxHashMap<CaptureKey, Arc<GlyphTexture>>,
    pixels: cache::Cache<capture::Key, GlyphPixels>,
    swash: text::cosmic_text::SwashCache,
    slots: Vec<Slot>,
    used: usize,
    linearize: u32,
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct CaptureKey {
    paragraph: usize,
    identity: usize,
    region: [u32; 4],
    scale: u32,
    placement: [u32; 3],
}
impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("text effect sampling"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mipmaps = mipmaps::Mipmaps::new(device);
        Self {
            sampler,
            noise: noise::Noise::new(device, queue, &mipmaps),
            mipmaps,
            glyphs: FxHashMap::default(),
            pixels: cache::Cache::new(),
            swash: text::cosmic_text::SwashCache::new(),
            slots: vec![],
            used: 0,
            linearize: u32::from(format.is_srgb()),
        }
    }
    fn trim(&mut self) {
        self.slots.truncate(self.used);
        if self.used == 0 {
            self.swash = text::cosmic_text::SwashCache::new();
        }
        self.used = 0;
        self.glyphs
            .retain(|_, g| g.used.swap(false, Ordering::Relaxed));
        self.pixels.trim();
    }
}
impl Pipeline {
    fn group(
        &self,
        device: &wgpu::Device,
        uniforms: &wgpu::Buffer,
        glyphs: &GlyphTexture,
        programs: &programs::Programs,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text effect instance"),
            layout: &programs.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&glyphs.pixels.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: glyphs.geometry.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&self.noise.view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&self.noise.sampler),
                },
            ],
        })
    }
}
#[cfg(test)]
fn rasterize(
    paragraph: &text::Paragraph,
    scale: f32,
    region: Rectangle,
    swash: &mut text::cosmic_text::SwashCache,
) -> (u32, u32, f32, Vec<u8>, GlyphTable) {
    let plan = capture::Plan::new(paragraph, scale, region);
    let (pixels, ink) = plan.pixels(swash);
    (
        plan.key.width,
        plan.key.height,
        plan.scale(),
        pixels,
        plan.table(&ink),
    )
}

fn upload_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    plan: &capture::Plan,
    swash: &mut text::cosmic_text::SwashCache,
    mipmaps: &mipmaps::Mipmaps,
) -> Arc<GlyphPixels> {
    let (pixels, ink) = plan.pixels(swash);
    let (width, height) = (plan.key.width, plan.key.height);
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cached text colour and coverage"),
        size,
        mip_level_count: mipmaps::levels(width, height),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        size,
    );
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("filter captured glyphs once"),
    });
    mipmaps.encode(device, &mut encoder, &texture);
    queue.submit([encoder.finish()]);
    let bytes = mipmaps::bytes(width, height);
    #[cfg(any(test, feature = "profiling"))]
    {
        TOTAL_UPLOADS.fetch_add(1, Ordering::Relaxed);
        TOTAL_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
    Arc::new(GlyphPixels {
        view: texture.create_view(&Default::default()),
        size: [width as f32, height as f32],
        scale: plan.scale(),
        ink,
        bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn capture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    input: &CaptureInput,
    scale: f32,
    placement: capture::Placement,
    swash: &mut text::cosmic_text::SwashCache,
    mipmaps: &mipmaps::Mipmaps,
    cache: &mut cache::Cache<capture::Key, GlyphPixels>,
) -> Arc<GlyphTexture> {
    // Only paragraph, resolution or placement changes rebuild the raster recipe.
    #[cfg(any(test, feature = "profiling"))]
    {
        input.counters.key_builds.fetch_add(1, Ordering::Relaxed);
        TOTAL_KEYS.fetch_add(1, Ordering::Relaxed);
    }
    let plan = capture::Plan::at(&input.paragraph, scale, input.region, placement);
    let (pixels, missed) = if let Some(pixels) = cache.get(&plan.key) {
        #[cfg(any(test, feature = "profiling"))]
        {
            input.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
            TOTAL_HITS.fetch_add(1, Ordering::Relaxed);
        }
        (pixels, false)
    } else {
        #[cfg(any(test, feature = "profiling"))]
        input.counters.uploads.fetch_add(1, Ordering::Relaxed);
        (upload_pixels(device, queue, &plan, swash, mipmaps), true)
    };
    let table = plan.table(&pixels.ink);
    // Each occurrence owns source offsets and line indices; identical pixels can
    // come from entirely different paragraphs, clusters and wrapped rows.
    use wgpu::util::DeviceExt as _;
    let geometry = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cached shaped glyph geometry"),
        contents: bytemuck::bytes_of(&table),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    if missed {
        let retained_bytes = pixels.bytes
            + plan.key.bytes()
            + std::mem::size_of::<GlyphPixels>()
            + pixels.ink.capacity() * std::mem::size_of::<[f32; 4]>();
        cache.insert(plan.key, pixels.clone(), retained_bytes);
    }
    #[cfg(any(test, feature = "profiling"))]
    let bytes = pixels.bytes + std::mem::size_of::<GlyphTable>();
    #[cfg(any(test, feature = "profiling"))]
    {
        TOTAL_BYTES.fetch_add(std::mem::size_of::<GlyphTable>(), Ordering::Relaxed);
        input
            .counters
            .resident_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }
    Arc::new(GlyphTexture {
        _paragraph: input.paragraph.clone(),
        _identity: input.identity.clone(),
        pixels,
        geometry,
        glyph_count: table.info[0],
        origin: plan.origin,
        #[cfg(any(test, feature = "profiling"))]
        bytes,
        #[cfg(any(test, feature = "profiling"))]
        counters: input.counters.clone(),
        used: std::sync::atomic::AtomicBool::new(true),
    })
}
// Renderer command caches can outlive a widget or script generation. A draw
// snapshot retains capture data, never the host's admission lease or pending flag.
#[derive(Debug)]
struct CaptureInput {
    identity: Arc<()>,
    paragraph: text::Paragraph,
    region: Rectangle,
    capture_scale: f32,
    #[cfg(any(test, feature = "profiling"))]
    counters: Arc<Counters>,
}
#[derive(Debug)]
struct Primitive {
    programs: Arc<programs::Programs>,
    program: Arc<wgpu::RenderPipeline>,
    input: CaptureInput,
    decoration: InlineDecoration,
    effect: Arc<ShaderEffect>,
    surface: Rectangle,
    anchor: Rectangle,
    now: iced::time::Instant,
    slot: AtomicUsize,
}
impl shader::Primitive for Primitive {
    type Pipeline = Pipeline;
    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &shader::Viewport,
    ) {
        let scale = viewport.scale_factor();
        let raster_scale = scale * (bounds.width / self.surface.width);
        let capture_scale = raster_scale * self.input.capture_scale;
        let program = self.program.clone();
        let region = self.input.region;
        // `bounds` includes the renderer's layer transform. Recover the same
        // paragraph origin used by native text, including wrapped-fragment offsets.
        let placement = capture::Placement {
            origin: iced::Point::new(
                bounds.x * scale + (self.anchor.x - self.surface.x - region.x) * raster_scale,
                bounds.y * scale + (self.anchor.y - self.surface.y - region.y) * raster_scale,
            ),
            scale: raster_scale,
        };
        let key = CaptureKey {
            paragraph: std::ptr::from_ref(self.input.paragraph.buffer()) as usize,
            identity: Arc::as_ptr(&self.input.identity) as usize,
            region: [
                region.x.to_bits(),
                region.y.to_bits(),
                region.width.to_bits(),
                region.height.to_bits(),
            ],
            scale: capture_scale.to_bits(),
            placement: [
                placement.origin.x.to_bits(),
                placement.origin.y.to_bits(),
                raster_scale.to_bits(),
            ],
        };
        // Input leases bound the number of distinct live fragments before draw.
        let glyphs = pipeline
            .glyphs
            .entry(key)
            .or_insert_with(|| {
                capture(
                    device,
                    queue,
                    &self.input,
                    capture_scale,
                    placement,
                    &mut pipeline.swash,
                    &pipeline.mipmaps,
                    &mut pipeline.pixels,
                )
            })
            .clone();
        glyphs.used.store(true, Ordering::Relaxed);
        let index = pipeline.used;
        pipeline.used += 1;
        if index == pipeline.slots.len() {
            let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("text effect uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = pipeline.group(device, &uniforms, &glyphs, &self.programs);
            let params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("text shader user uniforms"),
                size: 1024,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let params_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("text shader user parameters"),
                layout: &self.programs.params_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                }],
            });
            pipeline.slots.push(Slot {
                program: program.clone(),
                params,
                params_group,
                uniforms,
                group,
                glyphs: glyphs.clone(),
            });
        } else if !Arc::ptr_eq(&pipeline.slots[index].glyphs, &glyphs) {
            let group = pipeline.group(
                device,
                &pipeline.slots[index].uniforms,
                &glyphs,
                &self.programs,
            );
            pipeline.slots[index].group = group;
            pipeline.slots[index].glyphs = glyphs.clone();
        }
        pipeline.slots[index].program = program;
        let elapsed = self
            .decoration
            .elapsed(self.now)
            .unwrap_or_default()
            .as_secs_f64();
        let duration = f64::from(self.decoration.effect.duration_ms) / 1000.0;
        let progress = if duration > 0.0 {
            (elapsed / duration).min(1.0) as f32
        } else {
            0.0
        };
        let seconds = if duration > 0.0 {
            elapsed.min(duration)
        } else {
            elapsed
        };
        let background = crate::prefs::current().palette.background;
        let u = Uniforms {
            origin: [bounds.x * scale, bounds.y * scale],
            resolution: [bounds.width * scale, bounds.height * scale],
            surface: [self.surface.width, self.surface.height],
            texture_size: glyphs.pixels.size,
            scale: glyphs.pixels.scale,
            text_size: [region.width, region.height],
            outset: f32::from(self.decoration.effect.outset),
            time: (seconds % 4096.0) as f32,
            progress,
            seed: (self.decoration.id % 65536) as f32 * 0.173,
            linearize: pipeline.linearize,
            background: [background.r, background.g, background.b, background.a],
            paint_offset: [
                self.surface.x - self.anchor.x,
                self.surface.y - self.anchor.y,
            ],
            capture_offset: glyphs.origin,
            effect_scale: self.effect.scale.get(),
            duration: duration as f32,
            envelope: self.effect.envelope(
                std::time::Duration::from_secs_f64(seconds),
                self.decoration.effect.duration_ms,
            ),
            replaces_text: u32::from(self.effect.replace),
        };
        queue.write_buffer(&pipeline.slots[index].uniforms, 0, bytemuck::bytes_of(&u));
        queue.write_buffer(&pipeline.slots[index].params, 0, &self.effect.uniforms);
        self.slot.store(index, Ordering::Relaxed);
    }
    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(slot) = pipeline.slots.get(self.slot.load(Ordering::Relaxed)) else {
            return true;
        };
        pass.set_pipeline(&slot.program);
        pass.set_bind_group(0, &slot.group, &[]);
        pass.set_bind_group(1, &slot.params_group, &[]);
        let fragments = self.effect.shader.fragments_per_glyph;
        if fragments == 0 {
            pass.draw(0..3, 0..1);
        } else {
            pass.draw(0..6, 0..1 + slot.glyphs.glyph_count * fragments);
        }
        true
    }
}
pub(super) fn ready(
    renderer: &dyn std::any::Any,
    input: &Input,
    effect: &Arc<ShaderEffect>,
) -> bool {
    let Some(programs) = register(renderer) else {
        input.retire();
        return false;
    };
    match programs.ready(&effect.shader) {
        programs::Status::Ready(_) => {
            input.pending.store(false, Ordering::Relaxed);
            true
        }
        status => {
            input.retire();
            input.pending.store(
                matches!(status, programs::Status::Pending),
                Ordering::Relaxed,
            );
            false
        }
    }
}
pub(super) fn draw(
    renderer: &mut dyn std::any::Any,
    input: &Input,
    bounds: Rectangle,
    clip: Rectangle,
    decoration: &InlineDecoration,
    effect: &Arc<ShaderEffect>,
    now: iced::time::Instant,
) -> bool {
    use iced::advanced::Renderer as _;
    let surface = if effect.pane {
        clip
    } else {
        bounds.expand(f32::from(decoration.effect.outset))
    };
    if surface.intersection(&clip).is_none() {
        return false;
    }
    let Some(programs) = register(renderer) else {
        return false;
    };
    let program = match programs.ready(&effect.shader) {
        programs::Status::Ready(program) => program,
        status => {
            input.retire();
            input.pending.store(
                matches!(status, programs::Status::Pending),
                Ordering::Relaxed,
            );
            return false;
        }
    };
    input.pending.store(false, Ordering::Relaxed);
    if !input.admit() {
        return false;
    }
    let primitive = || Primitive {
        programs: programs.clone(),
        program: program.clone(),
        input: CaptureInput {
            identity: input.identity.clone(),
            paragraph: input.paragraph.clone(),
            region: input.region,
            capture_scale: effect.capture_scale.get(),
            #[cfg(any(test, feature = "profiling"))]
            counters: input.counters.clone(),
        },
        decoration: decoration.clone(),
        effect: effect.clone(),
        surface,
        anchor: bounds,
        now,
        slot: AtomicUsize::new(usize::MAX),
    };
    if let Some(iced::Renderer::Primary(r)) = renderer.downcast_mut::<iced::Renderer>() {
        r.with_layer(clip, |r| r.draw_primitive(surface, primitive()));
        return true;
    }
    if let Some(r) = renderer.downcast_mut::<iced_wgpu::Renderer>() {
        r.with_layer(clip, |r| r.draw_primitive(surface, primitive()));
        return true;
    }
    false
}

pub(super) fn register(renderer: &dyn std::any::Any) -> Option<Arc<programs::Programs>> {
    let gpu = renderer.downcast_ref::<iced_wgpu::Renderer>().or_else(|| {
        match renderer.downcast_ref::<iced::Renderer>() {
            Some(iced::Renderer::Primary(gpu)) => Some(gpu),
            _ => None,
        }
    });
    gpu.map(|gpu| {
        let (device, format, storage) = gpu.shader_context();
        programs::Programs::for_renderer(device, format, storage)
    })
}
pub(super) fn prewarm(
    shader: &Arc<smudgy_session_model::text_shader::Shader>,
) -> Result<(), String> {
    programs::prewarm(shader)
}

#[cfg(test)]
pub(super) use programs::assert_prewarmed;

#[cfg(test)]
mod geometry_tests {
    use super::*;
    use iced::advanced::text::{self as api, Paragraph as _};
    use iced::{Font, Pixels, Size};
    #[test]
    fn geometry_preserves_clusters_spaces_and_bounds_long_inputs() {
        for content in ["office café e\u{301} שלום 東京", "a ", &"a".repeat(300)] {
            let paragraph = text::Paragraph::with_text(api::Text {
                content,
                bounds: Size::new(10000.0, 60.0),
                size: Pixels(24.0),
                line_height: api::LineHeight::default(),
                font: Font::MONOSPACE,
                align_x: api::Alignment::Left,
                align_y: iced::alignment::Vertical::Top,
                shaping: api::Shaping::Advanced,
                wrapping: api::Wrapping::None,
            });
            let (_, _, _, _, table) = rasterize(
                &paragraph,
                1.0,
                Rectangle::with_size(paragraph.min_bounds()),
                &mut text::cosmic_text::SwashCache::new(),
            );
            assert!(table.info[0] > 0 && table.info[0] <= 256);
            assert_eq!(table.info[1], u32::from(content.len() == 300));
            for glyph in &table.items[..table.info[0] as usize] {
                assert!(content.is_char_boundary(glyph.cluster[0] as usize));
                assert!(content.is_char_boundary(glyph.cluster[1] as usize));
                let baseline = f32::from_bits(glyph.cluster[3]);
                assert!(baseline.is_finite());
                assert!(baseline >= glyph.advance[1]);
                assert!(baseline <= glyph.advance[1] + glyph.advance[3]);
                assert!(
                    glyph
                        .advance
                        .iter()
                        .chain(glyph.ink.iter())
                        .all(|v| v.is_finite())
                );
                if glyph.ink[2] > 0.0 {
                    assert!(glyph.ink[3] > 0.0);
                }
                if &content[glyph.cluster[0] as usize..glyph.cluster[1] as usize] == " " {
                    assert_eq!(glyph.ink, [0.0; 4]);
                    assert!(glyph.advance[2] > 0.0);
                }
            }
            if content == "a " {
                assert_eq!(table.info[0], 2);
            }
        }
        assert!(std::mem::size_of::<GlyphTable>() <= 16384);
    }
}
