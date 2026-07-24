use std::{
    collections::BTreeMap,
    sync::Arc,
};

use arc_swap::ArcSwap;
use deno_core::v8;
use iced::{
    Subscription,
    futures::{SinkExt, channel::mpsc::Sender},
    widget::container,
};
use smudgy_cloud::WidgetIsolate;
use smudgy_map_widget::map_view;

type ElementFn<'a, Theme, Renderer> =
    Arc<dyn Fn() -> iced::Element<'a, WidgetMessage, Theme, Renderer>>;

/// One mounted widget: its render closure, whether it is currently shown (`enabled = false`
/// hides it without dropping the tree), and which pane hosts it. `target` is the pane's
/// interned name id as a bare integer — this crate is a leaf that cannot name core's pane
/// types, and a `Copy` u32 crosses that boundary more cheaply than a `String` (no clone when
/// the inner map rebuilds on widget mutation). `None` is the untargeted overlay (the session's
/// main pane). Resolution happens at render time by integer matching, so an entry whose target
/// pane doesn't currently exist simply renders nowhere, and a same-name recreated pane
/// re-attaches it (the name id is stable across close/recreate).
struct Entry<'a, Theme, Renderer> {
    enabled: bool,
    element: ElementFn<'a, Theme, Renderer>,
    target: Option<u32>,
}

// Manual `Clone` (not derived): the `element` `Arc` is cloneable for any `Theme`/`Renderer`, so
// `Entry` must not inherit the derive's spurious `Theme: Clone`/`Renderer: Clone` bounds.
impl<Theme, Renderer> Clone for Entry<'_, Theme, Renderer> {
    fn clone(&self) -> Self {
        Self {
            enabled: self.enabled,
            element: self.element.clone(),
            target: self.target,
        }
    }
}

pub struct WidgetRoot<'a, Theme, Renderer> {
    inner: Arc<ArcSwap<Inner<'a, Theme, Renderer>>>,
    tx: Arc<tokio::sync::watch::Sender<()>>,
    /// Map-entry GC queue, shared with every map render closure this root's
    /// widgets hold (see [`crate::map::MapReaper`]). It lives on the root
    /// because the root is the one handle both the build ops (script thread)
    /// and the rendering session store (UI thread) already share.
    map_reaper: crate::map::MapReaper,
}

impl<Theme, Renderer> std::fmt::Debug for WidgetRoot<'_, Theme, Renderer> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WidgetRoot {{ widgets: {:?} }}",
            self.inner.load().elements.keys().collect::<Vec<_>>()
        )
    }
}

#[derive(Debug, Clone)]
pub enum WidgetMessage {
    /// A widget callback (e.g. a button `onPress` or a `Markdown` `onLink`). `callback` is a v8
    /// handle bound to the isolate that built it; `isolate` is that isolate's token so `core` can
    /// dispatch the call back into it instead of always `main`. `args` are positional string
    /// arguments forwarded to the JS function — empty for a no-arg `onPress`, a single clicked URL
    /// for a `Markdown` `onLink`.
    InvokeCallback {
        callback: Arc<v8::Global<v8::Function>>,
        isolate: WidgetIsolate,
        args: Vec<String>,
    },
    /// A widget interaction with no host effect, dropped by the UI. The `Markdown` op contract
    /// allows an absent link handler (links become inert); the `smudgy:widgets` factory always
    /// supplies a default `onLink`, so this is the fallback for direct op use.
    Noop,
    /// A `TextEditor` edit/cursor action. The UI applies `action` to the keyed buffer in the
    /// `TextEditorStore`, then -- if it actually edited text -- invokes `on_change` (in `isolate`)
    /// with the buffer's new full text.
    TextEditorAction {
        key: String,
        action: iced::widget::text_editor::Action,
        on_change: Option<Arc<v8::Global<v8::Function>>>,
        isolate: WidgetIsolate,
    },
    MapMessage {
        id: crate::MapWidgetId,
        message: map_view::Message,
    },
}
unsafe impl Send for WidgetMessage {}
unsafe impl Sync for WidgetMessage {}

/// Widgets are keyed by `(creator, name)` so two packages' `createWidget("hud")` cannot clobber
/// each other (`creator` is the importer's provenance JSON; a package's creator maps 1:1 to its
/// isolate, so the `(IsolateId, Origin, name)` keying collapses to `(creator, name)`, the isolate
/// dimension being redundant). The isolate is threaded separately for callback routing.
#[derive(Clone)]
struct Inner<'a, Theme, Renderer> {
    elements: BTreeMap<(String, String), Entry<'a, Theme, Renderer>>,
}

unsafe impl<Theme, Renderer> Send for WidgetRoot<'_, Theme, Renderer> {}
unsafe impl<Theme, Renderer> Sync for WidgetRoot<'_, Theme, Renderer> {}

impl<Theme, Renderer> Clone for WidgetRoot<'_, Theme, Renderer> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            tx: self.tx.clone(),
            map_reaper: self.map_reaper.clone(),
        }
    }
}

struct HashedArc<T>(pub Arc<T>);

impl<T> std::hash::Hash for HashedArc<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

impl<'a, Theme, Renderer> Default for WidgetRoot<'a, Theme, Renderer>
where
    Theme: 'a,
    Renderer: 'a,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, Theme, Renderer> WidgetRoot<'a, Theme, Renderer>
where
    Theme: 'a,
    Renderer: 'a,
{
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = tokio::sync::watch::channel(());

        Self {
            inner: Arc::new(ArcSwap::from_pointee(Inner {
                elements: BTreeMap::new(),
            })),
            tx: Arc::new(tx),
            map_reaper: crate::map::MapReaper::default(),
        }
    }

    /// The map-entry GC queue shared between this root's build ops and the
    /// session store that renders it.
    #[must_use]
    pub fn map_reaper(&self) -> &crate::map::MapReaper {
        &self.map_reaper
    }

    pub fn subscription<SubMessage: std::hash::Hash + Copy + Send + 'static>(
        &self,
        id: SubMessage,
    ) -> Subscription<SubMessage> {
        Subscription::run_with((id, HashedArc(self.tx.clone())), move |(id, ext_tx)| {
            let ext_tx = ext_tx.0.clone();
            let id = *id;
            iced::stream::channel(1, move |mut ui_tx: Sender<SubMessage>| async move {
                let mut rx = ext_tx.subscribe();
                loop {
                    if rx.changed().await.is_err() {
                        break;
                    }
                    if ui_tx.send(id).await.is_err() {
                        break;
                    }
                }
            })
        })
    }

    /// Upsert a widget under `(creator, name)`. A re-mount (e.g. `widget.update`) preserves the
    /// existing `enabled` state but takes the new `target` — re-mounting a name into a different
    /// pane moves it.
    pub fn insert(
        &self,
        creator: &str,
        name: &str,
        element: ElementFn<'a, Theme, Renderer>,
        target: Option<u32>,
    ) {
        let mut elements = self.inner.load().elements.clone();
        let key = (creator.to_string(), name.to_string());
        let enabled = elements.get(&key).is_none_or(|e| e.enabled);
        elements.insert(
            key,
            Entry {
                enabled,
                element,
                target,
            },
        );
        self.inner.swap(Arc::new(Inner { elements }));
        self.tx.send(()).ok();
    }

    pub fn remove(&self, creator: &str, name: &str) {
        let mut elements = self.inner.load().elements.clone();
        elements.remove(&(creator.to_string(), name.to_string()));
        self.inner.swap(Arc::new(Inner { elements }));
        self.tx.send(()).ok();
    }

    /// Drop every mounted widget, across all creators and panes. The embedder calls this when
    /// the script engine whose isolates minted the mounted callbacks is torn down (an engine
    /// rebuild): every entry's render closure holds `v8::Global` callbacks bound to the dead
    /// isolates, so the widgets it draws can no longer do anything. Reloaded modules re-mount
    /// theirs; dynamically created widgets are gone, as they would never be re-minted. Dropping
    /// a `v8::Global` whose isolate is already disposed is a no-op, so the swap is safe from
    /// any thread once those isolates are down.
    pub fn clear(&self) {
        self.inner.swap(Arc::new(Inner {
            elements: BTreeMap::new(),
        }));
        self.tx.send(()).ok();
    }

    /// Show/hide a widget without dropping its tree. No-op if `(creator, name)` is not mounted.
    pub fn set_enabled(&self, creator: &str, name: &str, enabled: bool) {
        let mut elements = self.inner.load().elements.clone();
        if let Some(entry) = elements.get_mut(&(creator.to_string(), name.to_string())) {
            entry.enabled = enabled;
        }
        self.inner.swap(Arc::new(Inner { elements }));
        self.tx.send(()).ok();
    }

    /// The names of `creator`'s mounted widgets (origin-scoped — a package sees only its own).
    #[must_use]
    pub fn list(&self, creator: &str) -> Vec<String> {
        self.inner
            .load()
            .elements
            .keys()
            .filter(|(c, _)| c == creator)
            .map(|(_, n)| n.clone())
            .collect()
    }

    #[must_use]
    pub fn exists(&self, creator: &str, name: &str) -> bool {
        self.inner
            .load()
            .elements
            .contains_key(&(creator.to_string(), name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestRoot = WidgetRoot<'static, iced::Theme, iced::Renderer>;

    fn element() -> ElementFn<'static, iced::Theme, iced::Renderer> {
        Arc::new(|| iced::widget::text("x").into())
    }

    fn entry_target(root: &TestRoot, creator: &str, name: &str) -> Option<Option<u32>> {
        root.inner
            .load()
            .elements
            .get(&(creator.to_string(), name.to_string()))
            .map(|e| e.target)
    }

    #[test]
    fn remount_updates_target_but_preserves_enabled() {
        let root = TestRoot::new();
        root.insert("c", "hud", element(), None);
        assert_eq!(entry_target(&root, "c", "hud"), Some(None));

        root.set_enabled("c", "hud", false);
        // Re-mounting into a pane moves the widget; the hidden state survives the move.
        root.insert("c", "hud", element(), Some(3));
        assert_eq!(entry_target(&root, "c", "hud"), Some(Some(3)));
        let inner = root.inner.load();
        let entry = &inner.elements[&("c".to_string(), "hud".to_string())];
        assert!(!entry.enabled);

        // ...and back to the untargeted overlay.
        root.insert("c", "hud", element(), None);
        assert_eq!(entry_target(&root, "c", "hud"), Some(None));
    }

    #[test]
    fn clear_drops_every_creator_and_pane() {
        let root = TestRoot::new();
        root.insert("c1", "overlay", element(), None);
        root.insert("c1", "docked", element(), Some(7));
        root.insert("c2", "hud", element(), Some(3));
        root.clear();
        assert!(root.inner.load().elements.is_empty());
        // A post-clear mount starts from defaults (nothing is remembered about the old entry).
        root.insert("c1", "overlay", element(), Some(1));
        assert_eq!(entry_target(&root, "c1", "overlay"), Some(Some(1)));
    }

    #[test]
    fn targets_are_per_entry() {
        let root = TestRoot::new();
        root.insert("c", "overlay", element(), None);
        root.insert("c", "docked", element(), Some(7));
        assert_eq!(entry_target(&root, "c", "overlay"), Some(None));
        assert_eq!(entry_target(&root, "c", "docked"), Some(Some(7)));
        root.remove("c", "docked");
        assert_eq!(entry_target(&root, "c", "docked"), None);
    }
}

impl<'a, Theme, Renderer: iced::advanced::Renderer> WidgetRoot<'a, Theme, Renderer>
where
    Theme: iced::widget::container::Catalog + 'a,
    Renderer: 'a,
{
    /// The stack of enabled entries whose pane target passes `filter`, which receives each
    /// entry's target name id (`None` = untargeted / main overlay). The caller decides what a
    /// pane body shows — this crate has no notion of which ids are live panes.
    pub fn view(
        &self,
        filter: impl Fn(Option<u32>) -> bool,
        class: impl Fn() -> Theme::Class<'a>,
    ) -> iced::Element<'_, WidgetMessage, Theme, Renderer> {
        let inner = self.inner.load();
        let shown: Vec<_> = inner
            .elements
            .iter()
            .filter(|(_, e)| e.enabled && filter(e.target))
            .collect();
        if shown.is_empty() {
            iced::widget::column(vec![]).into()
        } else {
            // Instrumented: with iced's `debug` feature on (smudgy_ui's `iced-debug`),
            // the whole pass and each mounted widget's element build show up as custom
            // spans in comet ("widgets view" / "widget <name>"). No-ops otherwise.
            iced_debug::time_with("widgets view", || {
                iced::widget::stack(shown.iter().map(|((_, name), e)| {
                    iced_debug::time_with(format!("widget {name}"), || {
                        container((e.element)()).class(class()).into()
                    })
                }))
                .into()
            })
        }
    }
}
