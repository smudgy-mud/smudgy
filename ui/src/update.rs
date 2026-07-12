/// The result of a child `update()`: a `Task` in the child's own message type
/// plus an optional `Event` for the parent to interpret.
///
/// The type is defined in `smudgy_map_widget` because `MapView` follows the
/// same contract; this alias is the canonical path within the ui crate.
pub use smudgy_map_widget::Update;
