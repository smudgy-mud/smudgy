//! GPU filtering performed only when a cached texture is created.
use iced::wgpu;

#[derive(Debug)]
pub(super) struct Mipmaps(wgpu::ComputePipeline);

pub(super) fn levels(width: u32, height: u32) -> u32 {
    (width.max(height).ilog2() + 1).min(9)
}

pub(super) fn bytes(width: u32, height: u32) -> usize {
    (0..levels(width, height))
        .map(|level| ((width >> level).max(1) * (height >> level).max(1) * 4) as usize)
        .sum()
}

impl Mipmaps {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filter cached text effect textures"),
            source: wgpu::ShaderSource::Wgsl(
                r"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var output: texture_storage_2d<rgba8unorm, write>;
@compute @workgroup_size(8, 8) fn main(@builtin(global_invocation_id) p: vec3u) {
    if any(p.xy >= textureDimensions(output)) { return; }
    let q = vec2i(p.xy * 2u);
    let hi = vec2i(textureDimensions(source)) - vec2i(1);
    let value = (textureLoad(source, min(q, hi), 0)
        + textureLoad(source, min(q + vec2i(1,0), hi), 0)
        + textureLoad(source, min(q + vec2i(0,1), hi), 0)
        + textureLoad(source, min(q + vec2i(1,1), hi), 0)) * 0.25;
    textureStore(output, p.xy, value);
}"
                .into(),
            ),
        });
        Self(
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("filter cached text effect textures"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            }),
        )
    }

    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
    ) {
        for level in 1..texture.mip_level_count() {
            let view = |level| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            };
            let source = view(level - 1);
            let output = view(level);
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("cached text effect texture mip"),
                layout: &self.0.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&output),
                    },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("filter cached text effect texture mip"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.0);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(
                (texture.width() >> level).max(1).div_ceil(8),
                (texture.height() >> level).max(1).div_ceil(8),
                1,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filtered_payload_accounts_for_thin_and_non_power_of_two_captures() {
        assert_eq!(bytes(1, 1), 4);
        assert_eq!(bytes(8, 1), (8 + 4 + 2 + 1) * 4);
        assert_eq!(bytes(9, 7), (9 * 7 + 4 * 3 + 2 + 1) * 4);
        assert_eq!(bytes(256, 256), 349_524);
        assert_eq!(levels(2048, 16), 9);
    }
}
