use super::*;
use iced::{
    Font, Pixels, Size,
    advanced::{
        Renderer as _,
        renderer::Headless,
        text::{self as api, Paragraph as _},
    },
};
use smudgy_session_model::{
    inline_content::{InlineOwner, TextEffect},
    text_shader::{Shader, ShaderEffect},
};
use std::time::Duration;

#[test]
#[ignore = "requires hardware GPU; run serialized with the inline effect GPU tests"]
fn prewarm_cache_dpi_admission_and_retirement() {
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    register_renderer(&renderer);
    let shader = Shader::compile(
        "copy.wgsl",
        "fn effect(p: vec2f) -> vec4f { return sampleText(p); }",
    )
    .unwrap();
    prewarm(&shader).unwrap();
    let (device, format, storage) = renderer.shader_context();
    gpu::assert_prewarmed(device, format, storage, &shader);
    let paragraph = text::Paragraph::with_text(api::Text {
        content: "cached glyph input",
        bounds: Size::new(300.0, 40.0),
        size: Pixels(24.0),
        line_height: api::LineHeight::default(),
        font: Font::MONOSPACE,
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    let region = Rectangle::with_size(paragraph.min_bounds());
    let input = Input::from_paragraph(&paragraph, region).unwrap();
    let mut effect = InlineDecoration::new(
        0..18,
        TextEffect {
            shader: Arc::new(ShaderEffect {
                pane: false,
                scale: Default::default(),
                capture_scale: Default::default(),
                fade_in_ms: 0,
                fade_out_ms: 0,
                uniforms: shader.uniforms(&Default::default()).unwrap(),
                shader,
                replace: true,
                hold: false,
                animated: true,
            }),

            duration_ms: 0,
            outset: 16,
        },
        InlineOwner::default(),
    );
    let viewport = Rectangle::with_size(Size::new(400.0, 160.0));
    for (scale, quality, expected_uploads) in [
        (1.0, 1.0, 1),
        (1.0, 1.0, 1),
        (2.0, 1.0, 2),
        (2.0, 1.0, 2),
        (1.0, 8.0, 3),
        (1.0, 8.0, 3),
        // The capture budget is identical, but the native grid and baseline
        // rounding changed with DPI, so these raster recipes differ.
        (2.0, 8.0, 4),
        (2.0, 8.0, 4),
    ] {
        let settings = &mut effect.effect.shader;
        Arc::make_mut(settings).capture_scale =
            smudgy_session_model::text_shader::CaptureScale::new(quality).unwrap();
        renderer.reset(viewport);
        assert!(draw(
            &mut renderer,
            &input,
            region,
            viewport,
            &effect,
            iced::time::Instant::now()
        ));
        Headless::screenshot(
            &mut renderer,
            Size::new(400, 160),
            scale,
            iced::Color::BLACK,
        );
        assert_eq!(input.stats().uploads, expected_uploads);
        assert!(
            input.stats().resident_bytes > 0
                && input.stats().resident_bytes <= 2 * (1024 * 1024 + 12_304)
        );
    }
    assert!(!draw(
        &mut renderer,
        &input,
        region,
        Rectangle::default(),
        &effect,
        iced::time::Instant::now()
    ));
    assert!(
        !input.admitted(),
        "zero-area hidden panes release their slots"
    );
    renderer.reset(viewport);
    // A full budget denies an effect before native foreground is hidden. A
    // previously denied input can retry as soon as a retired input releases it.
    let mut held = Vec::new();
    while INPUTS.load(Ordering::Relaxed) < 64 {
        let next = Input::from_paragraph(&paragraph, region).unwrap();
        assert!(next.admit());
        held.push(next);
    }
    let denied = Input::from_paragraph(&paragraph, region).unwrap();
    assert!(!denied.can_admit(1));
    assert!(!denied.admit());
    assert!(!denied.admitted());
    held.pop().unwrap().retire();
    assert!(denied.can_admit(1));
    assert!(!denied.can_admit(2));
    assert!(denied.admit());
    denied.retire();
    drop(held);
    input.retire();
    for _ in 0..2 {
        renderer.reset(viewport);
        Headless::screenshot(&mut renderer, Size::new(400, 160), 1.0, iced::Color::BLACK);
    }
    assert_eq!(input.stats().resident_bytes, 0);
    assert_eq!(INPUTS.load(Ordering::Relaxed), 0);
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn cold_static_shader_compiles_without_admitting_or_hiding_glyphs() {
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    let shader = Shader::compile(
        "cold",
        "fn effect(p: vec2f) -> vec4f { return sampleText(p); }",
    )
    .unwrap();
    let paragraph = text::Paragraph::with_text(api::Text {
        content: "cold static text",
        bounds: Size::new(300.0, 40.0),
        size: Pixels(24.0),
        line_height: api::LineHeight::default(),
        font: Font::MONOSPACE,
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    let region = Rectangle::with_size(paragraph.min_bounds());
    let input = Input::from_paragraph(&paragraph, region).unwrap();
    let effect = InlineDecoration::new(
        0..16,
        TextEffect {
            shader: Arc::new(ShaderEffect {
                uniforms: shader.uniforms(&Default::default()).unwrap(),
                shader,
                pane: false,
                scale: Default::default(),
                capture_scale: Default::default(),
                fade_in_ms: 0,
                fade_out_ms: 0,
                replace: true,
                hold: false,
                animated: false,
            }),

            duration_ms: 0,
            outset: 8,
        },
        InlineOwner::default(),
    );
    let viewport = Rectangle::with_size(Size::new(400.0, 160.0));
    renderer.reset(viewport);
    assert!(!draw(
        &mut renderer,
        &input,
        region,
        viewport,
        &effect,
        iced::time::Instant::now()
    ));
    assert!(input.pending());
    assert!(!input.admitted());
    assert_eq!(input.stats().uploads, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !draw(
        &mut renderer,
        &input,
        region,
        viewport,
        &effect,
        iced::time::Instant::now(),
    ) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!input.pending());
    assert!(input.admitted());
    Headless::screenshot(&mut renderer, Size::new(400, 160), 1.0, iced::Color::BLACK);
    assert_eq!(input.stats().uploads, 1);
    input.retire();
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn renderer_commands_do_not_retain_host_admission_leases() {
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    register_renderer(&renderer);
    let shader = Shader::compile(
        "lease-copy",
        "fn effect(p: vec2f) -> vec4f {return sampleText(p);}",
    )
    .unwrap();
    prewarm(&shader).unwrap();
    let paragraph = text::Paragraph::with_text(api::Text {
        content: "discard this host",
        bounds: Size::new(300.0, 40.0),
        size: Pixels(24.0),
        line_height: api::LineHeight::default(),
        font: Font::MONOSPACE,
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    let region = Rectangle::with_size(paragraph.min_bounds());
    let input = Input::from_paragraph(&paragraph, region).unwrap();
    let counters = input.counters();
    let effect = InlineDecoration::new(
        0..17,
        TextEffect {
            shader: Arc::new(ShaderEffect {
                uniforms: shader.uniforms(&Default::default()).unwrap(),
                shader,
                pane: false,
                scale: Default::default(),
                capture_scale: Default::default(),
                fade_in_ms: 0,
                fade_out_ms: 0,
                replace: false,
                hold: false,
                animated: true,
            }),

            duration_ms: 0,
            outset: 8,
        },
        InlineOwner::default(),
    );
    let viewport = Rectangle::with_size(Size::new(400.0, 160.0));
    renderer.reset(viewport);
    let before = INPUTS.load(Ordering::Relaxed);
    assert!(draw(
        &mut renderer,
        &input,
        region,
        viewport,
        &effect,
        iced::time::Instant::now()
    ));
    assert_eq!(INPUTS.load(Ordering::Relaxed), before + 1);
    drop(input);
    assert_eq!(
        INPUTS.load(Ordering::Relaxed),
        before,
        "queued renderer snapshots must not pin host leases"
    );
    Headless::screenshot(&mut renderer, Size::new(400, 160), 1.0, iced::Color::BLACK);
    assert_eq!(
        counters.stats().uploads,
        1,
        "queued capture data remains valid"
    );
    for _ in 0..2 {
        renderer.reset(viewport);
        Headless::screenshot(&mut renderer, Size::new(400, 160), 1.0, iced::Color::BLACK);
    }
    assert_eq!(counters.stats().resident_bytes, 0);
}
