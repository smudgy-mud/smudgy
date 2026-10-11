//! Fixed, tiled noise shared by every text shader in a renderer. No frame uploads.
use iced::wgpu;

#[derive(Debug)]
pub(super) struct Noise {
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

impl Noise {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mipmaps: &super::mipmaps::Mipmaps,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shared text effect noise"),
            size: wgpu::Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: 1,
            },
            mip_level_count: 9,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let generate = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("generate tiled text effect noise"),
            source: wgpu::ShaderSource::Wgsl(
                r"
@group(0) @binding(0) var output: texture_storage_2d<rgba8unorm, write>;
fn hash(p: vec2u) -> f32 {
    var h = p.x * 1597334677u ^ p.y * 3812015801u ^ 1013904223u;
    h = (h ^ (h >> 16u)) * 2246822519u;
    h = (h ^ (h >> 13u)) * 3266489917u;
    return f32(h ^ (h >> 16u)) / 4294967295.0;
}
@compute @workgroup_size(8, 8) fn main(@builtin(global_invocation_id) p: vec3u) {
    if any(p.xy >= textureDimensions(output)) { return; }
    // Independent channels; the host helper hashes Z layers into the red atlas.
    textureStore(output, p.xy, vec4f(hash(p.xy), hash(p.xy ^ vec2u(193u, 97u)), 0.0, 1.0));
}"
                .into(),
            ),
        });
        let generate = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("initialize text effect noise"),
            layout: None,
            module: &generate,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("initialize shared text effect noise"),
        });
        let output = texture.create_view(&wgpu::TextureViewDescriptor {
            mip_level_count: Some(1),
            ..Default::default()
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("text effect noise base level"),
            layout: &generate.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&output),
            }],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("generate text effect noise"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&generate);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(32, 32, 1);
        }
        mipmaps.encode(device, &mut encoder, &texture);
        queue.submit([encoder.finish()]);
        Self {
            view: texture.create_view(&Default::default()),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("tiled filtered text effect noise"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        }
    }
}
