use std::{cell::RefCell, collections::HashMap, mem, rc::Rc};

use iced::widget::text_editor::{Action, Content};

thread_local! {
    static ACTIVE_STORE: RefCell<Option<TextEditorStore>> = const { RefCell::new(None) };
}

/// Builder options for a text editor, read from props at build time and re-applied each frame.
#[derive(Clone, Default)]
pub struct EditorConfig {
    pub height: Option<iced::Length>,
    pub padding: Option<f32>,
    pub placeholder: Option<String>,
    pub size: Option<f32>,
}

/// Per-session store of the live editing buffers, keyed so each `<TextEditor>` instance keeps its
/// own `Content` (a key is either the author's `id`, scoped to its package, or an auto-generated
/// per-build key). Mirrors [`crate::MapStore`]: a UI-thread-local handle the view closures look up
/// while rendering, with interior mutability so edits apply in place.
#[derive(Clone, Default)]
pub struct TextEditorStore {
    editors: Rc<RefCell<HashMap<String, Rc<EditorEntry>>>>,
}

pub struct EditorHandle {
    entry: Rc<EditorEntry>,
}

struct EditorEntry {
    content: Rc<RefCell<Content>>,
}

impl TextEditorStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            editors: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Return the editor's buffer, creating it from `initial_text` on first use. An existing buffer
    /// is preserved (its current edits are kept, not reset to `initial_text`) so a re-render with a
    /// stable `id` does not discard in-progress edits.
    #[must_use]
    pub fn ensure_editor(&self, key: &str, initial_text: &str) -> EditorHandle {
        if let Some(entry) = self.editors.borrow().get(key) {
            return EditorHandle {
                entry: Rc::clone(entry),
            };
        }

        let entry = Rc::new(EditorEntry {
            content: Rc::new(RefCell::new(Content::with_text(initial_text))),
        });
        self.editors
            .borrow_mut()
            .insert(key.to_string(), Rc::clone(&entry));

        EditorHandle { entry }
    }

    /// (Re)seed the keyed editor's buffer to `text`, replacing any existing buffer. Called once per
    /// mount so a fresh build -- e.g. a script reload that re-uses an `id` -- reflects the new
    /// `value` rather than a stale buffer left in the store by a previous run.
    #[must_use]
    pub fn seed_editor(&self, key: &str, text: &str) -> EditorHandle {
        let entry = Rc::new(EditorEntry {
            content: Rc::new(RefCell::new(Content::with_text(text))),
        });
        self.editors
            .borrow_mut()
            .insert(key.to_string(), Rc::clone(&entry));
        EditorHandle { entry }
    }

    pub fn remove_editor(&self, key: &str) {
        self.editors.borrow_mut().remove(key);
    }

    /// Apply an action to the keyed editor. Returns the full text **iff** the action edited it, so
    /// the caller can fire `onChange` only on real edits, not cursor/selection movements.
    #[must_use]
    pub fn perform(&self, key: &str, action: Action) -> Option<String> {
        let entry = self.editors.borrow().get(key).map(Rc::clone)?;
        let is_edit = action.is_edit();
        entry.content.borrow_mut().perform(action);
        is_edit.then(|| entry.content.borrow().text())
    }
}

impl EditorHandle {
    /// Build the `text_editor` element for this buffer, lifted to `'static`.
    #[must_use]
    pub fn element(
        &self,
        config: &EditorConfig,
    ) -> iced::Element<'static, Action, smudgy_theme::Theme, iced::Renderer> {
        let content_ptr = self.entry.content.as_ptr();
        // SAFETY: mirrors `MapEntry::element` (map.rs). The `Content` lives in the store for as long
        // as the editor is mounted, and the borrowed element is consumed within the same frame, so
        // lifting the borrow to `'static` is sound.
        let content: &Content = unsafe { &*content_ptr };

        let mut editor = iced::widget::text_editor(content).on_action(|action| action);
        if let Some(padding) = config.padding {
            editor = editor.padding(padding);
        }
        if let Some(placeholder) = &config.placeholder {
            editor = editor.placeholder(placeholder.clone());
        }
        if let Some(size) = config.size {
            editor = editor.size(size);
        }

        // iced's `text_editor` never draws a scrollbar -- a height-bounded editor scrolls silently
        // (wheel + cursor-follow only). To give overflow a visible scrollbar, leave the editor
        // unbounded (it grows to its content) and clip it in a `scrollable` of the requested height.
        // Left unbounded, the editor's own wheel handling falls through to the scrollable (it guards
        // against internal scrolling when laid out with unbounded height), so the wheel scrolls the
        // bar. Without a `height`, the editor simply grows to fit its content.
        let element: iced::Element<'_, Action, smudgy_theme::Theme, iced::Renderer> =
            if let Some(height) = config.height {
                iced::widget::scrollable(editor)
                    .width(iced::Length::Fill)
                    .height(height)
                    .into()
            } else {
                editor.into()
            };
        // SAFETY: see above -- extend the content borrow to `'static`.
        unsafe {
            mem::transmute::<
                iced::Element<'_, Action, smudgy_theme::Theme, iced::Renderer>,
                iced::Element<'static, Action, smudgy_theme::Theme, iced::Renderer>,
            >(element)
        }
    }
}

pub fn with_text_store_context<R>(store: &TextEditorStore, f: impl FnOnce() -> R) -> R {
    ACTIVE_STORE.with(|slot| {
        let previous = slot.replace(Some(store.clone()));
        let result = f();
        slot.replace(previous);
        result
    })
}

pub fn with_active_text_store<R>(f: impl FnOnce(&TextEditorStore) -> R) -> Option<R> {
    ACTIVE_STORE
        .with(|slot| slot.borrow().clone())
        .map(|store| f(&store))
}
