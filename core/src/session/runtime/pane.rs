//! Native compatibility facade for the portable pane model.
//!
//! Pane identity, definitions, placement, and registry mutation are host-
//! neutral. Keeping this re-export preserves the native module path while the
//! browser consumes the same model directly from `smudgy_session_model`.

pub use smudgy_session_model::pane::*;
