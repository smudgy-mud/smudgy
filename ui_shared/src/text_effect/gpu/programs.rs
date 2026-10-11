//! Engine-owned programs. Imports and the cold-draw worker share one compilation
//! result; visible draw commands pin pipelines while idle programs obey an LRU.
use iced::wgpu;
use iced_wgpu::primitive;
use rustc_hash::{FxHashMap, FxHashSet};
use smudgy_session_model::text_shader::Shader;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock, RwLock, Weak},
};

const IDLE_PROGRAMS: usize = 64;
const PENDING_PROGRAMS: usize = 64;
// wgpu 27 error scopes form a device-wide stack. Serialize our short creation
// sections, including separate iced engines backed by the same device.
static COMPILER: Mutex<()> = Mutex::new(());
static ACTIVE: OnceLock<Mutex<Vec<Weak<Programs>>>> = OnceLock::new();

#[derive(Debug)]
struct Program {
    owner: Weak<Shader>,
    result: OnceLock<Result<Arc<wgpu::RenderPipeline>, String>>,
}
#[derive(Debug)]
struct Entry {
    program: Arc<Program>,
    touched: u64,
}
#[derive(Debug, Default)]
struct Cache {
    entries: FxHashMap<u64, Entry>,
    clock: u64,
}
impl Cache {
    fn get(&mut self, shader: &Arc<Shader>) -> Arc<Program> {
        self.clock += 1;
        let entry = self.entries.entry(shader.id).or_insert_with(|| Entry {
            program: Arc::new(Program {
                owner: Arc::downgrade(shader),
                result: OnceLock::new(),
            }),
            touched: self.clock,
        });
        entry.touched = self.clock;
        entry.program.clone()
    }
    fn trim(&mut self) {
        self.entries
            .retain(|_, entry| entry.program.owner.strong_count() > 0);
        let mut idle = Vec::new();
        for (&id, entry) in &mut self.entries {
            let Some(Ok(pipeline)) = entry.program.result.get() else {
                // Pending work is bounded separately. Keep a failed attempt for
                // the shader's lifetime so redraws cannot retry it indefinitely.
                continue;
            };
            if Arc::strong_count(&entry.program) == 1 && Arc::strong_count(pipeline) == 1 {
                idle.push((entry.touched, id));
            } else {
                self.clock += 1;
                entry.touched = self.clock;
            }
        }
        if idle.len() > IDLE_PROGRAMS {
            idle.sort_unstable();
            for (_, id) in idle.iter().take(idle.len() - IDLE_PROGRAMS) {
                self.entries.remove(id);
            }
        }
    }
}
#[derive(Debug, Default)]
struct Pending {
    ids: FxHashSet<u64>,
    queue: VecDeque<(u64, Arc<Program>)>,
    working: bool,
    unavailable: bool,
}
#[derive(Debug)]
pub(super) enum Status {
    Ready(Arc<wgpu::RenderPipeline>),
    Pending,
    Failed,
}
#[derive(Debug)]
pub(in crate::text_effect) struct Programs {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    pub layout: wgpu::BindGroupLayout,
    pub params_layout: wgpu::BindGroupLayout,
    cache: Mutex<Cache>,
    pending: Mutex<Pending>,
    #[cfg(test)]
    compilations: std::sync::atomic::AtomicUsize,
}
// iced already owns a type-indexed store shared by clones of an engine. No global
// strong reference or additional renderer identity is needed to keep this alive.
struct StoredPrograms(Arc<Programs>);
impl primitive::Pipeline for StoredPrograms {
    fn new(device: &wgpu::Device, _: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self(Programs::new(device, format))
    }
    fn trim(&mut self) {
        self.0.cache.lock().unwrap().trim();
    }
}
impl Programs {
    pub fn for_renderer(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        storage: &RwLock<primitive::Storage>,
    ) -> Arc<Self> {
        let existing = storage
            .read()
            .unwrap()
            .get::<StoredPrograms>()
            .and_then(|stored| stored.downcast_ref::<StoredPrograms>())
            .map(|stored| stored.0.clone());
        if let Some(programs) = existing {
            return programs;
        }
        let mut storage = storage.write().unwrap();
        if let Some(stored) = storage.get::<StoredPrograms>() {
            return stored.downcast_ref::<StoredPrograms>().unwrap().0.clone();
        }
        let programs = Self::new(device, format);
        storage.store::<StoredPrograms, _>(StoredPrograms(programs.clone()));
        let mut active = ACTIVE.get_or_init(Mutex::default).lock().unwrap();
        active.retain(|programs| programs.strong_count() > 0);
        active.push(Arc::downgrade(&programs));
        programs
    }
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Arc<Self> {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text effect layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text shader parameters"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        Arc::new(Self {
            device: device.clone(),
            format,
            layout,
            params_layout,
            cache: Mutex::default(),
            pending: Mutex::default(),
            #[cfg(test)]
            compilations: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    /// A cold draw keeps native text visible. A failed pipeline stops retrying.
    pub(super) fn ready(self: &Arc<Self>, shader: &Arc<Shader>) -> Status {
        let program = self.cache.lock().unwrap().get(shader);
        if let Some(result) = program.result.get() {
            return match result {
                Ok(pipeline) => Status::Ready(pipeline.clone()),
                Err(_) => Status::Failed,
            };
        }
        let mut pending = self.pending.lock().unwrap();
        if pending.unavailable {
            return Status::Failed;
        }
        if pending.ids.len() < PENDING_PROGRAMS && pending.ids.insert(shader.id) {
            pending.queue.push_back((shader.id, program));
            if !pending.working {
                pending.working = true;
                let programs = self.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("text-shader-compiler".into())
                    .spawn(move || programs.work())
                {
                    let message = format!("cannot start text shader compiler: {error}");
                    log::error!("{message}");
                    // Do not wait on a OnceLock being initialized by a concurrent
                    // import: even failure to spawn must keep draws nonblocking.
                    pending.queue.clear();
                    pending.ids.clear();
                    pending.working = false;
                    pending.unavailable = true;
                    return Status::Failed;
                }
            }
        }
        Status::Pending
    }
    fn work(&self) {
        loop {
            let next = {
                let mut pending = self.pending.lock().unwrap();
                let next = pending.queue.pop_front();
                if next.is_none() {
                    pending.working = false;
                }
                next
            };
            let Some((id, program)) = next else { break };
            if let Some(shader) = program.owner.upgrade() {
                let _ = self.compile_program(&program, &shader);
            }
            self.pending.lock().unwrap().ids.remove(&id);
            self.cache.lock().unwrap().trim();
        }
    }
    pub fn compile(&self, shader: &Arc<Shader>) -> Result<Arc<wgpu::RenderPipeline>, String> {
        let program = self.cache.lock().unwrap().get(shader);
        let result = self.compile_program(&program, shader);
        drop(program);
        self.cache.lock().unwrap().trim();
        result
    }
    fn compile_program(
        &self,
        program: &Program,
        shader: &Shader,
    ) -> Result<Arc<wgpu::RenderPipeline>, String> {
        program
            .result
            .get_or_init(|| {
                // Waiting imports/workers never hold the cache mutex. Draws only
                // inspect OnceLock and therefore never wait for driver compilation.
                let result = self.checked_compile(shader);
                if let Err(error) = &result {
                    log::error!("{error}");
                }
                result
            })
            .clone()
    }
    fn checked_compile(&self, shader: &Shader) -> Result<Arc<wgpu::RenderPipeline>, String> {
        let _compiler = COMPILER.lock().unwrap();
        #[cfg(test)]
        self.compilations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for filter in [
            wgpu::ErrorFilter::OutOfMemory,
            wgpu::ErrorFilter::Internal,
            wgpu::ErrorFilter::Validation,
        ] {
            self.device.push_error_scope(filter);
        }
        // A backend panic must also resolve the attempt and release the worker's
        // pending ID. Pop every scope before reporting either kind of failure.
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.create_pipeline(shader)
        }));
        // The scopes are device-wide, so they also catch whatever the UI thread
        // raised meanwhile. wgpu names the labelled resource an error came from;
        // one that names a resource of ours, or none at all, is this shader's.
        // Any other would have reached the uncaptured-error handler and is
        // reported the way it would have been, without condemning the shader.
        let tag = resource_tag(shader);
        let (mine, foreign): (Vec<_>, Vec<_>) = (0..3)
            .filter_map(|_| iced::futures::executor::block_on(self.device.pop_error_scope()))
            .map(|error| error.to_string())
            .partition(|message| own_error(message, &tag));
        for message in foreign {
            log::error!("wgpu error during text shader compilation: {message}");
        }
        if !mine.is_empty() {
            return Err(format!(
                "{}: GPU text shader compilation failed: {}",
                shader.label,
                mine.join("; ")
            ));
        }
        attempt.map(Arc::new).map_err(|panic| {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("unknown backend panic");
            format!(
                "{}: GPU text shader compilation panicked: {message}",
                shader.label
            )
        })
    }
    fn create_pipeline(&self, shader: &Shader) -> wgpu::RenderPipeline {
        let label = resource_tag(shader);
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&label),
                source: wgpu::ShaderSource::Wgsl(shader.source.as_ref().into()),
            });
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&label),
                bind_group_layouts: &[&self.layout, &self.params_layout],
                push_constant_ranges: &[],
            });
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("smudgy_vs"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("smudgy_fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
                cache: None,
            })
    }
}
/// The label every wgpu resource of one shader carries: the script's name for it,
/// made unique with its id so error attribution cannot mistake a lookalike.
fn resource_tag(shader: &Shader) -> String {
    format!("smudgy text shader {} #{}", shader.label, shader.id)
}
/// Whether a captured error belongs to the shader whose resources carry `tag`.
/// wgpu names the labelled resource an error came from; an error naming some
/// other resource is another thread's, while one naming none is assumed ours.
fn own_error(message: &str, tag: &str) -> bool {
    !message.contains("label = '") || message.contains(tag)
}
/// Runs on the importing script's thread, when an engine has registered its GPU.
pub(super) fn prewarm(shader: &Arc<Shader>) -> Result<(), String> {
    let programs: Vec<_> = {
        let mut active = ACTIVE.get_or_init(Mutex::default).lock().unwrap();
        active.retain(|programs| programs.strong_count() > 0);
        active.iter().filter_map(Weak::upgrade).collect()
    };
    let mut failure = None;
    for programs in programs {
        if let Err(error) = programs.compile(shader) {
            failure.get_or_insert(error);
        }
    }
    failure.map_or(Ok(()), Err)
}
#[cfg(test)]
pub(in crate::text_effect) fn assert_prewarmed(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    storage: &RwLock<primitive::Storage>,
    shader: &Arc<Shader>,
) {
    let programs = Programs::for_renderer(device, format, storage);
    let Status::Ready(before) = programs.ready(shader) else {
        panic!("not prewarmed")
    };
    assert!(Arc::ptr_eq(&before, &programs.compile(shader).unwrap()));
}

#[cfg(test)]
mod tests;
