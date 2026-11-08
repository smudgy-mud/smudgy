use super::*;
use iced::Color;
use iced::advanced::{Renderer as _, text::Renderer as _};
use smudgy_session_model::{
    Style,
    inline_content::{Baseline, InlineDecoration, InlineOwner, TextEffect},
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
fn concealed_tail_and_scroll_anchor_fill_from_earlier_visible_lines() {
    use smudgy_session_model::{
        LinkProtocol, LinkVisibility, LinkVisibilityAction, LinkVisibilityExpire,
    };

    let renderer = painter();
    let buffer = RefCell::new(TerminalBuffer::new());
    for number in 1..=8 {
        let mut line = StyledLine::from_output_str(&format!("Line {number}"));
        if number >= 7 {
            line.links.push(LinkSpan {
                begin_pos: 0,
                end_pos: line.text.len(),
                action: LinkAction::Configured {
                    primary: Some(Box::new(LinkAction::Send(Arc::from("look")))),
                    disabled: false,
                    primary_enabled: true,
                    menu: None,
                    menu_on_left_click: false,
                    protocol: Some(LinkProtocol {
                        visibility: Some(LinkVisibility {
                            action: if number == 7 {
                                LinkVisibilityAction::Conceal
                            } else {
                                LinkVisibilityAction::Reveal
                            },
                            delay_ms: (number == 8).then_some(0),
                            expire: LinkVisibilityExpire {
                                input: false,
                                prompt: false,
                                output: false,
                                output_delay_ms: 0,
                            },
                            whole_line: true,
                        }),
                        ..LinkProtocol::default()
                    }),
                },
                tooltip: None,
                style: None,
            });
        }
        buffer.borrow_mut().push_line(Arc::new(line));
    }
    let links = buffer.borrow().link_state();
    let deleted_key = buffer.borrow().link_keys().next().unwrap();
    assert_eq!(links.borrow().line(deleted_key), Some(7));
    links
        .borrow_mut()
        .activate_visibility(deleted_key, Instant::now());

    let selection = Rc::new(RefCell::new(Selection::None));
    let mut pane = TerminalPane::new(buffer.borrow(), selection.clone());
    let mut state = tree(&pane);
    let (_, line_height) = effective_metrics(&crate::prefs::current(), None);
    let size = Size::new(600.0, line_height * 2.5);
    for anchor in [None, Some(7), Some(4)] {
        pane.last_line_number = anchor;
        layout(&mut pane, &mut state, &renderer, size);
        let cached = state.state.downcast_ref::<Cache>();
        assert_eq!(cached.last_line_number, anchor.unwrap_or(8));
        assert_eq!(cached.cache[0].line_number, anchor.unwrap_or(8).min(6));
        assert!(cached.cache.iter().all(|row| row.line_number <= 6));
        assert!(
            cached
                .cache
                .iter()
                .map(ParagraphCache::row_height)
                .sum::<f32>()
                >= size.height
        );
    }
    assert!(!links.borrow().contains(deleted_key));
    assert!(
        links.borrow().line_concealed(7),
        "retirement retains the tombstone"
    );

    // A timer reveals the newest line without changing the retained line count.
    links
        .borrow_mut()
        .update_visibility_timers(Instant::now() + Duration::from_millis(1));
    pane.last_line_number = None;
    layout(&mut pane, &mut state, &renderer, size);
    let cached = state.state.downcast_ref::<Cache>();
    assert_eq!(cached.cache[0].line_number, 8);
    assert!(cached.cache.iter().all(|row| row.line_number != 7));
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
    let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_, _| {
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
                crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_, _| {
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
    let resolver =
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_, _| {
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

    // Keep the first shaping alive: once dropped, the reshaped row can be
    // allocated at the same addresses and compare equal by identity.
    let _first_row = {
        let row = &tree.state.downcast_ref::<Cache>().cache[0];
        (row.paragraph.clone(), row.effects.clone())
    };
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
    let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_, _| {
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
        // A row taller than the widget: no riser, so the glyph is three bytes.
        &crate::inline_object::ObjectMetrics {
            cell_width: 8.0,
            line_height: 40.0,
        },
        0.0,
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

/// One row holding an object, laid out `width` wide: its visual lines, the
/// row's height, the rendered length of the whole text (the object's text
/// becomes its glyph, and a riser when it is tall and sits text on its last
/// row), the width of the riser's word, and the pane's measured cell
/// advance.
struct ObjectRow {
    /// Per visual line: the text baseline in pixels from the line's top,
    /// and the line's height.
    lines: Vec<(f32, f32)>,
    row_height: f32,
    rendered_len: usize,
    /// What the glyphs shaped for the riser and the break after it
    /// advance by: nothing, or they would show as a gap.
    riser_width: f32,
    advance: f32,
}

/// Lay out `text` with `object` (`(byte range, width, height, baseline
/// policy)`) in a pane `width` wide.
fn row_beside_object(
    renderer: &Painter,
    text: &str,
    font_size: f32,
    object: Option<(Range<usize>, f32, f32, Baseline)>,
    width: f32,
) -> ObjectRow {
    let mut source = StyledLine::from_styled_runs(&[(text, Style::DEFAULT, None)], Style::DEFAULT);
    let range = object.as_ref().map(|(range, ..)| range.clone());
    let resolver = object.map(|(range, width, height, baseline)| {
        let mut object = smudgy_session_model::inline_content::InlineObject::new(
            range,
            Arc::new(()),
            InlineOwner::default(),
        );
        object.baseline = baseline;
        source.objects = Some(Arc::new(vec![object]));
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_, _| {
            Some(iced::widget::space().width(width).height(height).into())
        })
    });
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(source));
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default())
        .font_size(Some(font_size))
        .inline_widgets(resolver);
    let mut tree = tree(&pane);
    layout(&mut pane, &mut tree, renderer, Size::new(width, 400.0));
    let state = tree.state.downcast_ref::<Cache>();
    let cache = &state.cache[0];
    let riser = range.map(|range| {
        let start = cache.offsets.source_to_rendered(range.start) + 3;
        start..start + 6
    });
    let mut riser_width = 0.0;
    let lines = cache
        .paragraph
        .buffer()
        .layout_runs()
        .map(|run| {
            if let Some(riser) = &riser {
                riser_width += run
                    .glyphs
                    .iter()
                    .filter(|glyph| glyph.start >= riser.start && glyph.end <= riser.end)
                    .map(|glyph| glyph.w)
                    .sum::<f32>();
            }
            (run.line_y - run.line_top, run.line_height)
        })
        .collect();
    ObjectRow {
        lines,
        row_height: cache.row_height(),
        rendered_len: cache.offsets.source_to_rendered(text.len()),
        riser_width,
        advance: state.advance.expect("the pane measured its advance").2,
    }
}

/// Where the text baseline lands in a row holding an object, in pixels from
/// the row's top, plus the row's height and the rendered length of the whole
/// text; the text shapes on one line.
fn baseline_beside_object(
    renderer: &Painter,
    text: &str,
    font_size: f32,
    object: Option<(Range<usize>, f32, f32, Baseline)>,
) -> (f32, f32, usize) {
    let row = row_beside_object(renderer, text, font_size, object, 900.0);
    assert_eq!(row.lines.len(), 1, "{text:?} must shape on one visual line");
    (row.lines[0].0, row.row_height, row.rendered_len)
}

/// kitty leaves the cursor on an image's LAST row, so text printed straight
/// after a three-row placement must share that bottom row's baseline — the
/// baseline the same text has in a plain row, two rows further down.
#[test]
fn text_after_a_three_row_object_sits_beside_its_last_row() {
    let renderer = painter();
    let font_size = 16.0;
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(font_size));
    let (plain, plain_row, _) = baseline_beside_object(&renderer, "before after", font_size, None);
    assert!((plain_row - row_h).abs() < 0.01);

    // Five placeholder columns, then text; the object is three rows tall.
    let text = "before      after";
    let (beside, row, rendered_len) = baseline_beside_object(
        &renderer,
        text,
        font_size,
        Some((6..11, 40.0, 3.0 * row_h, Baseline::LastRow)),
    );
    assert_eq!(
        rendered_len,
        text.len() - 5 + 9,
        "five placeholder bytes become the glyph, its riser and the break after \
         it, three bytes each"
    );
    assert!(
        (row - 3.0 * row_h).abs() < 0.01,
        "row grows to the object: {row}"
    );
    let last_row_baseline = 2.0 * row_h + plain;
    assert!(
        (beside - last_row_baseline).abs() < 0.5,
        "text baseline {beside}px from the row top; the last row's baseline is \
         {last_row_baseline}px (plain row baseline {plain}px, row height {row_h}px, \
         object row {row}px)"
    );
}

/// The riser is a word of its own, closed by a zero-width space, so when the
/// word after a three-row object wraps, the riser stays on the object's
/// line: the text before the object keeps the last row's baseline and the
/// wrapped word takes a plain line with a plain baseline. (A riser glued to
/// that word left with it: the object's line fell back to centring its text
/// mid-image, and the plain line below carried a three-row riser that pushed
/// its baseline two rows down.)
#[test]
fn a_word_wrapping_after_a_three_row_object_leaves_the_riser_behind() {
    let renderer = painter();
    let font_size = 16.0;
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(font_size));
    let (plain, _, _) = baseline_beside_object(&renderer, "before after", font_size, None);
    // As the processor lays a placement out: the placeholder columns are
    // the object's whole range, and the next text follows them directly.
    let text = "before     after";
    let object = Some((6..11, 40.0, 3.0 * row_h, Baseline::LastRow));
    let one_line = row_beside_object(&renderer, text, font_size, object.clone(), 900.0);
    let advance = one_line.advance;
    assert!(
        one_line.riser_width.abs() < 1e-3,
        "the riser's word advances by {}px",
        one_line.riser_width
    );
    // Room for "before", the object and the riser, but not for "after".
    let width = 6.0 * advance + 40.0 + 2.0;
    let row = row_beside_object(&renderer, text, font_size, object, width);
    assert_eq!(row.rendered_len, text.len() - 5 + 9);
    assert!(
        (row.row_height - 4.0 * row_h).abs() < 0.01,
        "three object rows and a plain one: {}",
        row.row_height
    );
    let [
        (first_baseline, first_height),
        (second_baseline, second_height),
    ] = row.lines[..]
    else {
        panic!("two visual lines, got {:?}", row.lines);
    };
    assert!(
        (first_height - 3.0 * row_h).abs() < 0.01,
        "the object's line is three rows tall: {first_height}"
    );
    assert!(
        (first_baseline - (2.0 * row_h + plain)).abs() < 0.5,
        "text before the object sits on its last row: baseline {first_baseline}px, \
         expected {}px",
        2.0 * row_h + plain
    );
    assert!(
        (second_height - row_h).abs() < 0.01,
        "the wrapped word's line is a plain row: {second_height}"
    );
    assert!(
        (second_baseline - plain).abs() < 0.5,
        "the wrapped word keeps a plain baseline: {second_baseline}px, expected {plain}px"
    );
}

/// Wrapping before the object is the other way round: the text before it
/// takes a plain line, and the object's line, now first in nothing but
/// position, still sits the word after it on the object's last row.
#[test]
fn text_wrapping_before_a_three_row_object_keeps_a_plain_line() {
    let renderer = painter();
    let font_size = 16.0;
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(font_size));
    let (plain, _, _) = baseline_beside_object(&renderer, "before after", font_size, None);
    let text = "before this     after";
    let object = Some((11..16, 40.0, 3.0 * row_h, Baseline::LastRow));
    let advance = row_beside_object(&renderer, text, font_size, object.clone(), 900.0).advance;
    // "before this" fits; the object no longer does, and the object with
    // its riser and "after" fits a line of its own.
    let width = 11.0 * advance + 30.0;
    let row = row_beside_object(&renderer, text, font_size, object, width);
    assert_eq!(row.rendered_len, text.len() - 5 + 9);
    let [
        (first_baseline, first_height),
        (second_baseline, second_height),
    ] = row.lines[..]
    else {
        panic!("two visual lines, got {:?}", row.lines);
    };
    assert!(
        (first_height - row_h).abs() < 0.01 && (first_baseline - plain).abs() < 0.5,
        "the text before the object is a plain line: {first_height}px tall, baseline \
         {first_baseline}px"
    );
    assert!(
        (second_height - 3.0 * row_h).abs() < 0.01,
        "the object's line is three rows tall: {second_height}"
    );
    assert!(
        (second_baseline - (2.0 * row_h + plain)).abs() < 0.5,
        "text after the object sits on its last row: baseline {second_baseline}px, \
         expected {}px",
        2.0 * row_h + plain
    );
}

/// An object's placeholder glyph is sized by the object's WIDTH, so its font
/// metrics must not move the text baseline: a wide one-row object leaves the
/// text exactly where a plain row puts it.
#[test]
fn a_wide_one_row_object_does_not_move_the_text_baseline() {
    let renderer = painter();
    let font_size = 16.0;
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(font_size));
    let (plain, _, _) = baseline_beside_object(&renderer, "before after", font_size, None);
    let text = "before      after";
    let (beside, row, rendered_len) = baseline_beside_object(
        &renderer,
        text,
        font_size,
        Some((6..11, 400.0, row_h, Baseline::LastRow)),
    );
    assert_eq!(
        rendered_len,
        text.len() - 5 + 3,
        "no riser for a one-row object"
    );
    assert!(
        (row - row_h).abs() < 0.01,
        "a one-row object keeps the row height: {row}"
    );
    assert!(
        (beside - plain).abs() < 0.5,
        "text baseline moved from {plain}px to {beside}px beside a 400px-wide object"
    );
}

/// A script widget keeps the alignment it always had: text beside a
/// one-and-a-half-row button sits centred in the row, not on its bottom
/// row, because only a placement asks for the last-row baseline.
#[test]
fn text_beside_a_centred_object_stays_centred_in_its_row() {
    let renderer = painter();
    let font_size = 16.0;
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(font_size));
    let (plain, _, _) = baseline_beside_object(&renderer, "before after", font_size, None);
    let text = "before      after";
    let (beside, row, rendered_len) = baseline_beside_object(
        &renderer,
        text,
        font_size,
        Some((6..11, 40.0, 1.5 * row_h, Baseline::Centered)),
    );
    assert_eq!(
        rendered_len,
        text.len() - 5 + 3,
        "no riser for a centred object, however tall"
    );
    assert!(
        (row - 1.5 * row_h).abs() < 0.01,
        "row grows to the object: {row}"
    );
    // cosmic-text centres the text's glyph box in the taller row, so the
    // baseline moves down by half the extra height and no further.
    let centred = plain + (row - row_h) / 2.0;
    let bottom_row = (row - row_h) + plain;
    assert!(
        (beside - centred).abs() < 0.5,
        "text baseline {beside}px from the row top; centred is {centred}px, the \
         bottom row's baseline would be {bottom_row}px"
    );
}

#[test]
fn object_factories_measure_in_the_rows_cell_grid() {
    use crate::inline_object::ObjectMetrics;
    let renderer = painter();
    let font_size = 16.0;
    let mut source =
        StyledLine::from_styled_runs(&[("ab image cd", Style::DEFAULT, None)], Style::DEFAULT);
    source.objects = Some(Arc::new(vec![
        smudgy_session_model::inline_content::InlineObject::new(
            3..8,
            Arc::new(()),
            InlineOwner::default(),
        ),
    ]));
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(source));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let record = seen.clone();
    // Sized in cells, as a terminal graphics placement is.
    let resolver =
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_, metrics| {
            record.borrow_mut().push(*metrics);
            Some(
                iced::widget::space()
                    .width(3.0 * metrics.cell_width)
                    .height(2.0 * metrics.line_height)
                    .into(),
            )
        });
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default())
        .font_size(Some(font_size))
        .inline_widgets(Some(resolver));
    let mut tree = tree(&pane);
    let size = Size::new(500.0, 300.0);
    layout(&mut pane, &mut tree, &renderer, size);
    let (_, line_height) = effective_metrics(&crate::prefs::current(), Some(font_size));
    let (_, _, advance) = tree
        .state
        .downcast_ref::<Cache>()
        .advance
        .expect("the pane measured its advance");
    assert!(advance > 0.0);
    let grid = ObjectMetrics {
        cell_width: advance,
        line_height,
    };
    assert_eq!(*seen.borrow(), vec![grid], "prepare views with the grid");

    // A cached row re-views its objects with the same grid.
    layout(&mut pane, &mut tree, &renderer, size);
    assert_eq!(
        *seen.borrow(),
        vec![grid, grid],
        "refresh views with the grid"
    );

    let row = &tree.state.downcast_ref::<Cache>().cache[0];
    let glyph = row
        .spans
        .spans()
        .iter()
        .find(|span| span.link.is_some_and(|m| m.object.is_some()))
        .and_then(|span| span.size)
        .unwrap()
        .0;
    assert!((glyph - 3.0 * advance).abs() < 1e-3, "three cells wide");
    assert!(
        (row.row_height() - 2.0 * line_height).abs() < 0.01,
        "two rows tall: {}",
        row.row_height()
    );
}

/// An anchor at byte `at`, as the graphics processor makes one for a placement that
/// leaves the cursor where it was: an empty range, centred, reaching `outset` pixels,
/// drawn under the text when `z` is negative.
fn anchor(
    at: usize,
    z: i32,
    outset: u16,
    resource: Arc<dyn std::any::Any + Send + Sync>,
) -> smudgy_session_model::inline_content::InlineObject {
    use smudgy_session_model::inline_content::{InlineObject, Layer};
    let mut object = InlineObject::new(at..at, resource, InlineOwner::default());
    object.baseline = Baseline::Centered;
    object.layer = if z < 0 { Layer::Under } else { Layer::Over };
    object.z = z;
    object.paint_outset = outset;
    object
}

/// A terminal graphics placement of a 1x1 image whose id is `image`, for an object's
/// resource: the host reads the image id to order a line's objects.
fn placement_of(image: u32) -> Arc<smudgy_protocol::graphics::Placement> {
    use smudgy_protocol::graphics::{Fit, ImageRecord, Placement, PlacementGeometry, SourceRect};
    let geometry = PlacementGeometry {
        columns: 1,
        rows: 1,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        x_offset: 0,
        y_offset: 0,
        fit: Fit::Natural,
        z: 0,
    };
    Arc::new(Placement::new(
        Arc::new(ImageRecord::new(image, 0, 1, 1)),
        0,
        geometry,
    ))
}

/// A box of one colour, sized up front: what an inline object draws in these tests, so
/// the order its quad reaches the renderer in shows. With `true`, it draws through a
/// layer of its own, as a cropped image does.
struct Swatch(Size, Color, bool);

impl<R: advanced::Renderer> Widget<(), smudgy_theme::Theme, R> for Swatch {
    fn size(&self) -> Size<iced::Length> {
        Size::new(
            iced::Length::Fixed(self.0.width),
            iced::Length::Fixed(self.0.height),
        )
    }
    fn layout(&mut self, _: &mut Tree, _: &R, _: &layout::Limits) -> layout::Node {
        layout::Node::new(self.0)
    }
    fn draw(
        &self,
        _: &Tree,
        renderer: &mut R,
        _: &smudgy_theme::Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
        let quad = Quad {
            bounds: layout.bounds(),
            ..Default::default()
        };
        if self.2 {
            renderer.with_layer(layout.bounds(), |renderer| {
                renderer.fill_quad(quad, self.1);
            });
        } else {
            renderer.fill_quad(quad, self.1);
        }
    }
}

/// What an inline widget saw of the pane's events.
#[derive(Debug, Default, PartialEq, Eq)]
struct Seen {
    redraws: usize,
    mouse: usize,
}

/// A widget of a fixed size that counts the events it is handed and, given an instant,
/// asks for a redraw then on each redraw request, as an animated placement does.
struct EventProbe(Size, Rc<RefCell<Seen>>, Option<Instant>);

impl<R: advanced::Renderer> Widget<(), smudgy_theme::Theme, R> for EventProbe {
    fn size(&self) -> Size<iced::Length> {
        Size::new(
            iced::Length::Fixed(self.0.width),
            iced::Length::Fixed(self.0.height),
        )
    }
    fn layout(&mut self, _: &mut Tree, _: &R, _: &layout::Limits) -> layout::Node {
        layout::Node::new(self.0)
    }
    fn draw(
        &self,
        _: &Tree,
        _: &mut R,
        _: &smudgy_theme::Theme,
        _: &renderer::Style,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
    }
    fn update(
        &mut self,
        _: &mut Tree,
        event: &Event,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &R,
        _: &mut dyn advanced::Clipboard,
        shell: &mut advanced::Shell<'_, ()>,
        _: &Rectangle,
    ) {
        let mut seen = self.1.borrow_mut();
        match event {
            Event::Window(window::Event::RedrawRequested(_)) => {
                seen.redraws += 1;
                if let Some(at) = self.2 {
                    shell.request_redraw_at(at);
                }
            }
            Event::Mouse(_) => seen.mouse += 1,
            _ => {}
        }
    }
}

/// An anchor is inert to the mouse — the reader selects and clicks through
/// the text under it — but sees the window's events: a redraw request
/// reaches its widget, which an animated placement needs to ask for its
/// next frame, and what it asks for reaches the pane's shell. A plain
/// object sees both.
#[test]
fn an_anchor_sees_redraw_requests_but_not_the_mouse() {
    let renderer = painter();
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(16.0));
    let size = Size::new(500.0, 400.0);
    let text = "before after";
    let next_frame = Instant::now() + Duration::from_millis(40);
    for anchored in [true, false] {
        let seen = Rc::new(RefCell::new(Seen::default()));
        let boxed = Size::new(60.0, 2.0 * row_h);
        let probe_seen = seen.clone();
        let resolver =
            crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_, _| {
                Some(iced::Element::new(EventProbe(
                    boxed,
                    probe_seen.clone(),
                    Some(next_frame),
                )))
            });
        let object = if anchored {
            anchor(7, 0, 40, Arc::new(()))
        } else {
            smudgy_session_model::inline_content::InlineObject::new(
                7..12,
                Arc::new(()),
                InlineOwner::default(),
            )
        };
        let mut source =
            StyledLine::from_styled_runs(&[(text, Style::DEFAULT, None)], Style::DEFAULT);
        source.objects = Some(Arc::new(vec![object]));
        let buffer = RefCell::new(TerminalBuffer::new());
        buffer.borrow_mut().push_line(Arc::new(source));
        let mut pane = TerminalPane::new(buffer.borrow(), Rc::default())
            .font_size(Some(16.0))
            .inline_widgets(Some(resolver));
        let mut tree = tree(&pane);
        let node = layout(&mut pane, &mut tree, &renderer, size);
        let boxes = crate::inline_object::Host::<(), smudgy_theme::Theme, Painter>::get(&tree)
            .map(|host| host.boxes())
            .unwrap_or_default();
        let [(_, Some(bounds))] = boxes[..] else {
            panic!("one positioned box, got {boxes:?}");
        };
        let over = mouse::Cursor::Available(Point::new(bounds.center_x(), bounds.center_y()));
        let mut messages = Vec::new();
        // What the pane's shell asks for after the event.
        let mut send = |event: Event| {
            let mut shell = advanced::Shell::new(&mut messages);
            <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::update(
                &mut pane,
                &mut tree,
                &event,
                Layout::new(&node),
                over,
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &Rectangle::with_size(node.size()),
            );
            shell.redraw_request()
        };
        assert_eq!(
            send(Event::Window(
                window::Event::RedrawRequested(Instant::now()),
            )),
            iced::window::RedrawRequest::At(next_frame),
            "anchored: {anchored}: the widget's request is the pane's"
        );
        assert_eq!(
            *seen.borrow(),
            Seen {
                redraws: 1,
                mouse: 0
            },
            "anchored: {anchored}"
        );
        send(Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(bounds.center_x(), bounds.center_y()),
        }));
        send(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
        send(Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        )));
        assert_eq!(
            *seen.borrow(),
            Seen {
                redraws: 1,
                mouse: if anchored { 0 } else { 3 }
            },
            "anchored: {anchored}"
        );
    }
}

/// One laid-out row: what an anchor must leave alone, and where its box went.
struct LaidRow {
    row_height: f32,
    /// The text baseline from the row's top, per visual line.
    baselines: Vec<f32>,
    /// The x of every glyph of the row's text (not an object's), in order.
    glyphs: Vec<f32>,
    rendered_len: usize,
    offsets: RenderedOffsets,
    /// Each mount's box, `None` for one the layout did not position.
    boxes: Vec<Option<Rectangle>>,
    advance: f32,
}

/// Lay out `text` holding `objects`, each viewed by `resolver`, alone in a pane `size`
/// large.
fn lay_out_row(
    renderer: &Painter,
    text: &str,
    objects: Vec<smudgy_session_model::inline_content::InlineObject>,
    resolver: Option<crate::inline_object::Resolver>,
    size: Size,
) -> LaidRow {
    let mut source = StyledLine::from_styled_runs(&[(text, Style::DEFAULT, None)], Style::DEFAULT);
    if !objects.is_empty() {
        source.objects = Some(Arc::new(objects));
    }
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(source));
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default())
        .font_size(Some(16.0))
        .inline_widgets(resolver);
    let mut tree = tree(&pane);
    layout(&mut pane, &mut tree, renderer, size);
    let state = tree.state.downcast_ref::<Cache>();
    let cache = &state.cache[0];
    let spans = cache.spans.spans();
    let runs: Vec<_> = cache.paragraph.buffer().layout_runs().collect();
    LaidRow {
        row_height: cache.row_height(),
        baselines: runs.iter().map(|run| run.line_y - run.line_top).collect(),
        glyphs: runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
            .filter(|glyph| {
                spans[glyph.metadata]
                    .link
                    .is_none_or(|metadata| metadata.object.is_none())
            })
            .map(|glyph| glyph.x)
            .collect(),
        rendered_len: cache.offsets.source_to_rendered(text.len()),
        offsets: cache.offsets.clone(),
        boxes: crate::inline_object::Host::<(), smudgy_theme::Theme, Painter>::get(&tree)
            .map(|host| host.boxes().into_iter().map(|(_, bounds)| bounds).collect())
            .unwrap_or_default(),
        advance: state.advance.expect("the pane measured its advance").2,
    }
}

/// A placement made with `C=1` (or a negative z-index) leaves the cursor where it was:
/// its box hangs from that cell over the text that follows, the row keeps its height,
/// and the text around it keeps every glyph where it was.
#[test]
fn an_anchor_hangs_from_its_cell_without_moving_the_row_or_its_text() {
    let renderer = painter();
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(16.0));
    let size = Size::new(500.0, 400.0);
    let text = "before after";
    let plain = lay_out_row(&renderer, text, Vec::new(), None, size);
    let advance = plain.advance;
    // Three cells by two rows, at the cursor's cell: the "a" of "after".
    let boxed = Size::new(3.0 * advance, 2.0 * row_h);
    let resolver =
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_, _| {
            Some(
                iced::widget::space()
                    .width(boxed.width)
                    .height(boxed.height)
                    .into(),
            )
        });
    let anchored = lay_out_row(
        &renderer,
        text,
        vec![anchor(7, 0, 40, Arc::new(()))],
        Some(resolver),
        size,
    );

    assert!(
        (anchored.row_height - row_h).abs() < 0.01,
        "the row keeps its height: {} against {row_h}",
        anchored.row_height
    );
    assert_eq!(anchored.baselines.len(), 1);
    assert!(
        (anchored.baselines[0] - plain.baselines[0]).abs() < 0.01,
        "text baseline moved from {} to {}",
        plain.baselines[0],
        anchored.baselines[0]
    );
    assert_eq!(anchored.glyphs.len(), plain.glyphs.len());
    for (with, without) in anchored.glyphs.iter().zip(&plain.glyphs) {
        assert!(
            (with - without).abs() < 0.01,
            "a glyph moved: {:?} against {:?}",
            anchored.glyphs,
            plain.glyphs
        );
    }
    assert_eq!(anchored.rendered_len, text.len() + 1, "a one-byte marker");

    let [Some(bounds)] = anchored.boxes[..] else {
        panic!("one positioned box, got {:?}", anchored.boxes);
    };
    assert!(
        (bounds.x - 7.0 * advance).abs() < 0.5,
        "at the cursor's column: x {} for cell 7 of {advance}px",
        bounds.x
    );
    assert!(
        (bounds.y - (size.height - row_h)).abs() < 0.5,
        "at the top of its row: y {}, the row starts at {}",
        bounds.y,
        size.height - row_h
    );
    assert_eq!(bounds.size(), boxed, "laid out as the resolver measured it");

    // "before " is 0..7, the marker 7..8, "after" 8..13: text either side maps one to
    // one, the anchor's own offset maps past its marker (the text before it keeps a
    // point of its own at the marker's start, so it still maps one to one), and a hit
    // on the marker lands on the anchor's cell.
    let offsets = &anchored.offsets;
    assert_eq!(offsets.source_to_rendered(3), 3);
    assert_eq!(offsets.rendered_to_source(3), 3);
    assert_eq!(offsets.source_to_rendered(7), 8, "past the marker");
    assert_eq!(offsets.rendered_to_source(7), 7);
    assert_eq!(offsets.rendered_to_source(8), 7);
    assert_eq!(offsets.source_to_rendered(9), 10);
    assert_eq!(offsets.rendered_to_source(10), 9);
    assert_eq!(offsets.source_to_rendered(text.len()), text.len() + 1);
}

/// An anchor after a line's last character, one on an empty line and one right after
/// another object's glyph each find their marker; one inside another object's text has
/// no character left to mark and is not placed (nor drawn) at all.
#[test]
fn anchors_at_a_lines_end_on_an_empty_line_and_beside_an_object() {
    let renderer = painter();
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(16.0));
    let size = Size::new(500.0, 400.0);
    let resolver = || {
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |object, _| {
            let width = if object.range.is_empty() { 10.0 } else { 40.0 };
            Some(iced::widget::space().width(width).height(row_h).into())
        })
    };
    let end = lay_out_row(
        &renderer,
        "abc",
        vec![anchor(3, 0, 10, Arc::new(()))],
        Some(resolver()),
        size,
    );
    let [Some(bounds)] = end.boxes[..] else {
        panic!("{:?}", end.boxes);
    };
    assert!((bounds.x - 3.0 * end.advance).abs() < 0.5, "{bounds:?}");

    let empty = lay_out_row(
        &renderer,
        "",
        vec![anchor(0, -1, 10, Arc::new(()))],
        Some(resolver()),
        size,
    );
    let [Some(bounds)] = empty.boxes[..] else {
        panic!("{:?}", empty.boxes);
    };
    assert!(bounds.x.abs() < 0.5 && (bounds.y - (size.height - row_h)).abs() < 0.5);
    assert!(
        (empty.row_height - row_h).abs() < 0.01,
        "a line holding only an anchor is one plain row: {}",
        empty.row_height
    );

    // "xx" then an object over "widget", then "yy"; anchors inside its text and right
    // after it.
    let text = "xxwidgetyy";
    let object = smudgy_session_model::inline_content::InlineObject::new(
        2..8,
        Arc::new(()),
        InlineOwner::default(),
    );
    let beside = lay_out_row(
        &renderer,
        text,
        vec![
            object,
            anchor(5, 0, 10, Arc::new(())),
            anchor(8, 0, 10, Arc::new(())),
        ],
        Some(resolver()),
        size,
    );
    let advance = beside.advance;
    // Mounts are keyed by object id, in creation order: the object, then the anchors.
    let [Some(glyph), None, Some(after)] = beside.boxes[..] else {
        panic!("{:?}", beside.boxes);
    };
    assert!((glyph.x - 2.0 * advance).abs() < 0.5, "{glyph:?}");
    assert!(
        (after.x - (2.0 * advance + 40.0)).abs() < 0.5,
        "right after the object's glyph: {after:?}"
    );
    // The object's glyph, and the one marker placed: three bytes and one.
    assert_eq!(beside.rendered_len, text.len() - 6 + 3 + 1);
}

/// The quads in render order: layer by layer as they open, each layer's in order.
fn quad_colors(renderer: &mut Painter) -> Vec<(usize, Color)> {
    renderer
        .layers()
        .iter()
        .enumerate()
        .flat_map(|(index, layer)| {
            layer
                .quads
                .iter()
                .filter_map(move |(_, background)| match background {
                    Background::Color(color) => Some((index, *color)),
                    Background::Gradient(_) => None,
                })
        })
        .collect()
}

/// Draw a pane of `lines`, `size` large with `viewport` of it in view, whose objects
/// `resolver` views, and return the renderer.
fn draw_pane(
    lines: Vec<StyledLine>,
    resolver: crate::inline_object::Resolver,
    size: Size,
    viewport: Rectangle,
) -> Painter {
    let mut renderer = painter();
    let buffer = RefCell::new(TerminalBuffer::new());
    for line in lines {
        buffer.borrow_mut().push_line(Arc::new(line));
    }
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default()).inline_widgets(Some(resolver));
    let mut tree = tree(&pane);
    let node = layout(&mut pane, &mut tree, &renderer, size);
    renderer.reset(Rectangle::with_size(size));
    pane.draw(
        &tree,
        &mut renderer,
        &smudgy_theme::smudgy(),
        &renderer::Style::default(),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    renderer
}

/// Draw a one-line pane of `objects` over text in `style` (each object drawn as a swatch
/// in the colour `paint` picks, through a layer of its own when it says so) and return
/// the renderer.
fn draw_row(
    style: Style,
    objects: Vec<smudgy_session_model::inline_content::InlineObject>,
    paint: impl Fn(&smudgy_session_model::inline_content::InlineObject) -> (Color, bool) + 'static,
) -> Painter {
    let (_, row_h) = effective_metrics(&crate::prefs::current(), None);
    let mut source =
        StyledLine::from_styled_runs(&[("text over and under", style, None)], Style::DEFAULT);
    source.objects = Some(Arc::new(objects));
    let resolver =
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |object, _| {
            let (color, layered) = paint(object);
            Some(iced::Element::new(Swatch(
                Size::new(60.0, 2.0 * row_h),
                color,
                layered,
            )))
        });
    let size = Size::new(500.0, 200.0);
    draw_pane(vec![source], resolver, size, Rectangle::with_size(size))
}

/// The index of the layer the row's text was drawn in.
fn text_layer(renderer: &mut Painter) -> usize {
    renderer
        .layers()
        .iter()
        .position(|layer| !layer.text.is_empty())
        .expect("the row's text was drawn")
}

/// A negative z-index draws under the row's text and a zero one over it: layers paint in
/// the order they open, so the under pass opens one before the text's and the over pass
/// one after.
#[test]
fn under_placements_draw_before_the_text_and_over_ones_after() {
    const UNDER: Color = Color::from_rgb(1.0, 0.0, 0.0);
    const OVER: Color = Color::from_rgb(0.0, 0.0, 1.0);
    let mut renderer = draw_row(
        Style::DEFAULT,
        vec![
            anchor(0, -1, 60, Arc::new(())),
            anchor(5, 0, 60, Arc::new(())),
        ],
        |object| (if object.z < 0 { UNDER } else { OVER }, false),
    );
    let quads = quad_colors(&mut renderer);
    let layer_of = |color: Color| {
        quads
            .iter()
            .find(|(_, quad)| *quad == color)
            .map(|(index, _)| *index)
            .expect("the swatch was drawn")
    };
    let text = renderer
        .layers()
        .iter()
        .position(|layer| !layer.text.is_empty())
        .expect("the row's text was drawn");
    let (under, over) = (layer_of(UNDER), layer_of(OVER));
    assert!(
        under < text && text < over,
        "under in layer {under}, text in {text}, over in {over}"
    );

    // Without anything under the text, the text stays in the pane's own layer.
    let mut renderer = draw_row(Style::DEFAULT, vec![anchor(5, 0, 60, Arc::new(()))], |_| {
        (OVER, false)
    });
    let text = renderer
        .layers()
        .iter()
        .position(|layer| !layer.text.is_empty());
    assert_eq!(text, Some(0));
}

/// A line's objects draw in ascending z-index, then image id, in each pass; script
/// widgets (z 0, no image) keep their creation order. All of them overlap, so each one
/// drawn after another takes a layer of its own: an earlier one that drew through a
/// layer (as a cropped image does) would otherwise paint above it.
#[test]
fn a_lines_objects_draw_in_ascending_z_then_image_id() {
    let colors: Vec<Color> = (0..6)
        .map(|index| Color::from_rgb(index as f32 / 8.0, 0.5, 0.5))
        .collect();
    // Created in this order; each paints the colour of its index.
    let objects = vec![
        anchor(3, 2, 60, placement_of(1)),
        anchor(3, 0, 60, placement_of(9)),
        anchor(3, -1, 60, placement_of(4)),
        anchor(3, 0, 60, placement_of(3)),
        anchor(3, -5, 60, placement_of(7)),
        anchor(3, 0, 60, Arc::new(())),
    ];
    let ids: Vec<u64> = objects.iter().map(|object| object.id).collect();
    let palette = colors.clone();
    // In each pass, every other one (from the first drawn) draws through its own layer.
    let layered = [false, true, false, false, true, true];
    let mut renderer = draw_row(Style::DEFAULT, objects, move |object| {
        let index = ids.iter().position(|id| *id == object.id).unwrap();
        (palette[index], layered[index])
    });
    let drawn: Vec<usize> = quad_colors(&mut renderer)
        .iter()
        .filter_map(|(_, color)| colors.iter().position(|c| c == color))
        .collect();
    // Under: z -5 then -1. Over: the script widget (z 0, no image), image 3, image 9,
    // then z 2.
    assert_eq!(drawn, vec![4, 2, 5, 3, 1, 0]);
}

/// An anchor lies over text the reader selects and clicks: a button under the pointer
/// takes no click and sets no cursor there, where the same button as an ordinary object
/// does.
#[test]
fn anchors_are_inert_to_the_mouse() {
    let renderer = painter();
    for anchored in [false, true] {
        let text = "click here please";
        let mut source =
            StyledLine::from_styled_runs(&[(text, Style::DEFAULT, None)], Style::DEFAULT);
        source.objects = Some(Arc::new(vec![if anchored {
            anchor(0, 0, 60, Arc::new(()))
        } else {
            smudgy_session_model::inline_content::InlineObject::new(
                0..5,
                Arc::new(()),
                InlineOwner::default(),
            )
        }]));
        let buffer = RefCell::new(TerminalBuffer::new());
        buffer.borrow_mut().push_line(Arc::new(source));
        let resolver =
            crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_, _| {
                Some(
                    iced::widget::button("Button")
                        .width(60)
                        .height(20)
                        .on_press(())
                        .into(),
                )
            });
        let mut pane =
            TerminalPane::new(buffer.borrow(), Rc::default()).inline_widgets(Some(resolver));
        let mut tree = tree(&pane);
        let size = Size::new(500.0, 200.0);
        let node = layout(&mut pane, &mut tree, &renderer, size);
        let [Some(bounds)] =
            crate::inline_object::Host::<(), smudgy_theme::Theme, Painter>::get(&tree)
                .unwrap()
                .boxes()
                .into_iter()
                .map(|(_, bounds)| bounds)
                .collect::<Vec<_>>()[..]
        else {
            panic!("one positioned button");
        };
        let cursor = mouse::Cursor::Available(bounds.center());
        let mut messages = Vec::new();
        for event in [
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ] {
            <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::update(
                &mut pane,
                &mut tree,
                &event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut clipboard::Null,
                &mut advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
        }
        let interaction =
            <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::mouse_interaction(
                &pane,
                &tree,
                Layout::new(&node),
                cursor,
                &Rectangle::with_size(size),
                &renderer,
            );
        if anchored {
            assert!(messages.is_empty(), "an anchor took the click");
            assert_eq!(interaction, mouse::Interaction::Text);
        } else {
            assert_eq!(messages.len(), 1, "the ordinary button takes the click");
            assert_eq!(interaction, mouse::Interaction::Pointer);
        }
    }
}

/// A text style with a background colour, which bakes to a span highlight: a quad under
/// the text.
fn red_background() -> Style {
    Style {
        bg: smudgy_session_model::Color::Ansi {
            color: smudgy_session_model::AnsiColor::Red,
            bold: false,
        },
        ..Style::DEFAULT
    }
}

/// The graphics protocol puts a placement whose z-index is below `i32::MIN / 2` under
/// cells' background colours too, and every other negative one above the backgrounds
/// and under the text: the rows' background quads are a pass of their own between the
/// two under passes, and the text's layer opens after all three.
#[test]
fn backgrounds_draw_between_the_two_under_passes_and_under_the_text() {
    const DEEP: Color = Color::from_rgb(1.0, 0.0, 0.0);
    const SHALLOW: Color = Color::from_rgb(0.0, 1.0, 0.0);
    const OVER: Color = Color::from_rgb(0.0, 0.0, 1.0);
    let swatch = |object: &smudgy_session_model::inline_content::InlineObject| {
        let color = if object.z < crate::inline_object::Under::BACKGROUNDS_BELOW {
            DEEP
        } else if object.z < 0 {
            SHALLOW
        } else {
            OVER
        };
        (color, false)
    };
    let mut renderer = draw_row(
        red_background(),
        vec![
            anchor(0, i32::MIN, 60, Arc::new(())),
            anchor(0, -1, 60, Arc::new(())),
            anchor(5, 0, 60, Arc::new(())),
        ],
        swatch,
    );
    let quads = quad_colors(&mut renderer);
    let layer_of = |color: Color| {
        quads
            .iter()
            .find(|(_, quad)| *quad == color)
            .map(|(index, _)| *index)
            .expect("the swatch was drawn")
    };
    let background = quads
        .iter()
        .find(|(_, quad)| ![DEEP, SHALLOW, OVER].contains(quad))
        .map(|(index, _)| *index)
        .expect("the text's background quad was drawn");
    let text = text_layer(&mut renderer);
    assert!(
        layer_of(DEEP) < background
            && background < layer_of(SHALLOW)
            && layer_of(SHALLOW) < text
            && text < layer_of(OVER),
        "deep {}, background {background}, shallow {}, text {text}, over {}",
        layer_of(DEEP),
        layer_of(SHALLOW),
        layer_of(OVER)
    );

    // Nothing under the backgrounds: they stay in the pane's own layer, and the
    // placement under the text alone still parts them from the text.
    let mut renderer = draw_row(
        red_background(),
        vec![anchor(0, -1, 60, Arc::new(()))],
        swatch,
    );
    let quads = quad_colors(&mut renderer);
    let background = quads
        .iter()
        .find(|(_, quad)| *quad != SHALLOW)
        .map(|(index, _)| *index)
        .expect("the background quad");
    let shallow = quads
        .iter()
        .find(|(_, quad)| *quad == SHALLOW)
        .map(|(index, _)| *index)
        .expect("the swatch");
    let text = text_layer(&mut renderer);
    assert_eq!(background, 0, "the pane's own layer");
    assert!(background < shallow && shallow < text, "{shallow} {text}");
}

/// On the real GPU stack: a shader that replaces its text draws the glyphs in a layer
/// of its own, and a placement under the text must not cover them, so the under passes
/// go before the effects. An opaque swatch under a replaced word leaves its glyphs as
/// bright as native text, where the same swatch over the text hides them.
#[test]
#[ignore = "requires a hardware GPU; run serialized"]
fn under_placements_stay_under_a_replacement_shaders_text() {
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
    let shader = Shader::compile(
        "copy",
        "fn effect(p: vec2f) -> vec4f { return sampleText(p); }",
    )
    .unwrap();
    crate::text_effect::prewarm(&shader).unwrap();
    let uniforms = shader.uniforms(&Default::default()).unwrap();
    let text = "Readable Text";
    let size = Size::new(520.0, 180.0);
    let viewport = Rectangle::with_size(size);
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(36.0));
    const SWATCH: Color = Color::from_rgb(0.15, 0.1, 0.45);
    let mut capture = |replaced: bool, z: Option<i32>| {
        let mut line = if replaced {
            (*decorated(
                text,
                0..text.len(),
                Arc::new(ShaderEffect {
                    pane: false,
                    scale: Default::default(),
                    capture_scale: Default::default(),
                    fade_in_ms: 0,
                    fade_out_ms: 0,
                    shader: shader.clone(),
                    uniforms: uniforms.clone(),
                    replace: true,
                    hold: true,
                    animated: true,
                }),
            ))
            .clone()
        } else {
            StyledLine::from_styled_runs(&[(text, Style::DEFAULT, None)], Style::DEFAULT)
        };
        if let Some(effects) = line.decorations.as_mut() {
            Arc::make_mut(effects)[0].started = Instant::now() - Duration::from_secs(3);
        }
        if let Some(z) = z {
            line.objects = Some(Arc::new(vec![anchor(0, z, 400, Arc::new(()))]));
        }
        let buffer = RefCell::new(TerminalBuffer::new());
        buffer.borrow_mut().push_line(Arc::new(line));
        let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, iced_wgpu::Renderer>(
            move |_, _| {
                Some(iced::Element::new(Swatch(
                    Size::new(400.0, row_h),
                    SWATCH,
                    false,
                )))
            },
        );
        let mut pane = TerminalPane::<()>::new(buffer.borrow(), Rc::default())
            .font_size(Some(36.0))
            .inline_widgets(Some(resolver));
        type GpuPane<'a> = dyn Widget<(), smudgy_theme::Theme, iced_wgpu::Renderer> + 'a;
        let mut tree = Tree::new(&pane as &GpuPane<'_>);
        let widget = &mut pane as &mut GpuPane<'_>;
        let node = widget.layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
        // Two frames: the first schedules the replacement, the second draws it.
        let mut shot = Vec::new();
        for _ in 0..2 {
            renderer.reset(viewport);
            widget.draw(
                &tree,
                &mut renderer,
                &smudgy_theme::smudgy(),
                &renderer::Style::default(),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            shot = Headless::screenshot(
                &mut renderer,
                Size::new(520, 180),
                1.0,
                crate::prefs::current().palette.background,
            );
        }
        shot
    };
    let bright = |image: &[u8]| {
        image
            .chunks_exact(4)
            .filter(|p| p[..3].iter().all(|c| *c > 160))
            .count()
    };
    let swatched = |image: &[u8]| {
        image
            .chunks_exact(4)
            .filter(|p| {
                p[0].abs_diff((SWATCH.r * 255.0) as u8) < 8
                    && p[2].abs_diff((SWATCH.b * 255.0) as u8) < 8
            })
            .count()
    };
    let plain = capture(false, None);
    let covered = capture(false, Some(0));
    let under = capture(true, Some(-1));
    assert!(bright(&plain) > 200, "glyphs: {}", bright(&plain));
    assert!(
        bright(&covered) < bright(&plain) / 10,
        "the swatch over the text hides it: {} of {}",
        bright(&covered),
        bright(&plain)
    );
    assert!(
        swatched(&under) > 1000,
        "the swatch under the text is drawn: {}",
        swatched(&under)
    );
    assert!(
        bright(&under) > bright(&plain) * 8 / 10,
        "the shader's glyphs stay above the swatch: {} of {}",
        bright(&under),
        bright(&plain)
    );
}

/// A terminal leaves its cursor on an image's last row, and the riser puts the row's
/// text there: an anchor made on that row, before or after the image, hangs from that
/// last row, not from the image's top two rows up.
#[test]
fn anchors_beside_a_tall_last_row_object_hang_from_its_last_row() {
    let renderer = painter();
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(16.0));
    let size = Size::new(500.0, 400.0);
    let text = "ab img cd";
    let mut image = smudgy_session_model::inline_content::InlineObject::new(
        3..6,
        Arc::new(()),
        InlineOwner::default(),
    );
    image.baseline = Baseline::LastRow;
    let resolver =
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |object, _| {
            let (width, height) = if object.range.is_empty() {
                (10.0, row_h)
            } else {
                (40.0, 3.0 * row_h)
            };
            Some(iced::widget::space().width(width).height(height).into())
        });
    let row = lay_out_row(
        &renderer,
        text,
        vec![
            image,
            anchor(6, 0, 10, Arc::new(())),
            anchor(0, 0, 10, Arc::new(())),
        ],
        Some(resolver),
        size,
    );
    assert!(
        (row.row_height - 3.0 * row_h).abs() < 0.01,
        "{}",
        row.row_height
    );
    let [Some(image), Some(after), Some(before)] = row.boxes[..] else {
        panic!("{:?}", row.boxes);
    };
    let top = size.height - 3.0 * row_h;
    assert!(
        (image.y - top).abs() < 0.5,
        "the image at the row's top: {image:?}"
    );
    let last_row = top + 2.0 * row_h;
    assert!(
        (after.y - last_row).abs() < 0.5 && (before.y - last_row).abs() < 0.5,
        "anchors on the last text row ({last_row}): {after:?} {before:?}"
    );
    assert!(
        (after.x - (3.0 * row.advance + 40.0)).abs() < 0.5,
        "{after:?}"
    );
    assert!(before.x.abs() < 0.5, "{before:?}");
}

/// Lay out `text` with an anchor at byte `at` and the same text without one: the glyph
/// x of every character of each, in layout order, with the anchor's box.
fn rtl_glyphs(text: &str, at: usize) -> (Vec<f32>, Vec<f32>, Rectangle, f32) {
    let renderer = painter();
    let size = Size::new(500.0, 400.0);
    let plain = lay_out_row(&renderer, text, Vec::new(), None, size);
    let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_, _| {
        Some(iced::widget::space().width(10.0).height(10.0).into())
    });
    let anchored = lay_out_row(
        &renderer,
        text,
        vec![anchor(at, 0, 10, Arc::new(()))],
        Some(resolver),
        size,
    );
    assert_eq!(anchored.rendered_len, text.len() + 1);
    let [Some(bounds)] = anchored.boxes[..] else {
        panic!("{:?}", anchored.boxes);
    };
    (plain.glyphs, anchored.glyphs, bounds, anchored.advance)
}

/// The marker is bidi-neutral: an anchor between two Hebrew words leaves every glyph of
/// the line where it was (a strong left-to-right marker would part the run and swap the
/// words), and its box lands where the word after it begins: the run's left edge, as the
/// words read right to left.
#[test]
fn an_anchor_between_right_to_left_words_leaves_their_glyphs_in_place() {
    let text = "abc שלום עולם def";
    let (plain, anchored, bounds, advance) = rtl_glyphs(text, text.find('ע').unwrap());
    assert_eq!(anchored.len(), plain.len());
    for (with, without) in anchored.iter().zip(&plain) {
        assert!(
            (with - without).abs() < 0.01,
            "a glyph moved: {anchored:?} against {plain:?}"
        );
    }
    assert!((bounds.x - 4.0 * advance).abs() < 0.5, "{bounds:?}");
}

/// cosmic-text lays out the shaping runs of one word in logical order, and the marker is
/// a run of its own: an anchor inside a right-to-left word still misorders its letters.
/// Pins what the marker cannot fix; passes once cosmic-text orders a word's runs.
#[test]
#[ignore = "cosmic-text orders a word's shaping runs logically: an anchor inside a right-to-left word misorders it"]
fn an_anchor_inside_a_right_to_left_word_leaves_its_glyphs_in_place() {
    let text = "abc שלום def";
    let (plain, anchored, _, _) = rtl_glyphs(text, text.find('ל').unwrap());
    assert_eq!(anchored.len(), plain.len());
    for (with, without) in anchored.iter().zip(&plain) {
        assert!(
            (with - without).abs() < 0.01,
            "a glyph moved: {anchored:?} against {plain:?}"
        );
    }
}

/// The marker goes with the word after it: an anchor at the start of a word that wraps
/// wraps with it, to the next visual line's start, as a terminal's cursor would.
#[test]
fn an_anchor_before_a_wrapping_word_wraps_with_it() {
    let renderer = painter();
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(16.0));
    let advance = lay_out_row(&renderer, "x", Vec::new(), None, Size::new(500.0, 400.0)).advance;
    // Six cells and a half: "aaaa " fits, "bbb" after it does not.
    let size = Size::new(6.5 * advance, 400.0);
    let resolver = crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_, _| {
        Some(iced::widget::space().width(10.0).height(10.0).into())
    });
    let row = lay_out_row(
        &renderer,
        "aaaa bbb",
        vec![anchor(5, 0, 10, Arc::new(()))],
        Some(resolver),
        size,
    );
    assert_eq!(row.baselines.len(), 2, "wrapped");
    let [Some(bounds)] = row.boxes[..] else {
        panic!("{:?}", row.boxes);
    };
    assert!(
        bounds.x.abs() < 0.5 && (bounds.y - (size.height - row_h)).abs() < 0.5,
        "at the second line's start: {bounds:?}"
    );
}

/// A zero-width marker always fits the line it follows, so one whose cell does not (the
/// text before it fills the row) is left at that line's end by the paragraph; the anchor
/// hangs instead from the start of the next visual line, where the text after it went
/// and where a terminal's cursor goes after wrapping. With room for its cell it stays;
/// with nothing after it on a later line, too.
#[test]
fn an_anchor_whose_cell_does_not_fit_its_line_hangs_from_the_next_lines_start() {
    let renderer = painter();
    let (_, row_h) = effective_metrics(&crate::prefs::current(), Some(16.0));
    let advance = lay_out_row(&renderer, "x", Vec::new(), None, Size::new(500.0, 400.0)).advance;
    // Ten cells and a half.
    let size = Size::new(10.5 * advance, 400.0);
    let resolver = || {
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(|_, _| {
            Some(iced::widget::space().width(20.0).height(10.0).into())
        })
    };
    let full = lay_out_row(
        &renderer,
        "aaaaaaaaaabbb",
        vec![anchor(10, 0, 20, Arc::new(()))],
        Some(resolver()),
        size,
    );
    assert_eq!(full.baselines.len(), 2, "wrapped by glyph");
    let [Some(bounds)] = full.boxes[..] else {
        panic!("{:?}", full.boxes);
    };
    assert!(
        bounds.x.abs() < 0.5 && (bounds.y - (size.height - row_h)).abs() < 0.5,
        "at the second line's start: {bounds:?}"
    );

    let room = lay_out_row(
        &renderer,
        "aaaaaaaaabbb",
        vec![anchor(9, 0, 20, Arc::new(()))],
        Some(resolver()),
        size,
    );
    assert_eq!(room.baselines.len(), 2);
    let [Some(bounds)] = room.boxes[..] else {
        panic!("{:?}", room.boxes);
    };
    assert!(
        (bounds.x - 9.0 * advance).abs() < 0.5
            && (bounds.y - (size.height - 2.0 * row_h)).abs() < 0.5,
        "its cell fits the first line: {bounds:?}"
    );

    let last = lay_out_row(
        &renderer,
        "aaaaaaaaaa",
        vec![anchor(10, 0, 20, Arc::new(()))],
        Some(resolver()),
        size,
    );
    assert_eq!(last.baselines.len(), 1);
    let [Some(bounds)] = last.boxes[..] else {
        panic!("{:?}", last.boxes);
    };
    assert!(
        (bounds.x - 10.0 * advance).abs() < 0.5 && (bounds.y - (size.height - row_h)).abs() < 0.5,
        "nothing follows: it stays at the line's end: {bounds:?}"
    );
}

/// A terminal graphics placement with a relative placement `rows` rows below it, as the
/// protocol's `P`/`Q` keys make one.
fn placement_with_child(rows: i32) -> Arc<smudgy_protocol::graphics::Placement> {
    use smudgy_protocol::graphics::{ImageRecord, Placement, RelativeOffset};
    let parent = placement_of(1);
    let mut child = Placement::new(Arc::new(ImageRecord::new(2, 0, 1, 1)), 0, parent.geometry);
    child.anchored = true;
    child.relative = Some(RelativeOffset { columns: 0, rows });
    parent.children.write().unwrap().push(Arc::new(child));
    parent
}

/// A placement's widget draws its relative placements, which may lie rows away from it:
/// the host culls the family by the parent's box grown to reach every child's, so a child
/// in view keeps the family drawing after the parent's own box has left the viewport.
#[test]
fn a_placements_children_keep_it_drawing_after_its_own_box_leaves_the_viewport() {
    const PARENT: Color = Color::from_rgb(1.0, 0.0, 1.0);
    let (_, row_h) = effective_metrics(&crate::prefs::current(), None);
    let size = Size::new(500.0, 200.0);
    // Four rows at the pane's bottom; only the last one is in view, three rows below
    // the placement's.
    let viewport = Rectangle::new(
        Point::new(0.0, size.height - row_h),
        Size::new(500.0, row_h),
    );
    let drawn = |placement: Arc<smudgy_protocol::graphics::Placement>| {
        let outset = u16::try_from(row_h.ceil() as i64).unwrap();
        let mut first =
            StyledLine::from_styled_runs(&[("parent", Style::DEFAULT, None)], Style::DEFAULT);
        first.objects = Some(Arc::new(vec![anchor(0, 0, outset, placement)]));
        let lines = vec![
            first,
            StyledLine::from_output_str("one"),
            StyledLine::from_output_str("two"),
            StyledLine::from_output_str("three"),
        ];
        let resolver =
            crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_, _| {
                Some(iced::Element::new(Swatch(
                    Size::new(10.0, row_h),
                    PARENT,
                    false,
                )))
            });
        let mut renderer = draw_pane(lines, resolver, size, viewport);
        quad_colors(&mut renderer)
            .iter()
            .any(|(_, color)| *color == PARENT)
    };
    assert!(!drawn(placement_of(1)), "alone, its box is out of view");
    assert!(drawn(placement_with_child(3)), "its child is in view");
}

/// A widget that counts the operations and overlay requests reaching it.
struct Probe(Rc<Cell<(u32, u32)>>);

impl Widget<(), smudgy_theme::Theme, Painter> for Probe {
    fn size(&self) -> Size<iced::Length> {
        Size::new(iced::Length::Fixed(10.0), iced::Length::Fixed(10.0))
    }
    fn layout(&mut self, _: &mut Tree, _: &Painter, _: &layout::Limits) -> layout::Node {
        layout::Node::new(Size::new(10.0, 10.0))
    }
    fn draw(
        &self,
        _: &Tree,
        _: &mut Painter,
        _: &smudgy_theme::Theme,
        _: &renderer::Style,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
    }
    fn operate(
        &mut self,
        _: &mut Tree,
        _: Layout<'_>,
        _: &Painter,
        _: &mut dyn advanced::widget::Operation,
    ) {
        let (operated, overlaid) = self.0.get();
        self.0.set((operated + 1, overlaid));
    }
    fn overlay<'b>(
        &'b mut self,
        _: &'b mut Tree,
        _: Layout<'_>,
        _: &Painter,
        _: &Rectangle,
        _: iced::Vector,
    ) -> Option<advanced::overlay::Element<'b, (), smudgy_theme::Theme, Painter>> {
        let (operated, overlaid) = self.0.get();
        self.0.set((operated, overlaid + 1));
        None
    }
}

/// An anchor inside another object's text has nowhere to be: the layout does not
/// position it, and operations and overlay requests pass it by, as drawing does.
#[test]
fn operations_and_overlays_skip_an_unpositioned_mount() {
    struct Nothing;
    impl advanced::widget::Operation for Nothing {
        fn traverse(&mut self, _: &mut dyn FnMut(&mut dyn advanced::widget::Operation)) {}
    }
    let renderer = painter();
    let counts = Rc::new(Cell::new((0, 0)));
    let text = "xxwidgetyy";
    let mut source = StyledLine::from_styled_runs(&[(text, Style::DEFAULT, None)], Style::DEFAULT);
    source.objects = Some(Arc::new(vec![
        smudgy_session_model::inline_content::InlineObject::new(
            2..8,
            Arc::new(()),
            InlineOwner::default(),
        ),
        anchor(5, 0, 10, Arc::new(())),
        anchor(8, 0, 10, Arc::new(())),
    ]));
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(Arc::new(source));
    let probes = counts.clone();
    let resolver =
        crate::inline_object::resolver::<(), smudgy_theme::Theme, Painter>(move |_, _| {
            Some(iced::Element::new(Probe(probes.clone())))
        });
    let mut pane = TerminalPane::new(buffer.borrow(), Rc::default()).inline_widgets(Some(resolver));
    let mut tree = tree(&pane);
    let size = Size::new(500.0, 200.0);
    let node = layout(&mut pane, &mut tree, &renderer, size);
    let boxes = crate::inline_object::Host::<(), smudgy_theme::Theme, Painter>::get(&tree)
        .unwrap()
        .boxes();
    assert_eq!(
        boxes.iter().filter(|(_, bounds)| bounds.is_none()).count(),
        1
    );
    <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::operate(
        &mut pane,
        &mut tree,
        Layout::new(&node),
        &renderer,
        &mut Nothing,
    );
    let overlay = <TerminalPane<'_, ()> as Widget<(), smudgy_theme::Theme, Painter>>::overlay(
        &mut pane,
        &mut tree,
        Layout::new(&node),
        &renderer,
        &Rectangle::with_size(size),
        iced::Vector::ZERO,
    );
    assert!(overlay.is_none());
    assert_eq!(counts.get(), (2, 2), "the two positioned mounts");
}
