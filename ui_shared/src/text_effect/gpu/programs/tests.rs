use super::*;
use iced::{Font, Pixels, advanced::renderer::Headless};
use std::sync::{Barrier, atomic::Ordering};
use std::time::{Duration, Instant};

fn renderer() -> iced_wgpu::Renderer {
    iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap()
}
fn copy(label: &str) -> Arc<Shader> {
    Shader::compile(
        label,
        "fn effect(p: vec2f) -> vec4f { return sampleText(p); }",
    )
    .unwrap()
}
fn programs(renderer: &iced_wgpu::Renderer) -> Arc<Programs> {
    let (device, format, storage) = renderer.shader_context();
    Programs::for_renderer(device, format, storage)
}
fn settle(programs: &Arc<Programs>, shader: &Arc<Shader>) -> Status {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = programs.ready(shader);
        if !matches!(status, Status::Pending) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "compiler did not resolve its attempt"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn renderer_teardown_releases_programs_without_another_registry_call() {
    let renderer = renderer();
    let programs = programs(&renderer);
    assert!(Arc::ptr_eq(&programs, &self::programs(&renderer)));
    let owner = Arc::downgrade(&programs);
    drop(programs);
    assert!(owner.upgrade().is_some()); // Engine owns the cache before any draw.
    drop(renderer);
    assert!(owner.upgrade().is_none()); // Global prewarm registry cannot retain it.
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn cold_draw_and_concurrent_imports_compile_one_pipeline() {
    let renderer = renderer();
    let programs = programs(&renderer);
    let shader = copy("concurrent");
    assert!(matches!(programs.ready(&shader), Status::Pending));
    let start = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (programs, shader, start) = (programs.clone(), shader.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                programs.compile(&shader).unwrap()
            })
        })
        .collect();
    let pipelines: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(
        pipelines
            .iter()
            .all(|pipeline| Arc::ptr_eq(pipeline, &pipelines[0]))
    );
    assert_eq!(programs.compilations.load(Ordering::Relaxed), 1);
    assert!(matches!(settle(&programs, &shader), Status::Ready(_)));
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn playing_pipeline_survives_import_pressure_and_dead_sources_are_reclaimed() {
    let renderer = renderer();
    let programs = programs(&renderer);
    let active = copy("playing");
    let pipeline = programs.compile(&active).unwrap();
    let mut shaders = vec![active.clone()];
    for index in 0..IDLE_PROGRAMS + 5 {
        let shader = copy(&format!("idle-{index}"));
        programs.compile(&shader).unwrap();
        shaders.push(shader);
    }
    programs.cache.lock().unwrap().trim();
    assert_eq!(
        programs.cache.lock().unwrap().entries.len(),
        IDLE_PROGRAMS + 1
    );
    let Status::Ready(still_playing) = programs.ready(&active) else {
        panic!("evicted playing shader")
    };
    assert!(Arc::ptr_eq(&pipeline, &still_playing));
    assert_eq!(
        programs.compilations.load(Ordering::Relaxed),
        IDLE_PROGRAMS + 6
    );
    assert!(
        !programs
            .cache
            .lock()
            .unwrap()
            .entries
            .contains_key(&shaders[1].id)
    );
    // Touch an older surviving shader, then add pressure. Recency, rather than
    // import ID, determines which idle shader is removed.
    assert!(matches!(programs.ready(&shaders[6]), Status::Ready(_)));
    let newest = copy("newest");
    programs.compile(&newest).unwrap();
    programs.cache.lock().unwrap().trim();
    assert!(
        programs
            .cache
            .lock()
            .unwrap()
            .entries
            .contains_key(&shaders[6].id)
    );
    assert!(
        !programs
            .cache
            .lock()
            .unwrap()
            .entries
            .contains_key(&shaders[7].id)
    );
    drop(newest);
    drop((shaders, active, pipeline, still_playing));
    programs.cache.lock().unwrap().trim();
    assert!(programs.cache.lock().unwrap().entries.is_empty());
}

#[test]
fn captured_errors_naming_another_resource_are_not_the_shaders() {
    let tag = resource_tag(&copy("glow"));
    assert!(own_error(
        &format!(
            "Validation Error\n\nCaused by:\n  In Device::create_shader_module, label = '{tag}'\n"
        ),
        &tag
    ));
    assert!(own_error("Out of Memory", &tag));
    assert!(!own_error(
        "Validation Error\n\nCaused by:\n  In Device::create_buffer, label = 'iced_wgpu quad vertices'\n",
        &tag
    ));
    // A lookalike script label is a different shader with a different id.
    assert!(!own_error(
        "In Device::create_render_pipeline, label = 'smudgy text shader glow #0'",
        &tag
    ));
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn rejected_gpu_pipeline_resolves_once_and_does_not_wedge_the_worker() {
    let renderer = renderer();
    let programs = programs(&renderer);
    let mut shader = copy("driver-rejected");
    // Deliberately corrupt already-validated source to exercise the actual wgpu
    // error-scope boundary, independently of Naga's earlier import validation.
    Arc::get_mut(&mut shader).unwrap().source = "invalid WGSL".into();
    assert!(matches!(settle(&programs, &shader), Status::Failed));
    for _ in 0..10 {
        assert!(matches!(programs.ready(&shader), Status::Failed));
        assert!(
            programs
                .compile(&shader)
                .unwrap_err()
                .contains("driver-rejected")
        );
    }
    assert_eq!(programs.compilations.load(Ordering::Relaxed), 1);
    let valid = copy("after-failure");
    assert!(matches!(settle(&programs, &valid), Status::Ready(_)));
    assert_eq!(programs.compilations.load(Ordering::Relaxed), 2);
    let deadline = Instant::now() + Duration::from_secs(10);
    while programs.pending.lock().unwrap().working {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(programs.pending.lock().unwrap().ids.is_empty());
    // If the OS cannot start another compiler thread, cold draws must stop
    // retrying; a subsequent successful importing-thread compile still works.
    programs.pending.lock().unwrap().unavailable = true;
    let later = copy("import-without-worker");
    assert!(matches!(programs.ready(&later), Status::Failed));
    assert!(programs.pending.lock().unwrap().queue.is_empty());
    programs.compile(&later).unwrap();
    assert!(matches!(programs.ready(&later), Status::Ready(_)));
}
