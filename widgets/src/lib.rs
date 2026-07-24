mod canvas;
mod extension;
mod map;
mod text_editor;
mod widget;

pub use extension::smudgy_widgets as ext;

pub use map::{MapReapGuard, MapReaper, MapStore, MapWidgetId, with_store_context};
pub use text_editor::{TextEditorStore, with_text_store_context};
pub use widget::WidgetMessage;
pub use widget::WidgetRoot;
