//! Portable command-input operations and snapshots.

use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOp {
    Replace(Arc<String>),
    Append(Arc<String>),
    Clear,
    Propose(Arc<String>),
    SetCursor(usize),
    Select(usize, usize),
    SelectAll,
    Focus,
    Blur,
    Submit,
    SetMasked(bool),
    HistoryPush(Arc<String>),
    HistoryClear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    User,
    Script,
    Link,
    Other,
}

impl InputSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Script => "script",
            Self::Link => "link",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSnapshot {
    #[serde(serialize_with = "serialize_arc_str")]
    pub value: Arc<String>,
    pub cursor: usize,
    pub selection: Option<(usize, usize)>,
    pub focused: bool,
    pub masked: bool,
}

fn serialize_arc_str<S: serde::Serializer>(
    value: &Arc<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value)
}

impl InputSnapshot {
    #[must_use]
    pub fn content_suppressed(&self) -> Self {
        Self {
            value: Arc::new(String::new()),
            cursor: 0,
            selection: None,
            focused: self.focused,
            masked: self.masked,
        }
    }
}
