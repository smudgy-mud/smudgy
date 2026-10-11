//! Ordered output, native controls, inline layout and line-edit scenarios.
use super::*;

#[tokio::test]
// Keep the ordered script actions and their native transcript checks in one scenario.
#[allow(clippy::too_many_lines)]
async fn jsx_elements_work_as_widgets_and_ordered_terminal_content() {
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (_home, root, params) = session_fixture();
    let mut events = Box::pin(spawn_with_package_provider(
        params,
        fixture_package_provider(),
    ));
    let mut tx = None;
    let mut lines = Vec::new();
    let mut history = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timeout; transcript: {:?}",
                    lines
                        .iter()
                        .map(|l: &Arc<StyledLine>| &l.text)
                        .collect::<Vec<_>>()
                )
            })
            .expect("session closed");
        match event.event {
            SessionEvent::RuntimeReady(sender) => tx = Some(sender),
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        lines.push(line.clone());
                        if line.text == "history foo + foo" {
                            history = Some(line.clone());
                        }
                        if line.text == "INLINE_READY" {
                            let sender = tx.as_ref().unwrap();
                            for text in [
                                "replace foo + foo",
                                "history foo + foo",
                                "edit-history",
                                "verify-history",
                            ] {
                                sender
                                    .send(RuntimeAction::HandleIncomingLine(Arc::new(
                                        StyledLine::from_output_str(text),
                                    )))
                                    .unwrap();
                            }
                            sender.send(RuntimeAction::RequestRepaint).unwrap();
                        }
                    }
                }
                if lines.iter().any(|l| l.text.starts_with("HISTORY_TEXT:")) {
                    lines.push(history.take().expect("edited history row"));
                    break;
                }
            }
            SessionEvent::PerformLineOperation { operation, .. } => {
                history = Some(operation.apply(history.as_ref().expect("history row")));
            }
            _ => {}
        }
    }
    let _mounted_view = root.view(
        |_| true,
        || Box::new(|_| iced::widget::container::Style::default()),
    );
    assert_transcript(&lines);
    for message in exercise_native_controls(&lines)
        .into_iter()
        .chain(exercise_native_slider(&lines))
    {
        if let smudgy_widgets::WidgetMessage::InvokeCallback {
            callback,
            isolate,
            args,
        } = message
        {
            let (isolate, instance) =
                smudgy_core::session::runtime::IsolateId::from_widget_token(&isolate.0);
            tx.as_ref()
                .unwrap()
                .send(RuntimeAction::ExecuteWidgetCallback {
                    isolate,
                    instance,
                    function: callback,
                    args,
                })
                .unwrap();
        }
    }
    tx.as_ref()
        .unwrap()
        .send(RuntimeAction::RequestRepaint)
        .unwrap();
    let mut clicked = std::collections::BTreeSet::new();
    let expected_callbacks = [
        "BUTTON_CLICKED",
        "PANEL_CLICKED",
        "SLIDER_VALUE:number:500",
        "SLIDER_VALUE:number:0",
        "SLIDER_VALUE:number:2000",
        "SLIDER_VALUE:number:751",
        "SLIDER_RELEASE:751",
    ];
    while clicked.len() < expected_callbacks.len() {
        let event = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .unwrap()
            .unwrap();
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update
                    && expected_callbacks.contains(&line.text.as_str())
                {
                    clicked.insert(line.text.clone());
                }
            }
        }
    }
    drop(events);
    join_fixture_runtime().await;
}

// The exhaustive preset roster is intentionally checked against the same transcript.
#[allow(clippy::too_many_lines)]
fn assert_transcript(lines: &[Arc<StyledLine>]) {
    let projection = lines
        .iter()
        .find(|line| line.text == "x".repeat(1023))
        .expect("default widget projection truncates before a surrogate pair");
    assert_eq!(projection.objects.as_ref().unwrap()[0].range, 0..1023);
    let find = |text: &str| {
        lines
            .iter()
            .find(|l| l.text == text)
            .unwrap_or_else(|| panic!("missing {text:?}: {lines:?}"))
    };
    for text in ["builder span", "builder effect"] {
        assert_eq!(
            find(text).fonts.as_ref().unwrap()[0].options.font_size,
            Some(20)
        );
        assert!(find(text).objects.is_none());
    }
    for text in ["builder effect", "builder preset"] {
        assert!(
            find(text)
                .decorations
                .as_ref()
                .is_some_and(|effects| !effects.is_empty())
        );
    }
    assert!(find("explicit child").objects.is_none());
    assert!(!lines.iter().any(|line| line.text == "ignored child prop"));
    assert_eq!(
        find("builder button").objects.as_ref().unwrap()[0].range,
        0..14
    );
    assert_eq!(
        find("nested styled projection").objects.as_ref().unwrap()[0].range,
        0..24,
        "atomic containers retain their styled children's copy/search text"
    );
    let equivalent: Vec<_> = lines
        .iter()
        .filter(|line| line.text == "same text")
        .collect();
    assert_eq!(equivalent.len(), 2);
    assert_eq!(equivalent[0].spans, equivalent[1].spans);
    assert_eq!(equivalent[0].fonts, equivalent[1].fonts);
    for line in &equivalent {
        assert!(
            line.objects.is_none(),
            "text effects must not mount a widget capture"
        );
        let effect = &line.decorations.as_ref().unwrap()[0];
        assert_eq!(effect.range, 0..9);
        assert!(effect.effect.shader.shader.label.ends_with("copy.wgsl"));
    }
    assert!(find("flame 7 lit").objects.is_none());
    let rich = find("styled runs");
    assert!(rich.objects.is_none());
    let fonts = rich.fonts.as_ref().unwrap();
    assert_eq!(fonts.len(), 2);
    assert_eq!(fonts[0].range, 0..7);
    assert_eq!(fonts[1].range, 7..11);
    for font in fonts.iter() {
        assert_eq!(font.options.font_face.as_deref(), Some("serif"));
        assert_eq!(
            font.options.font_style,
            Some(smudgy_session_model::inline_content::InlineFontStyle::Italic)
        );
        assert_eq!(font.options.font_size, Some(22));
    }
    assert_eq!(fonts[1].options.font_weight, Some(700));
    let button = find("button: Click me after");
    assert_eq!(button.objects.as_ref().unwrap()[0].range, 8..16);
    assert_eq!(find("Status panel").objects.as_ref().unwrap().len(), 1);
    assert_eq!(
        find("red pulse").objects.as_ref().unwrap()[0].paint_outset,
        u16::MAX
    );
    let attention: Vec<_> = lines
        .iter()
        .filter(|line| line.text == "Attention starts here [red pulse]")
        .collect();
    assert_eq!(attention.len(), 2, "saved template strings can be reused");
    let first = &attention[0].objects.as_ref().unwrap()[0];
    let second = &attention[1].objects.as_ref().unwrap()[0];
    assert_eq!(first.range, 22..33);
    assert_eq!(first.paint_outset, u16::MAX);
    assert_ne!(first.id, second.id);
    let tagged = find("tagged hot end");
    assert_eq!(tagged.decorations.as_ref().unwrap()[0].range, 7..10);
    let font = &tagged.fonts.as_ref().unwrap()[0];
    assert_eq!(font.range, 7..10);
    assert_eq!(font.options.font_size, Some(24));
    assert_eq!(font.options.font_weight, Some(700));
    assert_eq!(
        font.options.font_style,
        Some(smudgy_session_model::inline_content::InlineFontStyle::Italic)
    );
    assert_eq!(font.options.font_face.as_deref(), Some("monospace"));
    let nested = find("large small").fonts.as_ref().unwrap();
    assert_eq!(nested[0].range, 0..6);
    assert_eq!(nested[0].options.font_size, Some(30));
    assert_eq!(nested[1].range, 6..11);
    assert_eq!(nested[1].options.font_size, Some(10));
    assert_eq!(nested[1].options.font_weight, Some(700));
    let combined = find("before café after");
    let effect = &combined.decorations.as_ref().unwrap()[0];
    assert_eq!(effect.range, 7..12);
    assert!(effect.effect.shader.shader.label.ends_with("copy.wgsl"));
    assert_eq!(
        find("attention").decorations.as_ref().unwrap()[0]
            .effect
            .duration_ms,
        1200
    );
    assert_eq!(find("north").links.len(), 1);
    assert!(matches!(
        find("inherited").spans[0].style.fg,
        smudgy_session_model::Color::Ansi {
            color: smudgy_session_model::AnsiColor::Blue,
            ..
        }
    ));
    assert_ne!(effect.id, find("café").decorations.as_ref().unwrap()[0].id);
    let intro = &find("scaled intro").decorations.as_ref().unwrap()[0];
    let settings = &intro.effect.shader;
    assert_eq!(settings.scale.get().to_bits(), 2.0_f32.to_bits());
    assert_eq!(intro.effect.outset, 16);
    assert_eq!(intro.effect.duration_ms, 500);
    assert_eq!(settings.fade_in_ms, 400);
    assert_eq!(settings.fade_out_ms, 400);
    assert!(
        intro.animated(intro.started + Duration::from_millis(250)),
        "speed zero still has a timed lifecycle"
    );
    assert!(
        intro
            .elapsed(intro.started + Duration::from_millis(500))
            .is_none()
    );
    assert_eq!(
        find("continuous smoke").decorations.as_ref().unwrap()[0]
            .effect
            .duration_ms,
        0
    );
    let smoke = &find("smoke intro").decorations.as_ref().unwrap()[0];
    assert!(!(smoke.effect.shader).hold && (smoke.effect.shader).fade_out_ms > 0);
    find("SHADER_DIAGNOSTIC_OK");
    find("custom shader");
    assert_effect_edits(lines);
    find("HISTORY_TEXT:!焔 café + café");
    assert_eq!(
        lines.iter().filter(|l| l.text == "EXPECTED_ERROR").count(),
        23
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.text == "MISSED_ERROR" || l.text == "WRONG_ERROR")
    );
}

/// Current-line and stored-line edits must produce the same selectable text,
/// fonts, links and effects, including after subsequent splices and restyling.
fn assert_effect_edits(lines: &[Arc<StyledLine>]) {
    use smudgy_session_model::{AnsiColor, Color, inline_content::InlineFontStyle};
    let edited: Vec<_> = lines
        .iter()
        .filter(|line| line.text == "!焔 café + café")
        .collect();
    assert_eq!(edited.len(), 2, "current and stored lines were both edited");
    let mut effect_ids = std::collections::BTreeSet::new();
    for line in edited {
        assert!(
            line.objects.is_none(),
            "text effects do not mount widget objects"
        );
        let mut effects: Vec<_> = line.decorations.as_ref().unwrap().iter().collect();
        effects.sort_by_key(|effect| effect.range.start);
        assert_eq!(effects.len(), 3, "overwritten effects must be removed");
        for (effect, range) in effects.iter().zip([1..4, 5..10, 13..18]) {
            assert_eq!(effect.range, range);
            assert!(effect.effect.shader.shader.label.ends_with("copy.wgsl"));
            assert!(
                effect_ids.insert(effect.id),
                "each occurrence has its own effect instance"
            );
        }
        let fonts = line.fonts.as_ref().unwrap();
        assert_eq!(fonts.len(), 4, "overwritten font metadata must be removed");
        assert_eq!(fonts[0].range, 0..1);
        assert_eq!(fonts[0].options.font_size, Some(30));
        for (font, range) in fonts[1..].iter().zip([1..4, 5..10, 13..18]) {
            assert_eq!(font.range, range);
            assert_eq!(font.options.font_size, Some(24));
            assert_eq!(font.options.font_face.as_deref(), Some("serif"));
            assert_eq!(font.options.font_style, Some(InlineFontStyle::Italic));
            assert_eq!(font.options.font_weight, Some(700));
        }
        assert_eq!(line.links.len(), 3);
        for (link, range) in line.links.iter().zip([1..4, 5..10, 13..18]) {
            assert_eq!(link.begin_pos..link.end_pos, range);
        }
        for (offset, expected) in [(5, AnsiColor::Blue), (13, AnsiColor::Red)] {
            let span = line
                .spans
                .iter()
                .find(|span| span.begin_pos <= offset && offset < span.end_pos)
                .unwrap();
            assert!(matches!(span.style.fg, Color::Ansi { color, .. } if color == expected));
        }
    }
}

/// A real renderer and widget event loop, with the native elements created by JSX.
/// Traverse the ordinary widget tree to locate labels instead of hard-coding pixels.
fn exercise_native_controls(lines: &[Arc<StyledLine>]) -> Vec<smudgy_widgets::WidgetMessage> {
    use smudgy_ui_shared::{
        split_terminal_pane::{ScrolledLayout, TerminalViewHandle, split_terminal_pane},
        terminal_buffer::TerminalBuffer,
    };
    let buffer = std::cell::RefCell::new(TerminalBuffer::new());
    for text in [
        "before café after",
        "button: Click me after",
        "Status panel",
        "red pulse",
    ] {
        buffer
            .borrow_mut()
            .push_line(lines.iter().find(|l| l.text == text).unwrap().clone());
    }
    let mut renderer = native_renderer();
    let mut pane = split_terminal_pane(
        buffer.borrow(),
        TerminalViewHandle::default(),
        None,
        None,
        None,
        None,
        ScrolledLayout::FullPane,
        Some(smudgy_ui_shared::inline_object::resolver(
            smudgy_widgets::inline_element,
        )),
    );
    let mut tree = Tree::new(&pane);
    let size = Size::new(640.0, 560.0);
    let node =
        pane.as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    let viewport = Rectangle::with_size(size);
    let mut labels = Labels(std::collections::BTreeMap::default());
    pane.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
    let click = labels
        .0
        .get("Click me")
        .expect("inline button label")
        .center();
    let action = labels.0.get("Action").expect("panel action label").center();
    assert!(viewport.contains(click) && viewport.contains(action));
    let mut messages = Vec::new();
    for point in [click, action] {
        for event in [
            Event::Mouse(mouse::Event::CursorMoved { position: point }),
            Event::Window(iced::window::Event::RedrawRequested(
                iced::time::Instant::now(),
            )),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ] {
            let mut shell = Shell::new(&mut messages);
            pane.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &renderer,
                &mut clipboard::Null,
                &mut shell,
                &viewport,
            );
        }
    }
    assert!(
        pane.as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &viewport,
                Vector::ZERO
            )
            .is_some(),
        "panel tooltip must reach the terminal overlay pass"
    );
    let canvas = lines.iter().find(|l| l.text == "red pulse").unwrap();
    let tick = canvas.objects.as_ref().unwrap()[0].started + Duration::from_millis(500);
    pane.as_widget_mut().update(
        &mut tree,
        &Event::Window(iced::window::Event::RedrawRequested(tick)),
        Layout::new(&node),
        mouse::Cursor::Available(action),
        &renderer,
        &mut clipboard::Null,
        &mut Shell::new(&mut messages),
        &viewport,
    );
    paint_native_scene(&mut pane, &mut tree, &mut renderer, &node, viewport, action);
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, smudgy_widgets::WidgetMessage::InvokeCallback { .. }))
            .count(),
        2
    );
    messages
}

/// Drag the public JSX slider past both endpoints and release at a stepped value.
fn exercise_native_slider(lines: &[Arc<StyledLine>]) -> Vec<smudgy_widgets::WidgetMessage> {
    let line = lines
        .iter()
        .find(|line| line.text == "Duration slider")
        .unwrap();
    let object = &line.objects.as_ref().unwrap()[0];
    let mut slider = smudgy_widgets::inline_element(object).unwrap();
    let renderer = native_renderer();
    let mut tree = Tree::new(&slider);
    let size = Size::new(200.0, 32.0);
    let node =
        slider
            .as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    let bounds = node.bounds();
    let viewport = Rectangle::with_size(size);
    let mut messages = Vec::new();
    for (event, x) in [
        (
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            0.25,
        ),
        (
            Event::Mouse(mouse::Event::CursorMoved {
                position: iced::Point::new(-10.0, bounds.center_y()),
            }),
            -0.05,
        ),
        (
            Event::Mouse(mouse::Event::CursorMoved {
                position: iced::Point::new(210.0, bounds.center_y()),
            }),
            1.05,
        ),
        (
            Event::Mouse(mouse::Event::CursorMoved {
                position: iced::Point::new(75.1, bounds.center_y()),
            }),
            0.3755,
        ),
        (
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            0.3755,
        ),
    ] {
        slider.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(iced::Point::new(
                bounds.x + bounds.width * x,
                bounds.center_y(),
            )),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
    }
    let values: Vec<_> = messages
        .iter()
        .filter_map(|message| {
            if let smudgy_widgets::WidgetMessage::InvokeCallback { args, .. } = message {
                Some(args.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        values,
        [
            vec!["500".to_owned()],
            vec!["0".to_owned()],
            vec!["2000".to_owned()],
            vec!["751".to_owned()],
            vec![]
        ],
        "numeric changes precede one release callback"
    );
    messages
}

fn paint_native_scene(
    pane: &mut iced::Element<
        '_,
        smudgy_widgets::WidgetMessage,
        smudgy_theme::Theme,
        iced::Renderer,
    >,
    tree: &mut Tree,
    renderer: &mut iced::Renderer,
    node: &layout::Node,
    viewport: Rectangle,
    action: iced::Point,
) {
    use iced::advanced::{Renderer as _, renderer};
    let theme = smudgy_theme::smudgy();
    let style = renderer::Style {
        text_color: iced::theme::Base::base(&theme).text_color,
    };
    renderer.reset(viewport);
    pane.as_widget().draw(
        tree,
        renderer,
        &theme,
        &style,
        Layout::new(node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    if let Some(mut tooltip) =
        pane.as_widget_mut()
            .overlay(tree, Layout::new(node), renderer, &viewport, Vector::ZERO)
    {
        let tip_layout = tooltip.as_overlay_mut().layout(renderer, viewport.size());
        renderer.with_layer(viewport, |renderer| {
            tooltip.as_overlay().draw(
                renderer,
                &theme,
                &style,
                Layout::new(&tip_layout),
                mouse::Cursor::Available(action),
            );
        });
    }
    let iced::Renderer::Secondary(renderer) = renderer else {
        unreachable!()
    };
    assert!(
        renderer
            .layers()
            .iter()
            .all(|layer| layer.bounds.intersection(&viewport) == Some(layer.bounds))
    );
    let mut pixels = tiny_skia::Pixmap::new(640, 560).unwrap();
    let mut mask = tiny_skia::Mask::new(640, 560).unwrap();
    renderer.draw(
        &mut pixels.as_mut(),
        &mut mask,
        &iced::advanced::graphics::Viewport::with_physical_size(Size::new(640, 560), 1.0),
        &[viewport],
        iced::Color::from_rgb8(18, 18, 22),
    );
    for pixel in pixels.data_mut().chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let red_pixels = pixels
        .data()
        .chunks_exact(4)
        .filter(|p| p[0] > 45 && p[0] > p[1].saturating_add(20))
        .count();
    assert!(
        red_pixels > 10000,
        "tweened red canvas must paint far beyond its 12px box: {red_pixels}"
    );
}
