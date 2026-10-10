use super::*;
use iced::Color;
use iced::advanced::{Renderer as _, text::Renderer as _};
use smudgy_session_model::{
    Style,
    inline_content::{InlineDecoration, InlineOwner, TextEffect},
};

fn animated_shader() -> Arc<smudgy_session_model::text_shader::ShaderEffect> {
    use smudgy_session_model::text_shader::{Shader, ShaderEffect};
    let shader = Shader::compile(
        "identity.wgsl",
        "fn effect(p: vec2f) -> vec4f { if p.y >= 0.0 { return vec4f(0.0); } let c = sampleText(p + vec2f(0.0, 40.0 + 8.0 * sin(text.time * 3.0))); return vec4f(1.0, 0.12, 0.02, c.a * 0.7); }",
    )
    .unwrap();
    crate::text_effect::prewarm(&shader).unwrap();
    let uniforms = shader.uniforms(&Default::default()).unwrap();
    Arc::new(ShaderEffect {
        pane: false,
        scale: Default::default(),
        capture_scale: Default::default(),
        fade_in_ms: 0,
        fade_out_ms: 0,
        shader,
        uniforms,
        replace: false,
        hold: false,
        animated: true,
    })
}

type Painter = iced_tiny_skia::Renderer;
type Cache = State<<Painter as text::Renderer>::Paragraph>;

fn decorated(
    text: &str,
    range: Range<usize>,
    shader: Arc<smudgy_session_model::text_shader::ShaderEffect>,
) -> Arc<StyledLine> {
    let mut line = StyledLine::new(
        text,
        vec![smudgy_session_model::VtSpan {
            begin_pos: 0,
            end_pos: text.len(),
            style: Style::DEFAULT,
        }],
    );
    let mut effect = InlineDecoration::new(
        range,
        TextEffect {
            shader,

            duration_ms: 2000,
            outset: 64,
        },
        InlineOwner::default(),
    );
    effect.started -= Duration::from_millis(350);
    line.decorations = Some(Arc::new(vec![effect]));
    Arc::new(line)
}
fn painter() -> Painter {
    iced_tiny_skia::graphics::text::font_system()
        .write()
        .unwrap()
        .load_font(crate::assets::GEIST_MONO_BYTES.into());
    Painter::new(crate::assets::GEIST_MONO, Pixels(16.0))
}
fn layout(
    pane: &mut TerminalPane<'_, ()>,
    tree: &mut Tree,
    renderer: &Painter,
    size: Size,
) -> layout::Node {
    <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::layout(
        pane,
        tree,
        renderer,
        &layout::Limits::new(Size::ZERO, size),
    )
}
fn tree(pane: &TerminalPane<'_, ()>) -> Tree {
    Tree::new(pane as &dyn Widget<(), smudgy_theme::Theme, Painter>)
}

#[test]
fn wrapped_effect_uses_text_geometry_and_preserves_copy_and_instance() {
    let renderer = painter();
    let buffer = RefCell::new(TerminalBuffer::new());
    let text = "before café on fire across several wrapped lines after";
    let source = decorated(text, 7..text.len() - 6, animated_shader());
    let id = source.decorations.as_ref().unwrap()[0].id;
    buffer.borrow_mut().push_line(source);
    let selection = Rc::new(RefCell::new(Selection::None));
    let mut pane = TerminalPane::new(buffer.borrow(), selection.clone());
    let mut tree = tree(&pane);
    layout(&mut pane, &mut tree, &renderer, Size::new(150.0, 300.0));
    let cache = &tree.state.downcast_ref::<Cache>().cache[0];
    let effects = cache.effects.as_ref().unwrap();
    assert!(
        effects.len() > 1,
        "effect should follow the wrapped fragments"
    );
    assert!(effects.iter().all(|(effect, _, _)| effect.id == id));
    let height = cache.row_height();
    *selection.borrow_mut() = Selection::Selected {
        from: BufferPosition { line: 1, column: 7 },
        to: BufferPosition {
            line: 1,
            column: text.len() - 6,
        },
    };
    layout(&mut pane, &mut tree, &renderer, Size::new(150.0, 300.0));
    assert_eq!(
        tree.state.downcast_ref::<Cache>().cache[0].row_height(),
        height
    );
    assert_eq!(
        buffer.borrow().selected_text(&selection.borrow()),
        &text[7..text.len() - 6]
    );
    layout(&mut pane, &mut tree, &renderer, Size::new(700.0, 300.0));
    let cache = &tree.state.downcast_ref::<Cache>().cache[0];
    assert_eq!(cache.effects.as_ref().unwrap().len(), 1);
    assert_eq!(cache.effects.as_ref().unwrap()[0].0.id, id);
}

#[test]
fn overflow_is_painted_but_clipped_to_the_pane_and_offscreen_anchors_are_retained() {
    let mut renderer = painter();
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(decorated(
        "A warning: fire follows this text",
        11..33,
        animated_shader(),
    ));
    buffer.borrow_mut().push_line(Arc::new(StyledLine::new(
        "Ordinary text remains readable",
        vec![smudgy_session_model::VtSpan {
            begin_pos: 0,
            end_pos: "Ordinary text remains readable".len(),
            style: Style::DEFAULT,
        }],
    )));
    buffer
        .borrow_mut()
        .push_line(decorated("Attention", 0..9, animated_shader()));
    buffer.borrow_mut().push_line(Arc::new(StyledLine::new(
        "Last visible line",
        vec![smudgy_session_model::VtSpan {
            begin_pos: 0,
            end_pos: 17,
            style: Style::DEFAULT,
        }],
    )));
    buffer.borrow_mut().push_line(decorated(
        "An anchor just below the viewport",
        0..33,
        animated_shader(),
    ));
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default()).last_line_number(4);
    let mut tree = tree(&pane);
    let node = layout(&mut pane, &mut tree, &renderer, Size::new(640.0, 200.0));
    let state = tree.state.downcast_ref::<Cache>();
    assert_eq!(state.cache[0].line_number, 4);
    assert_eq!(state.effects_below[0].line_number, 5);
    let viewport = Rectangle::with_size(Size::new(640.0, 200.0));
    renderer.reset(viewport);
    pane.draw(
        &tree,
        &mut renderer,
        &smudgy_theme::smudgy(),
        &renderer::Style::default(),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    let layers = renderer.layers();
    assert!(layers.iter().any(|layer| !layer.text.is_empty()));
    assert!(
        layers
            .iter()
            .all(|layer| layer.bounds.intersection(&viewport) == Some(layer.bounds))
    );
}

#[test]
fn expired_effect_and_offscreen_effect_submit_no_primitives() {
    let mut renderer = painter();
    let line = decorated("attention", 0..9, animated_shader());
    let effect = &line.decorations.as_ref().unwrap()[0];
    let bounds = Rectangle::with_size(Size::new(80.0, 20.0));
    renderer.reset(Rectangle::with_size(Size::new(400.0, 300.0)));
    assert!(!crate::inline_effects::draw_with_fuel(
        &mut renderer,
        effect,
        bounds,
        bounds,
        effect.started + Duration::from_secs(3),
        None,
    ));
    assert!(!crate::inline_effects::draw_with_fuel(
        &mut renderer,
        effect,
        bounds,
        bounds + iced::Vector::new(500.0, 0.0),
        effect.started,
        None,
    ));
    assert!(renderer.layers().iter().all(|layer| layer.quads.is_empty()));
}

#[test]
fn decorated_buffer_index_tracks_edits_eviction_replacement_and_clear() {
    let mut buffer = TerminalBuffer::new_with_max_lines(std::num::NonZeroUsize::new(2).unwrap());
    let rich = decorated("fire", 0..4, animated_shader());
    let plain = Arc::new(StyledLine::new("plain", vec![]));
    buffer.push_line(rich.clone());
    buffer.push_line(plain.clone());
    buffer.push_line(plain.clone());
    assert!(!buffer.has_inline_effects());
    buffer.extend_line(rich.clone());
    buffer.extend_line(plain);
    assert!(buffer.has_inline_effects());
    buffer.begin_open_line_replacement();
    assert!(!buffer.has_inline_effects());
    buffer.finish_open_line_replacement(Some(rich.clone()));
    assert!(buffer.has_inline_effects());
    buffer.perform_line_operation(
        buffer.last_line_number(),
        smudgy_session_model::LineOperation::Remove { begin: 0, end: 4 },
    );
    assert!(!buffer.has_inline_effects());
    buffer.push_line(rich);
    buffer.clear_lines();
    assert!(!buffer.has_inline_effects());
}

#[test]
fn native_object_reserves_measured_box_wraps_and_preserves_copy() {
    let renderer = painter();
    let buffer = RefCell::new(TerminalBuffer::new());
    let mut source = StyledLine::from_styled_runs(
        &[("before Button after", Style::DEFAULT, None)],
        Style::DEFAULT,
    );
    source.objects = Some(Arc::new(vec![
        smudgy_session_model::inline_content::InlineObject::new(
            7..13,
            Arc::new(()),
            InlineOwner::default(),
        ),
    ]));
    buffer.borrow_mut().push_line(Arc::new(source));
    let selection = Rc::new(RefCell::new(Selection::None));
    let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_| {
        Some(
            iced::widget::button("Button")
                .width(120)
                .height(180)
                .on_press(())
                .into(),
        )
    });
    let mut pane =
        TerminalPane::new(buffer.borrow(), selection.clone()).inline_widgets(Some(resolver));
    let mut tree = tree(&pane);
    let node = layout(&mut pane, &mut tree, &renderer, Size::new(500.0, 300.0));
    let state = tree.state.downcast_ref::<Cache>();
    let cache = &state.cache[0];
    let spans = cache.spans.spans();
    let index = spans
        .iter()
        .position(|s| s.link.is_some_and(|m| m.object.is_some()))
        .unwrap();
    let bounds = cache.paragraph.span_bounds(index)[0];
    assert!((bounds.width - 120.0).abs() < 1.0, "{bounds:?}");
    assert!(
        (cache.row_height() - 180.0).abs() < 1.0,
        "row: {} {bounds:?}",
        cache.row_height()
    );
    *selection.borrow_mut() = Selection::Selected {
        from: BufferPosition { line: 1, column: 0 },
        to: BufferPosition {
            line: 1,
            column: 19,
        },
    };
    assert_eq!(
        buffer.borrow().selected_text(&selection.borrow()),
        "before Button after"
    );
    let mut messages = Vec::new();
    let cursor = mouse::Cursor::Available(Point::new(
        bounds.x + 10.0,
        300.0 - cache.row_height() + bounds.y + 10.0,
    ));
    for event in [
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ] {
        let mut shell = advanced::Shell::new(&mut messages);
        <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::update(
            &mut pane,
            &mut tree,
            &event,
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(node.size()),
        );
    }
    assert_eq!(
        messages.len(),
        1,
        "button must capture and publish through the terminal"
    );
    layout(&mut pane, &mut tree, &renderer, Size::new(150.0, 300.0));
    assert!(tree.state.downcast_ref::<Cache>().cache[0].row_height() > 180.0);
    pane = pane.last_line_position(0.5);
    layout(&mut pane, &mut tree, &renderer, Size::new(150.0, 100.0));
    let state = tree.state.downcast_ref::<Cache>();
    assert!((state.bottom_offset - state.cache[0].row_height() / 2.0).abs() < 1.0);
}

#[test]
fn text_contributes_line_height_but_small_objects_do_not_have_a_minimum() {
    let renderer = painter();
    for font_size in [16.0, 28.0] {
        let (_, line_height) = effective_metrics(&crate::prefs::current(), Some(font_size));
        for (text, width) in [
            ("Attention starts here [red pulse]", 700.0),
            ("[red pulse]", 700.0),
            ("Attention starts here [red pulse]", 180.0),
        ] {
            let mut source =
                StyledLine::from_styled_runs(&[(text, Style::DEFAULT, None)], Style::DEFAULT);
            source.objects = Some(Arc::new(vec![
                smudgy_session_model::inline_content::InlineObject::new(
                    text.find('[').unwrap()..text.len(),
                    Arc::new(()),
                    InlineOwner::default(),
                ),
            ]));
            let buffer = RefCell::new(TerminalBuffer::new());
            buffer.borrow_mut().push_line(Arc::new(source));
            let resolver =
                crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_| {
                    Some(iced::widget::space().width(12).height(12).into())
                });
            let mut pane = TerminalPane::new(buffer.borrow(), Rc::default())
                .font_size(Some(font_size))
                .inline_widgets(Some(resolver));
            let mut tree = tree(&pane);
            layout(&mut pane, &mut tree, &renderer, Size::new(width, 300.0));
            let cache = &tree.state.downcast_ref::<Cache>().cache[0];
            let runs: Vec<_> = cache.paragraph.buffer().layout_runs().collect();
            if text == "[red pulse]" {
                assert!((cache.row_height() - 12.0).abs() < 0.01);
                continue;
            }
            assert!(
                runs.iter().all(|run| run.line_height >= line_height - 0.01),
                "{text:?} at {font_size}px must retain normal leading on every wrapped row"
            );
            if width == 700.0 {
                assert!((cache.row_height() - line_height).abs() < 0.01);
            } else {
                assert!(runs.len() > 1);
            }
            let spans = cache.spans.spans();
            let index = spans
                .iter()
                .position(|span| span.link.is_some_and(|m| m.object.is_some()))
                .unwrap();
            assert!(
                (cache.paragraph.span_bounds(index)[0].width - 12.0).abs() < 0.1,
                "line leading must not stretch the canvas"
            );
        }
    }
}

#[test]
fn span_font_metrics_participate_in_layout_wrapping_and_selection() {
    use smudgy_session_model::inline_content::{InlineFont, InlineFontOptions, InlineFontStyle};
    let renderer = painter();
    let mut source =
        StyledLine::from_styled_runs(&[("small BIG tail", Style::DEFAULT, None)], Style::DEFAULT);
    source.fonts = Some(Arc::new(vec![InlineFont {
        range: 6..9,
        options: InlineFontOptions {
            font_size: Some(32),
            font_weight: Some(700),
            font_style: Some(InlineFontStyle::Italic),
            font_face: Some("monospace".into()),
        },
    }]));
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(source));
    let selection = Rc::new(RefCell::new(Selection::Selected {
        from: BufferPosition { line: 1, column: 6 },
        to: BufferPosition { line: 1, column: 9 },
    }));
    let mut pane = TerminalPane::new(buffer.borrow(), selection.clone()).font_size(Some(16.0));
    let mut tree = tree(&pane);
    layout(&mut pane, &mut tree, &renderer, Size::new(500.0, 300.0));
    let cache = &tree.state.downcast_ref::<Cache>().cache[0];
    let (_, base_height) = effective_metrics(&crate::prefs::current(), Some(16.0));
    assert!((cache.row_height() - base_height * 2.0).abs() < 0.01);
    let spans = cache.spans.spans();
    let large = spans.iter().find(|span| span.text == "BIG").unwrap();
    assert_eq!(large.size, Some(Pixels(32.0)));
    let font = large.font.unwrap();
    assert_eq!(font.family, iced::font::Family::Monospace);
    assert_eq!(font.weight, iced::font::Weight::Bold);
    assert_eq!(font.style, iced::font::Style::Italic);
    assert_eq!(buffer.borrow().selected_text(&selection.borrow()), "BIG");
    layout(&mut pane, &mut tree, &renderer, Size::new(85.0, 300.0));
    let cache = &tree.state.downcast_ref::<Cache>().cache[0];
    let heights: Vec<_> = cache
        .paragraph
        .buffer()
        .layout_runs()
        .map(|run| run.line_height)
        .collect();
    assert!(heights.len() > 1);
    assert!(
        heights
            .iter()
            .any(|height| (*height - base_height).abs() < 0.01)
    );
    assert!(
        heights
            .iter()
            .any(|height| (*height - base_height * 2.0).abs() < 0.01)
    );
}

#[test]
fn an_object_row_keeps_its_paragraph_while_its_widget_keeps_its_size() {
    use smudgy_session_model::inline_content::InlineObject;
    let renderer = painter();
    let text = "before widget after";
    let mut line = (*decorated(text, 0..6, animated_shader())).clone();
    line.objects = Some(Arc::new(vec![InlineObject::new(
        7..13,
        Arc::new(()),
        InlineOwner::default(),
    )]));
    let width = Rc::new(std::cell::Cell::new(100.0f32));
    let views = Rc::new(std::cell::Cell::new(0));
    let (measured, viewed) = (width.clone(), views.clone());
    let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_| {
        viewed.set(viewed.get() + 1);
        Some(
            iced::widget::Space::new()
                .width(measured.get())
                .height(30)
                .into(),
        )
    });
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(line));
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default()).inline_widgets(Some(resolver));
    let mut tree = tree(&pane);
    let size = Size::new(500.0, 300.0);
    let identity = |tree: &Tree| {
        let row = &tree.state.downcast_ref::<Cache>().cache[0];
        (
            std::ptr::from_ref(row.paragraph.buffer()),
            Arc::as_ptr(row.effects.as_ref().unwrap()),
        )
    };
    let glyph_width = |tree: &Tree| {
        let row = &tree.state.downcast_ref::<Cache>().cache[0];
        row.spans
            .spans()
            .iter()
            .find(|span| span.link.is_some_and(|m| m.object.is_some()))
            .and_then(|span| span.size)
            .unwrap()
            .0
    };
    layout(&mut pane, &mut tree, &renderer, size);
    let first = identity(&tree);
    assert_eq!(glyph_width(&tree), 100.0);

    // Bound props resolve in the factory, so every layout views the widget
    // again; the shaped paragraph and effect capture are kept.
    layout(&mut pane, &mut tree, &renderer, size);
    assert_eq!(views.get(), 2);
    assert_eq!(identity(&tree), first);

    width.set(160.0);
    layout(&mut pane, &mut tree, &renderer, size);
    assert_ne!(identity(&tree), first);
    assert_eq!(glyph_width(&tree), 160.0);
}

#[test]
fn copied_line_mounts_two_independent_buttons_and_concealed_prefix_mapping_stays_exact() {
    let renderer = painter();
    let mut source = StyledLine::from_styled_runs(
        &[("prefixéButton tail", Style::DEFAULT, None)],
        Style::DEFAULT,
    );
    source.objects = Some(Arc::new(vec![
        smudgy_session_model::inline_content::InlineObject::new(
            8..14,
            Arc::new(()),
            InlineOwner::default(),
        ),
    ]));
    let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_| {
        Some(
            iced::widget::button("Button")
                .width(100)
                .height(30)
                .on_press(())
                .into(),
        )
    });
    let mut children = Vec::new();
    let host = crate::inline_object::Host::<(), smudgy_theme::Theme, Painter>::init(&mut children);
    let rendered = host.prepare(
        &source,
        1,
        crate::terminal_buffer::RenderedSpans {
            spans: Rc::new(vec![Span::new("prefix Button tail")]),
            offsets: RenderedOffsets::Mapped {
                identity_prefix: 6,
                source: vec![6, 8, 19].into(),
                rendered: vec![6, 7, 18].into(),
            },
        },
        Some(&resolver),
        &renderer,
        500.0,
    );
    assert_eq!(rendered.offsets.source_to_rendered(3), 3);
    assert_eq!(rendered.offsets.rendered_to_source(3), 3);
    assert_eq!(rendered.offsets.source_to_rendered(14), 10);
    assert_eq!(rendered.offsets.rendered_to_source(10), 14);

    let buffer = RefCell::new(TerminalBuffer::new());
    let source = Arc::new(source);
    buffer.borrow_mut().push_line(source.clone());
    buffer.borrow_mut().push_line(source);
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default()).inline_widgets(Some(resolver));
    let mut tree = tree(&pane);
    let node = layout(&mut pane, &mut tree, &renderer, Size::new(500.0, 300.0));
    let state = tree.state.downcast_ref::<Cache>();
    let mut points = Vec::new();
    let mut y = 300.0;
    for cache in &state.cache {
        y -= cache.row_height();
        let spans = cache.spans.spans();
        let index = spans
            .iter()
            .position(|s| s.link.is_some_and(|m| m.object.is_some()))
            .unwrap();
        let bounds = cache.paragraph.span_bounds(index)[0];
        points.push(Point::new(bounds.center_x(), y + bounds.center_y()));
    }
    let mut messages = Vec::new();
    for point in points {
        for event in [
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ] {
            <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::update(
                &mut pane,
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &renderer,
                &mut clipboard::Null,
                &mut advanced::Shell::new(&mut messages),
                &Rectangle::with_size(node.size()),
            );
        }
    }
    assert_eq!(
        messages.len(),
        2,
        "copies of a line must own separate native widget trees"
    );
}

#[test]
#[ignore = "requires a hardware GPU; run serialized"]
fn shader_is_glyph_seeded_animated_and_clipped() {
    use iced::advanced::renderer::Headless;
    let _ = painter(); // load the bundled font
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        crate::assets::GEIST_MONO,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .expect("hardware GPU required for the fire render test");
    assert_gpu_shader_scheduling(&renderer);
    assert_gpu_shader_text_stays_in_front(&mut renderer);
    crate::text_effect::register_renderer(&renderer);
    let text = "FireEffect";
    let paragraph = <Painter as text::Renderer>::Paragraph::with_text(iced::advanced::text::Text {
        content: text,
        bounds: Size::new(500.0, 100.0),
        size: Pixels(44.0),
        line_height: text::LineHeight::Relative(1.25),
        font: crate::assets::GEIST_MONO,
        align_x: text::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::None,
    });
    let region = Rectangle::with_size(paragraph.min_bounds());
    let fuel = crate::text_effect::Input::from_paragraph(&paragraph, region).unwrap();
    let source = decorated(text, 0..text.len(), animated_shader());
    let effect = &source.decorations.as_ref().unwrap()[0];
    let viewport = Rectangle::with_size(Size::new(440.0, 190.0));
    let position = Point::new(80.0, 105.0);
    let mut previous = None;
    for millis in [300, 800, 1300] {
        renderer.reset(viewport);
        assert!(crate::inline_effects::draw_with_fuel(
            &mut renderer,
            effect,
            region + iced::Vector::new(position.x, position.y),
            viewport,
            effect.started + Duration::from_millis(millis),
            Some(&fuel),
        ));
        renderer.with_layer(viewport, |renderer| {
            renderer.fill_paragraph(
                &paragraph,
                position,
                Color::from_rgb8(255, 235, 198),
                viewport,
            );
        });
        let rgba = Headless::screenshot(
            &mut renderer,
            Size::new(440, 190),
            1.0,
            crate::prefs::current().palette.background,
        );
        let pixels =
            tiny_skia::Pixmap::from_vec(rgba, tiny_skia::IntSize::from_wh(440, 190).unwrap())
                .unwrap();
        let flame_pixels = pixels.data()[..440 * 100 * 4]
            .chunks_exact(4)
            .filter(|p| p[0] > 70 && p[0] > p[2].saturating_add(35))
            .count();
        assert!(
            flame_pixels > 400,
            "animated fixture must paint beyond its source text: {flame_pixels}"
        );
        if let Some(previous) = previous {
            assert_ne!(pixels.data(), previous);
        }
        previous = Some(pixels.data().to_vec());
    }
}

fn assert_gpu_shader_text_stays_in_front(renderer: &mut iced_wgpu::Renderer) {
    use iced::advanced::renderer::Headless;
    type GpuPane<'a> = dyn Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer> + 'a;
    let viewport = Rectangle::with_size(Size::new(520.0, 180.0));
    for font_size in [16.0, 44.0] {
        for selected in [false, true] {
            let mut capture = |fire: bool| {
                let text = "Readable FireEffect";
                let mut source = (*decorated(text, 0..text.len(), animated_shader())).clone();
                if fire {
                    let effect = &mut Arc::make_mut(source.decorations.as_mut().unwrap())[0];
                    effect.effect.duration_ms = 0;
                    effect.started = Instant::now() - Duration::from_millis(800);
                } else {
                    source.decorations = None;
                }
                let buffer = RefCell::new(TerminalBuffer::new());
                buffer.borrow_mut().push_line(Arc::new(source));
                let selection = Rc::new(RefCell::new(if selected {
                    Selection::Selected {
                        from: BufferPosition { line: 1, column: 0 },
                        to: BufferPosition {
                            line: 1,
                            column: text.len(),
                        },
                    }
                } else {
                    Selection::None
                }));
                let mut pane =
                    TerminalPane::<()>::new(buffer.borrow(), selection).font_size(Some(font_size));
                let mut tree = Tree::new(&pane as &GpuPane<'_>);
                let widget = &mut pane as &mut GpuPane<'_>;
                let node = widget.layout(
                    &mut tree,
                    renderer,
                    &layout::Limits::new(Size::ZERO, viewport.size()),
                );
                renderer.reset(viewport);
                widget.draw(
                    &tree,
                    renderer,
                    &smudgy_theme::smudgy(),
                    &renderer::Style::default(),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &viewport,
                );
                Headless::screenshot(
                    renderer,
                    Size::new(520, 180),
                    1.0,
                    crate::prefs::current().palette.background,
                )
            };
            let plain = capture(false);
            let burning = capture(true);

            let mut checked = 0;
            for (plain, burning) in plain.chunks_exact(4).zip(burning.chunks_exact(4)) {
                // Bright glyph pixels, including antialiased strokes. The flame
                // must neither tint nor obscure them, even on ordinary 16px text.
                if plain[..3].iter().all(|channel| *channel >= 160) {
                    checked += 1;
                    assert!(
                        plain[..3]
                            .iter()
                            .zip(&burning[..3])
                            .all(|(a, b)| a.abs_diff(*b) <= 12),
                        "foreground changed at {font_size}px (selected={selected}): {plain:?} -> {burning:?}"
                    );
                }
            }
            assert!(checked > 60);
            assert_ne!(plain, burning, "the flame must still be visible");
        }
    }
}

#[test]
fn shader_has_no_software_animation_or_fuel_allocation() {
    let mut renderer = painter();
    let source = decorated("fire", 0..4, animated_shader());
    let effect = &source.decorations.as_ref().unwrap()[0];
    let bounds = Rectangle::with_size(Size::new(60.0, 20.0));
    renderer.reset(bounds);
    assert!(!crate::inline_effects::draw_with_fuel(
        &mut renderer,
        effect,
        bounds,
        bounds,
        effect.started,
        None,
    ));
    assert!(
        renderer
            .layers()
            .iter()
            .all(|l| l.quads.is_empty() && l.images.is_empty())
    );
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(source);
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default());
    let mut tree = tree(&pane);
    layout(&mut pane, &mut tree, &renderer, Size::new(100.0, 100.0));
    let state = tree.state.downcast_ref::<Cache>();
    assert!(!effects_need_frame(
        &state.cache,
        bounds,
        bounds,
        Instant::now(),
        &renderer
    ));
    assert!(state.cache[0].effects.as_ref().unwrap()[0].2.is_none());
}

fn assert_gpu_shader_scheduling(renderer: &iced_wgpu::Renderer) {
    let buffer = RefCell::new(TerminalBuffer::new());
    let source = decorated("fire", 0..4, animated_shader());
    let started = source.decorations.as_ref().unwrap()[0].started;
    buffer.borrow_mut().push_line(source);
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default());
    let mut tree = Tree::new(&pane as &dyn Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>);
    let size = Size::new(200.0, 150.0);
    let node =
        <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>>::layout(
            &mut pane,
            &mut tree,
            renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
    for (elapsed, active) in [
        (Duration::from_millis(500), true),
        (Duration::from_secs(3), false),
    ] {
        let mut messages = Vec::new();
        let mut shell = advanced::Shell::new(&mut messages);
        <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>>::update(
            &mut pane,
            &mut tree,
            &Event::Window(window::Event::RedrawRequested(started + elapsed)),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::with_size(size),
        );
        assert_eq!(
            shell.redraw_request() == window::RedrawRequest::NextFrame,
            active,
            "visible fire requests the presentation cadence only while active"
        );
    }
}

#[test]
#[ignore = "requires a hardware GPU; run explicitly for text effect changes"]
fn replacement_shader_preserves_adjacent_text_selection_and_retirement() {
    use iced::advanced::renderer::Headless;
    use smudgy_session_model::text_shader::{Shader, ShaderEffect};
    let _ = painter();
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        crate::assets::GEIST_MONO,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .expect("GPU required");
    crate::text_effect::register_renderer(&renderer);
    let cases = [
        (
            "overlay",
            "fn effect(p:vec2f)->vec4f { let c=sampleText(p+vec2f(0,8)); return vec4f(1,0,0,c.a*0.5); }",
        ),
        (
            "remove",
            "fn effect(p:vec2f)->vec4f { return sampleText(p) * (1.0-text.progress); }",
        ),
    ];
    let buffer = RefCell::new(TerminalBuffer::new());
    let size = Size::new(800.0, 520.0);
    let viewport = Rectangle::with_size(size);
    for (name, source) in cases {
        let shader = Shader::compile(name, source).unwrap();
        crate::text_effect::prewarm(&shader).unwrap();
        let uniforms = shader.uniforms(&Default::default()).unwrap();
        let text = format!("{name}: Spellbound");
        let mut line = (*decorated(
            &text,
            name.len() + 2..text.len(),
            Arc::new(ShaderEffect {
                pane: false,
                scale: Default::default(),
                capture_scale: Default::default(),
                fade_in_ms: 0,
                fade_out_ms: 0,
                shader,
                uniforms,
                replace: name == "remove",
                hold: name == "remove",
                animated: true,
            }),
        ))
        .clone();
        let decoration = &mut Arc::make_mut(line.decorations.as_mut().unwrap())[0];
        decoration.started = Instant::now() - Duration::from_millis(950);
        decoration.effect.outset = if name == "poison-cloud" { 112 } else { 72 };
        decoration.effect.duration_ms = if name == "remove" { 2400 } else { 0 };
        buffer.borrow_mut().push_line(Arc::new(line));
        buffer
            .borrow_mut()
            .push_line(Arc::new(StyledLine::from_output_str(" ")));
    }
    let selection = Rc::new(RefCell::new(Selection::None));
    let mut pane = TerminalPane::<()>::new(buffer.borrow(), selection).font_size(Some(36.0));
    let mut tree = Tree::new(&pane as &dyn Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>);
    let node =
        <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>>::layout(
            &mut pane,
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
    renderer.reset(viewport);
    <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>>::draw(
        &pane,
        &tree,
        &mut renderer,
        &smudgy_theme::smudgy(),
        &renderer::Style::default(),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    let _rgba = Headless::screenshot(
        &mut renderer,
        Size::new(800, 520),
        1.0,
        crate::prefs::current().palette.background,
    );

    drop(pane);
    drop(tree);
    drop(buffer);

    // A finished foreground shader hides only its own glyphs. Adjacent text and
    // the original copy/selection geometry survive, and retirement restores it.
    let shader = Shader::compile("smoke", cases[1].1).unwrap();
    crate::text_effect::prewarm(&shader).unwrap();
    let uniforms = shader.uniforms(&Default::default()).unwrap();
    let mut line = (*decorated(
        "before VANISH after",
        7..13,
        Arc::new(ShaderEffect {
            pane: false,
            scale: Default::default(),
            capture_scale: Default::default(),
            fade_in_ms: 0,
            fade_out_ms: 0,
            shader,
            uniforms,
            replace: true,
            hold: true,
            animated: true,
        }),
    ))
    .clone();
    let effect = &mut Arc::make_mut(line.decorations.as_mut().unwrap())[0];
    effect.started = Instant::now() - Duration::from_secs(3);
    let owner = effect.owner.clone();
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(line));
    let selection = Rc::new(RefCell::new(Selection::Selected {
        from: BufferPosition { line: 1, column: 7 },
        to: BufferPosition {
            line: 1,
            column: 13,
        },
    }));
    let mut pane =
        TerminalPane::<()>::new(buffer.borrow(), selection.clone()).font_size(Some(36.0));
    let mut tree = Tree::new(&pane as &dyn Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>);
    let node =
        <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>>::layout(
            &mut pane,
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
    let state = tree
        .state
        .downcast_ref::<State<<iced_wgpu::Renderer as text::Renderer>::Paragraph>>();

    assert_eq!(buffer.borrow().selected_text(&selection.borrow()), "VANISH");
    assert!(!effects_need_frame(
        &state.cache,
        viewport,
        viewport,
        Instant::now(),
        &renderer
    ));
    let mut capture = || {
        renderer.reset(viewport);
        <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer>>::draw(
            &pane,
            &tree,
            &mut renderer,
            &smudgy_theme::smudgy(),
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        Headless::screenshot(
            &mut renderer,
            Size::new(800, 520),
            1.0,
            crate::prefs::current().palette.background,
        )
    };
    let dissolved = capture();
    assert!(!replacement_spans(&state.cache[0], Instant::now()).is_empty());
    owner.retire();
    let restored = capture();
    assert_ne!(dissolved, restored, "the actual foreground must disappear");
    let bright = |image: &[u8]| {
        image
            .chunks_exact(4)
            .filter(|p| p[..3].iter().all(|c| *c > 160))
            .count()
    };
    assert!(bright(&restored) > bright(&dissolved) + 100);
    assert!(replacement_spans(&state.cache[0], Instant::now()).is_empty());
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn cold_static_replacement_keeps_native_text_and_requests_its_ready_frame() {
    use iced::advanced::renderer::Headless;
    use smudgy_session_model::text_shader::{Shader, ShaderEffect};
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        iced::Font::MONOSPACE,
        iced::Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    let shader = Shader::compile(
        "static-copy",
        "fn effect(p: vec2f) -> vec4f {return sampleText(p);}",
    )
    .unwrap();
    let source = decorated(
        "before STATIC after",
        7..13,
        Arc::new(ShaderEffect {
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
    );
    let mut source = (*source).clone();
    Arc::make_mut(source.decorations.as_mut().unwrap())[0]
        .effect
        .duration_ms = 0;
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(source));
    let mut pane: iced::Element<'_, (), smudgy_theme::Theme, iced_wgpu::Renderer> =
        iced::Element::new(TerminalPane::new(buffer.borrow(), Rc::default()));
    let mut tree = Tree::new(&pane);
    let size = Size::new(400.0, 120.0);
    let viewport = Rectangle::with_size(size);
    let node =
        pane.as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    // The native event loop updates before its first draw. A cold static
    // shader must schedule another frame even though no draw has marked it pending.
    let mut initial_messages = Vec::new();
    let mut initial_shell = advanced::Shell::new(&mut initial_messages);
    pane.as_widget_mut().update(
        &mut tree,
        &Event::Window(window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &renderer,
        &mut clipboard::Null,
        &mut initial_shell,
        &viewport,
    );
    assert_eq!(
        initial_shell.redraw_request(),
        window::RedrawRequest::NextFrame
    );
    let state = tree
        .state
        .downcast_ref::<State<<iced_wgpu::Renderer as text::Renderer>::Paragraph>>();
    assert!(replacement_spans(&state.cache[0], Instant::now()).is_empty());
    assert!(
        state.cache[0]
            .effects
            .as_ref()
            .unwrap()
            .iter()
            .any(|(_, _, input)| input
                .as_ref()
                .is_some_and(crate::text_effect::Input::pending))
    );
    let paint = |renderer: &mut iced_wgpu::Renderer, tree: &Tree| {
        renderer.reset(viewport);
        pane.as_widget().draw(
            tree,
            renderer,
            &smudgy_theme::smudgy(),
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        Headless::screenshot(
            renderer,
            Size::new(400, 120),
            1.0,
            crate::prefs::current().palette.background,
        )
    };
    let first = paint(&mut renderer, &tree);
    assert!(
        first
            .chunks_exact(4)
            .filter(|p| p[0] > 120 && p[1] > 120 && p[2] > 120)
            .count()
            > 100,
        "cold foreground stays readable"
    );
    let mut messages = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        renderer.reset(viewport);
        pane.as_widget().draw(
            &tree,
            &mut renderer,
            &smudgy_theme::smudgy(),
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        let state = tree
            .state
            .downcast_ref::<State<<iced_wgpu::Renderer as text::Renderer>::Paragraph>>();
        if !replacement_spans(&state.cache[0], Instant::now()).is_empty() {
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    Headless::screenshot(
        &mut renderer,
        Size::new(400, 120),
        1.0,
        crate::prefs::current().palette.background,
    );
    let mut shell = advanced::Shell::new(&mut messages);
    pane.as_widget_mut().update(
        &mut tree,
        &Event::Window(window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &renderer,
        &mut clipboard::Null,
        &mut shell,
        &viewport,
    );
    assert_ne!(
        shell.redraw_request(),
        window::RedrawRequest::NextFrame,
        "static effects stop requesting frames after warming"
    );
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn failed_animated_shader_preserves_native_text_and_stops_requesting_frames() {
    use iced::advanced::renderer::Headless;
    use smudgy_session_model::text_shader::{Shader, ShaderEffect};
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        iced::Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    crate::text_effect::register_renderer(&renderer);
    let mut shader = Shader::compile(
        "rejected-in-terminal",
        "fn effect(p: vec2f) -> vec4f {return sampleText(p);}",
    )
    .unwrap();
    Arc::get_mut(&mut shader).unwrap().source = "invalid WGSL".into();
    assert!(crate::text_effect::prewarm(&shader).is_err());
    let source = decorated(
        "READABLE",
        0..8,
        Arc::new(ShaderEffect {
            uniforms: shader.uniforms(&Default::default()).unwrap(),
            shader,
            pane: false,
            scale: Default::default(),
            capture_scale: Default::default(),
            fade_in_ms: 0,
            fade_out_ms: 0,
            replace: true,
            hold: false,
            animated: true,
        }),
    );
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(source);
    let mut pane: iced::Element<'_, (), smudgy_theme::Theme, iced_wgpu::Renderer> =
        iced::Element::new(TerminalPane::new(buffer.borrow(), Rc::default()));
    let mut tree = Tree::new(&pane);
    let size = Size::new(400.0, 120.0);
    let viewport = Rectangle::with_size(size);
    let node =
        pane.as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    let mut messages = Vec::new();
    let mut shell = advanced::Shell::new(&mut messages);
    pane.as_widget_mut().update(
        &mut tree,
        &Event::Window(window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &renderer,
        &mut clipboard::Null,
        &mut shell,
        &viewport,
    );
    assert_ne!(shell.redraw_request(), window::RedrawRequest::NextFrame);
    renderer.reset(viewport);
    pane.as_widget().draw(
        &tree,
        &mut renderer,
        &smudgy_theme::smudgy(),
        &renderer::Style::default(),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    let state = tree
        .state
        .downcast_ref::<State<<iced_wgpu::Renderer as text::Renderer>::Paragraph>>();
    assert!(replacement_spans(&state.cache[0], Instant::now()).is_empty());
    for (_, _, input) in state.cache[0].effects.as_ref().unwrap().iter() {
        let input = input.as_ref().unwrap();
        assert!(!input.pending());
        assert!(!input.admitted());
        assert_eq!(input.stats().uploads, 0);
    }
    let image = Headless::screenshot(
        &mut renderer,
        Size::new(400, 120),
        1.0,
        crate::prefs::current().palette.background,
    );
    assert!(
        image
            .chunks_exact(4)
            .filter(|p| p[0] > 120 && p[1] > 120 && p[2] > 120)
            .count()
            > 100,
        "GPU rejection must leave readable native text"
    );
}

#[test]
fn scrolling_reuses_unchanged_paragraphs_across_visible_and_overscan_bands() {
    let renderer = painter();
    let buffer = RefCell::new(TerminalBuffer::new());
    for index in 0..80 {
        buffer.borrow_mut().push_line(decorated(
            &format!("{index:02} Storm gathers"),
            3..16,
            animated_shader(),
        ));
    }
    let selection = Rc::new(RefCell::new(Selection::None));
    let mut pane = TerminalPane::new(buffer.borrow(), selection.clone());
    let mut state = tree(&pane);
    let size = Size::new(800.0, 120.0);
    layout(&mut pane, &mut state, &renderer, size);
    let before: rustc_hash::FxHashMap<_, _> = {
        let cache = state.state.downcast_ref::<Cache>();
        cache
            .cache
            .iter()
            .chain(&cache.effects_above)
            .map(|row| (row.line_number, row.paragraph.clone()))
            .collect()
    };
    pane = pane.last_line_number(75);
    layout(&mut pane, &mut state, &renderer, size);
    let after = state.state.downcast_ref::<Cache>();
    let mut reused = 0;
    for row in after
        .cache
        .iter()
        .chain(&after.effects_above)
        .chain(&after.effects_below)
    {
        if let Some(previous) = before.get(&row.line_number) {
            assert!(
                std::ptr::eq(previous.buffer(), row.paragraph.buffer()),
                "scrolling must retain the shaped paragraph for line {}",
                row.line_number
            );
            reused += 1;
        }
    }
    assert!(reused > 5);
    // The overscan band is only as deep as the widest effect outset, so a row
    // that left it while scrolled is shaped afresh; every row that stayed in
    // some band keeps its paragraph when the tail grows by a line.
    let retained: std::collections::HashSet<_> = after
        .cache
        .iter()
        .chain(&after.effects_above)
        .chain(&after.effects_below)
        .map(|row| row.line_number)
        .collect();
    drop(pane);
    buffer
        .borrow_mut()
        .push_line(Arc::new(StyledLine::from_output_str("A new line")));
    let mut pane = TerminalPane::new(buffer.borrow(), selection);
    layout(&mut pane, &mut state, &renderer, size);
    let cache = state.state.downcast_ref::<Cache>();
    let shared = cache
        .cache
        .iter()
        .filter(|row| before.contains_key(&row.line_number) && retained.contains(&row.line_number));
    assert!(shared.clone().count() > 0);
    for row in shared {
        assert!(std::ptr::eq(
            before[&row.line_number].buffer(),
            row.paragraph.buffer()
        ));
    }
}
