//! Catalogue resources across isolate generations.
use super::*;

#[tokio::test]
#[ignore = "32 real script reloads; run alone with --ignored --test-threads=1"]
async fn effects_reload_soak_retires_every_previous_generation() {
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (_home, _root, params) = session_fixture();
    let base = smudgy_core::get_smudgy_home()
        .unwrap()
        .join("InlineWidgets/modules/inline.tsx");
    std::fs::write(
        base,
        r#"
import {echo} from "smudgy:core";
import {Span,Container,createWidget} from "smudgy:widgets";
import {Effects} from "@text-effects/all";
for(const [name,Effect] of Object.entries(Effects)) {
 echo(<Effect duration={1000}><Span fontStyle="italic">SOAK_EFFECT:{name}</Span></Effect>);
 createWidget(`soak-${name}`,<Container><Effect>{name}</Effect></Container>);
}
echo("SOAK_READY");
"#,
    )
    .unwrap();
    let mut events = Box::pin(spawn_with_package_provider(
        params,
        effects_package_provider(),
    ));
    let mut tx = None;
    let mut previous = Vec::<smudgy_session_model::inline_content::InlineOwner>::new();
    let mut current = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_mins(3);
    let mut generations = 0;
    while generations < 32 {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .expect("reload soak deadline")
            .expect("session closed");
        match event.event {
            SessionEvent::RuntimeReady(sender) => tx = Some(sender),
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        if line.text.starts_with("SOAK_EFFECT:")
                            && let Some(effects) = &line.decorations
                        {
                            current.extend(effects.iter().map(|e| e.owner.clone()));
                        }
                        if line.text == "SOAK_READY" {
                            // Reload replays retained transcript rows too; those carry
                            // retired owners and are not part of the new generation.
                            current
                                .retain(smudgy_session_model::inline_content::InlineOwner::active);
                            assert!(
                                previous.iter().all(|owner| !owner.active()),
                                "reload must retire the prior generation"
                            );
                            assert!(
                                current.len() == effect_component_names().len(),
                                "all presets load after every rebuild"
                            );
                            assert!(
                                current
                                    .iter()
                                    .all(smudgy_session_model::inline_content::InlineOwner::active)
                            );
                            previous = std::mem::take(&mut current);
                            generations += 1;
                            if generations < 32 {
                                tx.as_ref().unwrap().send(RuntimeAction::Reload).unwrap();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    drop(events);
    join_fixture_runtime().await;
}
