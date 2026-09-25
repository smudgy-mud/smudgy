//! Platform-neutral MUD wire protocols shared by Smudgy hosts.
//!
//! This crate owns byte-stream parsing, negotiation, message framing, and
//! charset conversion. It deliberately owns no sockets, async runtime,
//! filesystem, UI, or scripting engine.

pub mod css_color;
pub mod gmcp;
pub mod json;
pub mod mccp;
pub mod msdp;
pub mod mssp;
pub mod responders;
pub mod sgr;
pub mod telnet;
pub mod transcode;
pub mod url_host;
pub mod vt;
pub mod wss;

pub use url_host::link_url_host;
