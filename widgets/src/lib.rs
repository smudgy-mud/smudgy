mod canvas;
mod extension;
mod image_store;
mod map;
mod shader_budget;
mod text_editor;
mod widget;

pub use extension::inline_element;
pub use extension::{SmudgyMarkdownViewer, smudgy_widgets as ext};
pub use smudgy_session_model::native_callback::CallbackLease as WidgetCallback;

pub use image_store::{
    DecodedImage, EntryState, FetchError, FileStamp, ImageEntryCell, ImageFetcher, ImageStore,
};
pub use map::{MapReapGuard, MapReaper, MapStore, MapWidgetId, with_store_context};
pub use text_editor::{TextEditorStore, with_text_store_context};
pub use widget::WidgetMessage;
pub use widget::WidgetRoot;
