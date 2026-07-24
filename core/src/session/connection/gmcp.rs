//! GMCP wire-format helpers (`docs/gmcp.md`; protocol reference
//! `docs/gmcp-reference.md`): the message-level layer over the telnet subnegotiation
//! framing. One message is `Package[.Sub].Message <json>` — a dotted, case-insensitive
//! name, one space, an optional JSON data part. This module owns splitting inbound
//! payloads, framing outbound messages, and the handshake the connection task sends when
//! the option negotiates on. The session-side producer (store writes, catalogue,
//! merge keys) lives in `session::runtime::gmcp`.

use super::telnet::{frame_subnegotiation, option};

/// The game-agnostic baseline advertised in the handshake's `Core.Supports.Set` — exactly
/// Mudlet's core set, which a decade of ecosystem use has proven safe to enable blind
/// (`docs/gmcp.md` §6.1). Deliberately absent: `Comm`/`Comm.Channel` (can eat chat),
/// `Char.Login` (credential policy), and every game-specific module (packages enable what
/// they consume).
pub const BASELINE_SUPPORTS: [&str; 4] = ["Char 1", "Char.Skills 1", "Char.Items 1", "Room 1"];

/// Cap on an inbound GMCP payload the client will process (`docs/gmcp.md` §10
/// hardening): the server controls subnegotiation buffer growth, so a payload past this is
/// dropped (with a log) rather than parsed and stored.
pub const MAX_INBOUND_PAYLOAD: usize = 256 * 1024;

/// Split one inbound GMCP payload into `(name, data)` at the first space. The name is
/// returned trimmed; the data part keeps its exact text (it is the raw JSON, parsed on the
/// session thread). `None` data means the message had no data part (`Core.Ping`).
#[must_use]
pub fn split_message(payload: &str) -> (&str, Option<&str>) {
    match payload.split_once(' ') {
        Some((name, data)) => {
            let data = data.trim();
            (name.trim(), if data.is_empty() { None } else { Some(data) })
        }
        None => (payload.trim(), None),
    }
}

/// Frame one outbound GMCP message — `IAC SB GMCP <name>[ <data>] IAC SE`, `0xFF` doubled —
/// appending to `into`. The name goes out with the caller's casing (servers must match
/// case-insensitively; we don't mangle outbound).
pub fn frame_message(name: &str, data: Option<&str>, into: &mut Vec<u8>) {
    let mut payload = Vec::with_capacity(name.len() + data.map_or(0, |d| d.len() + 1));
    payload.extend_from_slice(name.as_bytes());
    if let Some(data) = data {
        payload.push(b' ');
        payload.extend_from_slice(data.as_bytes());
    }
    frame_subnegotiation(option::GMCP, &payload, into);
}

/// Frame the enable handshake (`docs/gmcp.md` §6.1): `Core.Hello` identifying the
/// client, then `Core.Supports.Set` with the baseline module set. Registered-module adds
/// follow from the session thread once the module registry exists (phase 2).
pub fn frame_handshake(into: &mut Vec<u8>) {
    let hello = format!(
        "{{\"client\":\"smudgy\",\"version\":\"{}\"}}",
        env!("CARGO_PKG_VERSION")
    );
    frame_message("Core.Hello", Some(&hello), into);
    // The baseline is a const array of plain module names; the JSON spelling is direct.
    let supports = format!("[\"{}\"]", BASELINE_SUPPORTS.join("\",\""));
    frame_message("Core.Supports.Set", Some(&supports), into);
}

#[cfg(test)]
mod tests {
    use super::super::telnet::command::{IAC, SB, SE};
    use super::super::telnet::option::GMCP;
    use super::*;

    #[test]
    fn split_message_handles_data_no_data_and_padding() {
        assert_eq!(split_message("Core.Ping"), ("Core.Ping", None));
        assert_eq!(
            split_message("Char.Vitals { \"hp\": 100 }"),
            ("Char.Vitals", Some("{ \"hp\": 100 }"))
        );
        // A trailing space with no data reads as no data part.
        assert_eq!(split_message("Core.Ping "), ("Core.Ping", None));
        assert_eq!(split_message("room.info {}"), ("room.info", Some("{}")));
    }

    #[test]
    fn frame_message_produces_the_wire_form() {
        let mut framed = Vec::new();
        frame_message("Char.Skills.Get", Some("{\"group\":\"combat\"}"), &mut framed);
        let expected: Vec<u8> = [
            &[IAC, SB, GMCP][..],
            b"Char.Skills.Get {\"group\":\"combat\"}",
            &[IAC, SE][..],
        ]
        .concat();
        assert_eq!(framed, expected);

        let mut bare = Vec::new();
        frame_message("Char.Items.Inv", None, &mut bare);
        let expected: Vec<u8> = [&[IAC, SB, GMCP][..], b"Char.Items.Inv", &[IAC, SE][..]].concat();
        assert_eq!(bare, expected);
    }

    #[test]
    fn handshake_is_hello_then_baseline_supports() {
        let mut framed = Vec::new();
        frame_handshake(&mut framed);
        let text = String::from_utf8_lossy(&framed);
        let hello = text.find("Core.Hello {\"client\":\"smudgy\"").expect("hello framed");
        let supports = text.find("Core.Supports.Set [\"Char 1\"").expect("supports framed");
        assert!(hello < supports, "Core.Hello precedes Core.Supports.Set");
        for module in BASELINE_SUPPORTS {
            assert!(text.contains(module), "baseline advertises {module}");
        }
    }
}
