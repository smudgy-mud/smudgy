use super::*;
use iced::{
    Font, Pixels, Point, Size, Transformation, Vector,
    advanced::{
        Renderer as _,
        renderer::Headless,
        text::{self as api, Paragraph as _, Renderer as _},
    },
};
use smudgy_session_model::{
    inline_content::{InlineOwner, TextEffect},
    text_shader::{CaptureScale, Shader, ShaderEffect},
};

pub(super) fn best_translation(native: &[u8], frame: &[u8]) -> (i32, i32) {
    let mut best = (u64::MAX, 0, 0);
    for dy in -1_i32..=1 {
        for dx in -1_i32..=1 {
            let mut error = 0_u64;
            for y in 2..238 {
                for x in 2..638 {
                    let a = i32::from(native[(y * 640 + x) as usize * 4]);
                    let b = i32::from(frame[((y + dy) * 640 + x + dx) as usize * 4]);
                    error += u64::from((a - b).unsigned_abs().pow(2));
                }
            }
            best = best.min((error, dx, dy));
        }
    }
    (best.1, best.2)
}

pub(super) fn renderer() -> iced_wgpu::Renderer {
    let mut fonts = text::font_system().write().unwrap();
    fonts.load_font(
        include_bytes!("../../../assets/fonts/GeistMono[wght].ttf")
            .as_slice()
            .into(),
    );
    fonts.load_font(
        include_bytes!("../../../assets/fonts/GeistMono-Italic[wght].ttf")
            .as_slice()
            .into(),
    );
    drop(fonts);
    let renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    register_renderer(&renderer);
    renderer
}

pub(super) fn decoration(
    shader: &Arc<Shader>,
    quality: f32,
    pane: bool,
    params: serde_json::Value,
) -> InlineDecoration {
    InlineDecoration::new(
        0..100,
        TextEffect {
            shader: Arc::new(ShaderEffect {
                shader: shader.clone(),
                uniforms: shader.uniforms(params.as_object().unwrap()).unwrap(),
                pane,
                scale: Default::default(),
                capture_scale: CaptureScale::new(quality).unwrap(),
                fade_in_ms: 0,
                fade_out_ms: 0,
                replace: true,
                hold: true,
                animated: true,
            }),

            duration_ms: 2400,
            outset: 4,
        },
        InlineOwner::default(),
    )
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn native_copy_matches_fractional_dpi_fragments_and_translated_widgets() {
    let mut renderer = renderer();
    let shader = Shader::compile(
        "aligned-copy.wgsl",
        "fn effect(p: vec2f) -> vec4f { return sampleText(p); }",
    )
    .unwrap();
    prewarm(&shader).unwrap();
    let screen = Rectangle::with_size(Size::new(640.0, 240.0));
    for font in [
        Font::with_name("Geist Mono"),
        Font {
            style: iced::font::Style::Italic,
            ..Font::with_name("Geist Mono")
        },
        Font::MONOSPACE,
    ] {
        for prefix in ["", "prefix ", "\n   "] {
            let spans = [
                api::Span::<()>::new(prefix).color(iced::Color::TRANSPARENT),
                api::Span::new("Hollow crystal").color(iced::Color::WHITE),
            ];
            let paragraph = text::Paragraph::with_spans(api::Text {
                content: &spans[..],
                bounds: Size::new(400.0, 80.0),
                size: Pixels(16.0),
                line_height: api::LineHeight::default(),
                font,
                align_x: api::Alignment::Left,
                align_y: iced::alignment::Vertical::Top,
                shaping: api::Shaping::Advanced,
                wrapping: api::Wrapping::None,
            });
            let region = paragraph.span_bounds(1)[0];
            let input = Input::from_paragraph(&paragraph, region).unwrap();
            for dpi in [1.0, 1.25, 2.0] {
                for at in [
                    Point::new(40.35, 40.65),
                    Point::new(1.35 - region.x, 0.35 - region.y),
                ] {
                    for transform in [
                        Transformation::IDENTITY,
                        Transformation::translate(0.375, 0.625),
                        Transformation::translate(0.375, 0.625) * Transformation::scale(1.5),
                    ] {
                        renderer.reset(screen);
                        renderer.with_transformation(transform, |r| {
                            r.fill_paragraph(&paragraph, at, iced::Color::WHITE, screen)
                        });
                        let native = Headless::screenshot(
                            &mut renderer,
                            Size::new(640, 240),
                            dpi,
                            iced::Color::BLACK,
                        );
                        for pane in [false, true] {
                            let effect = decoration(&shader, 1.0, pane, serde_json::json!({}));
                            renderer.reset(screen);
                            renderer.with_transformation(transform, |r| {
                                assert!(draw(
                                    r,
                                    &input,
                                    region + Vector::new(at.x, at.y),
                                    screen,
                                    &effect,
                                    effect.started
                                ))
                            });
                            let copied = Headless::screenshot(
                                &mut renderer,
                                Size::new(640, 240),
                                dpi,
                                iced::Color::BLACK,
                            );
                            let max_error = native
                                .iter()
                                .zip(&copied)
                                .map(|(a, b)| a.abs_diff(*b))
                                .max()
                                .unwrap();
                            if transform.scale_factor() == 1.0 {
                                assert_eq!(
                                    max_error, 0,
                                    "font={font:?} prefix={prefix:?} dpi={dpi} at={at:?} transform={transform:?} pane={pane}"
                                );
                            } else {
                                // iced supplies transformed float bounds, not its
                                // exact layer matrix. Recovering scale may change
                                // a quarter-pixel bin at a tie, but must not shift
                                // the text by a pixel.
                                assert_eq!(
                                    best_translation(&native, &copied),
                                    (0, 0),
                                    "scaled: font={font:?} prefix={prefix:?} dpi={dpi} at={at:?} pane={pane}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
