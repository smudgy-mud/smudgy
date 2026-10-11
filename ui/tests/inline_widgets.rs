//! Exercise real JSX, native widget construction, ordered echo and trigger edits together.
use futures::StreamExt;
use iced::{
    Event, Rectangle, Size, Vector,
    advanced::{Layout, Shell, clipboard, layout, mouse, widget::Tree},
};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionParams, spawn_with_package_provider,
};
use smudgy_session_model::StyledLine;
use std::{sync::Arc, time::Duration};

#[path = "support/inline_widgets.rs"]
mod support;
use support::*;
#[path = "inline_widgets/content.rs"]
mod content;
#[path = "inline_widgets/imports.rs"]
mod imports;
#[path = "inline_widgets/reload.rs"]
mod reload;
fn session_fixture() -> (
    &'static tempfile::TempDir,
    smudgy_widgets::WidgetRoot<'static, smudgy_theme::Theme, iced::Renderer>,
    Arc<SessionParams>,
) {
    support::session_fixture(include_str!("inline_widgets/content.tsx"), &[])
}
fn fixture_package_provider() -> smudgy_core::session::PackageProviderFactory {
    Arc::new(|| std::rc::Rc::new(smudgy_script::InMemoryPackageProvider::new()))
}
