//! Native resources and retained UI state across isolate generations.
use super::*;

#[tokio::test]
// Keep creation, retained UI state, reload and shutdown in one ordered scenario.
#[allow(clippy::too_many_lines)]
async fn retained_trees_and_queued_clicks_are_safe_across_reload_and_shutdown() {
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (home, root, params) = session_fixture();
    let modules = home.path().join("InlineWidgets/modules");
    let script = |generation| {
        format!(
            r#"
import {{echo}} from "smudgy:core";
import {{Button,createWidget}} from "smudgy:widgets";
const button=<Button onPress={{() => echo("CALLBACK_LIVE:{generation}")}}>Retained button</Button>;
echo(button);createWidget("retained",button);echo("CALLBACK_READY:{generation}");
"#
        )
    };
    std::fs::write(modules.join("inline.tsx"), script(0)).unwrap();
    let mut events = Box::pin(spawn_with_package_provider(
        params,
        fixture_package_provider(),
    ));
    let mut sender = None;
    let mut current = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap()
            .unwrap();
        match event.event {
            SessionEvent::RuntimeReady(tx) => sender = Some(tx),
            SessionEvent::UpdateBuffer(updates) => {
                let mut ready = false;
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        if line.text == "Retained button" {
                            current = Some(line.clone());
                        }
                        ready |= line.text == "CALLBACK_READY:0";
                    }
                }
                if ready {
                    break;
                }
            }
            _ => {}
        }
    }
    let sender = sender.unwrap();
    let old_line = current.take().unwrap();
    let object = &old_line.objects.as_ref().unwrap()[0];
    let mut old_tree = smudgy_widgets::inline_element(object).unwrap();
    let queued = click_button(&mut old_tree);
    let retained_root = root.view(
        |_| true,
        || Box::new(|_| iced::widget::container::Style::default()),
    );
    std::fs::write(modules.join("inline.tsx"), script(1)).unwrap();
    sender.send(RuntimeAction::Reload).unwrap();
    loop {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap()
            .unwrap();
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            let mut ready = false;
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    if line.text == "Retained button"
                        && line.objects.as_ref().unwrap()[0].owner.active()
                    {
                        current = Some(line.clone());
                    }
                    ready |= line.text == "CALLBACK_READY:1";
                }
            }
            if ready {
                break;
            }
        }
    }
    assert!(!object.owner.active());
    assert!(smudgy_widgets::inline_element(object).is_none());
    // A tree already handed to iced can still emit a press after retirement.
    let late = click_button(&mut old_tree);
    let current = current.unwrap();
    let mut live_tree =
        smudgy_widgets::inline_element(&current.objects.as_ref().unwrap()[0]).unwrap();
    for message in [queued, late, click_button(&mut live_tree)] {
        let smudgy_widgets::WidgetMessage::InvokeCallback {
            callback,
            isolate,
            args,
        } = message
        else {
            panic!("button callback")
        };
        let (isolate, instance) =
            smudgy_core::session::runtime::IsolateId::from_widget_token(&isolate.0);
        sender
            .send(RuntimeAction::ExecuteWidgetCallback {
                isolate,
                instance,
                function: callback,
                args,
            })
            .unwrap();
    }
    loop {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap()
            .unwrap();
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            let mut live = false;
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    assert_ne!(line.text, "CALLBACK_LIVE:0", "stale presses are inert");
                    live |= line.text == "CALLBACK_LIVE:1";
                }
            }
            if live {
                break;
            }
        }
    }
    // Release old native trees while the new isolate is running; hold its tree
    // until after shutdown to cover both orders without transferring V8 handles.
    drop(old_tree);
    drop(retained_root);
    sender.send(RuntimeAction::Shutdown).unwrap();
    drop(events);
    join_fixture_runtime().await;
    drop(live_tree);
}

fn click_button(
    element: &mut iced::Element<
        'static,
        smudgy_widgets::WidgetMessage,
        smudgy_theme::Theme,
        iced::Renderer,
    >,
) -> smudgy_widgets::WidgetMessage {
    let renderer = native_renderer();
    let mut tree = Tree::new(&*element);
    let viewport = Rectangle::with_size(Size::new(400.0, 100.0));
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, viewport.size()),
    );
    let point = Layout::new(&node).bounds().center();
    let mut messages = Vec::new();
    for event in [
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ] {
        element.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
    }
    assert_eq!(messages.len(), 1);
    messages.pop().unwrap()
}
