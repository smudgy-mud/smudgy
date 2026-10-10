//! Catalogue presets preserve native text, typography and current/stored line edits.
use super::*;

#[tokio::test]
async fn presets_preserve_text_typography_and_line_edits() {
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (_home, root, params) = session_fixture();
    let mut events = Box::pin(spawn_with_package_provider(
        params,
        effects_package_provider(),
    ));
    let mut tx = None;
    let mut lines = Vec::new();
    let mut history = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .expect("catalogue transcript deadline")
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
                        if line.text == "CATALOGUE_READY" {
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
                if lines
                    .iter()
                    .any(|line| line.text.starts_with("HISTORY_TEXT:"))
                {
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
    drop(events);
    join_fixture_runtime().await;
}

fn assert_transcript(lines: &[Arc<StyledLine>]) {
    let line = |text: &str| lines.iter().find(|line| line.text == text).unwrap();
    let settings = |text: &str| &line(text).decorations.as_ref().unwrap()[0].effect;
    assert_eq!(
        line("before café after").decorations.as_ref().unwrap()[0].range,
        7..12
    );
    assert!(
        settings("builder preset")
            .shader
            .shader
            .label
            .ends_with("fire.wgsl")
    );
    let scaled = settings("scaled intro");
    assert_eq!(scaled.shader.scale.get().to_bits(), 2.0_f32.to_bits());
    assert_eq!(scaled.outset, 176);
    assert_eq!(scaled.duration_ms, 500);
    assert_eq!(
        (scaled.shader.fade_in_ms, scaled.shader.fade_out_ms),
        (400, 400)
    );
    assert_eq!(settings("continuous smoke").duration_ms, 0);
    assert!(!settings("smoke intro").shader.hold);
    assert!(settings("held frost").shader.hold);
    let emphasis = settings("emphasis");
    assert!(emphasis.shader.pane);
    assert_eq!(
        emphasis.shader.capture_scale.get().to_bits(),
        4.0_f32.to_bits()
    );
    let font = &line("emphasis").fonts.as_ref().unwrap()[0].options;
    assert_eq!(font.font_size, Some(24));
    assert_eq!(font.font_face.as_deref(), Some("serif"));
    assert_eq!(font.font_weight, Some(700));
    assert_eq!(
        font.font_style,
        Some(smudgy_session_model::inline_content::InlineFontStyle::Italic)
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.text == "EXPECTED_ERROR")
            .count(),
        14
    );
    assert!(
        !lines
            .iter()
            .any(|line| ["MISSED_ERROR", "WRONG_ERROR"].contains(&line.text.as_str()))
    );
    assert!(lines.iter().any(|line| line.text == "BURST_QUEUE_OK"));
    assert_effect_edits(lines);
}

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
            assert!(effect.effect.shader.shader.label.ends_with("fire.wgsl"));
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
