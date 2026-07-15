//! MSDP wire-format helpers (`docs/gmcp-mapping-plan.md` §9 item 3; protocol reference
//! <https://tintin.mudhalla.net/protocols/msdp/>): the variable-level layer over the telnet
//! subnegotiation framing. One subnegotiation carries one or more variables as
//! `VAR <name> VAL <value>` sequences, where a value is text, a `TABLE_OPEN … TABLE_CLOSE`
//! of nested `VAR`/`VAL` pairs, or an `ARRAY_OPEN … ARRAY_CLOSE` of `VAL`s. This module
//! owns decoding inbound payloads into JSON values and framing the outbound handshake; the
//! session-side producer (store writes, catalogue) lives in `session::runtime::msdp`.
//!
//! Decoding is liberal by design, informed by live captures of real servers
//! (`docs/gmcp-mapping-plan.md` §4.2):
//!
//! - **all scalar values decode as JSON strings** — MSDP is stringly-typed on the wire
//!   (Luminari sends vnums as ASCII digits); consumers parse what they need;
//! - **multiple `VAL`s for one variable become a JSON array** even without
//!   `ARRAY_OPEN`/`ARRAY_CLOSE` markers (God Wars II sends `COMMANDS` that way);
//! - unterminated tables/arrays close at end of payload; text is lossy-UTF-8.

use serde_json::Value;

use super::telnet::{frame_subnegotiation, option};

/// MSDP value/structure markers (protocol constants; disjoint from telnet commands).
pub mod marker {
    pub const VAR: u8 = 1;
    pub const VAL: u8 = 2;
    pub const TABLE_OPEN: u8 = 3;
    pub const TABLE_CLOSE: u8 = 4;
    pub const ARRAY_OPEN: u8 = 5;
    pub const ARRAY_CLOSE: u8 = 6;
}

/// Cap on an inbound MSDP payload the client will process — same hardening rationale as
/// the GMCP cap (`docs/gmcp-plan.md` §10): the server controls subnegotiation buffer
/// growth, so a payload past this is dropped (with a log) rather than parsed and stored.
pub const MAX_INBOUND_PAYLOAD: usize = 256 * 1024;

/// The variables the enable handshake `REPORT`s — the mapping-relevant baseline
/// (`docs/gmcp-mapping-plan.md` §9 item 3): the composite `ROOM` table (Luminari-style)
/// plus the flat spellings a KaVir-snippet server sends instead. Reporting a variable a
/// server doesn't define is specified as ignored, so the union is safe to request blind.
pub const BASELINE_REPORTS: [&str; 6] = [
    "ROOM",
    "ROOM_VNUM",
    "ROOM_NAME",
    "ROOM_EXITS",
    "ROOM_TERRAIN",
    "AREA_NAME",
];

/// Frame one outbound `VAR <name> VAL <value> [VAL <value> …]` subnegotiation, `0xFF`
/// doubled by the telnet framer.
pub fn frame_command(name: &str, values: &[&str], into: &mut Vec<u8>) {
    let mut payload = Vec::with_capacity(
        1 + name.len() + values.iter().map(|v| v.len() + 1).sum::<usize>(),
    );
    payload.push(marker::VAR);
    payload.extend_from_slice(name.as_bytes());
    for value in values {
        payload.push(marker::VAL);
        payload.extend_from_slice(value.as_bytes());
    }
    frame_subnegotiation(option::MSDP, &payload, into);
}

/// Frame the enable handshake: `LIST REPORTABLE_VARIABLES` (the response lands in the
/// store like any variable — the Store tab's protocol inventory), then one `REPORT` of the
/// mapping baseline.
pub fn frame_handshake(into: &mut Vec<u8>) {
    frame_command("LIST", &["REPORTABLE_VARIABLES"], into);
    frame_command("REPORT", &BASELINE_REPORTS, into);
}

/// Decode one inbound payload into its `(name, value)` variables, in wire order.
/// Bytes before the first `VAR` (a malformed lead-in) are skipped.
#[must_use]
pub fn parse_variables(payload: &[u8]) -> Vec<(String, Value)> {
    let mut variables = Vec::new();
    let mut cursor = Cursor { bytes: payload, at: 0 };
    // Skip to the first VAR.
    while cursor.peek().is_some_and(|b| b != marker::VAR) {
        cursor.at += 1;
    }
    while cursor.peek() == Some(marker::VAR) {
        cursor.at += 1;
        let name = cursor.take_text();
        let mut values = Vec::new();
        while cursor.peek() == Some(marker::VAL) {
            cursor.at += 1;
            values.push(cursor.take_value());
        }
        let value = match values.len() {
            0 => Value::Null,
            1 => values.pop().expect("one value"),
            // Marker-less top-level array (GW2's COMMANDS shape).
            _ => Value::Array(values),
        };
        variables.push((name, value));
    }
    variables
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    /// Text up to the next marker byte (or end), lossy-UTF-8.
    fn take_text(&mut self) -> String {
        let start = self.at;
        while let Some(b) = self.peek() {
            if (marker::VAR..=marker::ARRAY_CLOSE).contains(&b) {
                break;
            }
            self.at += 1;
        }
        String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned()
    }

    /// One value: a table, an array, or scalar text (as a JSON string).
    fn take_value(&mut self) -> Value {
        match self.peek() {
            Some(b) if b == marker::TABLE_OPEN => {
                self.at += 1;
                let mut table = serde_json::Map::new();
                while let Some(b) = self.peek() {
                    if b == marker::TABLE_CLOSE {
                        self.at += 1;
                        break;
                    }
                    if b == marker::VAR {
                        self.at += 1;
                        let key = self.take_text();
                        let mut values = Vec::new();
                        while self.peek() == Some(marker::VAL) {
                            self.at += 1;
                            values.push(self.take_value());
                        }
                        let value = match values.len() {
                            0 => Value::Null,
                            1 => values.pop().expect("one value"),
                            _ => Value::Array(values),
                        };
                        table.insert(key, value);
                    } else {
                        // Stray byte inside a table: skip it rather than stall.
                        self.at += 1;
                    }
                }
                Value::Object(table)
            }
            Some(b) if b == marker::ARRAY_OPEN => {
                self.at += 1;
                let mut items = Vec::new();
                while let Some(b) = self.peek() {
                    if b == marker::ARRAY_CLOSE {
                        self.at += 1;
                        break;
                    }
                    if b == marker::VAL {
                        self.at += 1;
                        items.push(self.take_value());
                    } else {
                        self.at += 1;
                    }
                }
                Value::Array(items)
            }
            _ => Value::String(self.take_text()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::telnet::command::{IAC, SB, SE};
    use super::marker::{ARRAY_CLOSE, ARRAY_OPEN, TABLE_CLOSE, TABLE_OPEN, VAL, VAR};
    use super::*;
    use serde_json::json;

    fn bytes(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn scalar_and_multiple_variables_decode_in_order() {
        let payload = bytes(&[
            &[VAR],
            b"ROOM_VNUM",
            &[VAL],
            b"14101",
            &[VAR],
            b"ROOM_NAME",
            &[VAL],
            b"A Small Island Beach",
        ]);
        let vars = parse_variables(&payload);
        assert_eq!(
            vars,
            vec![
                ("ROOM_VNUM".to_string(), json!("14101")),
                ("ROOM_NAME".to_string(), json!("A Small Island Beach")),
            ]
        );
    }

    #[test]
    fn luminari_room_table_decodes_with_nested_tables() {
        // The golden's composite ROOM shape (docs/gmcp-mapping-plan.md §4.2).
        let payload = bytes(&[
            &[VAR],
            b"ROOM",
            &[VAL, TABLE_OPEN, VAR],
            b"VNUM",
            &[VAL],
            b"14100",
            &[VAR],
            b"NAME",
            &[VAL],
            b"A Small Island Beach",
            &[VAR],
            b"COORDS",
            &[VAL, TABLE_OPEN, VAR],
            b"X",
            &[VAL],
            b"0",
            &[VAR],
            b"Y",
            &[VAL],
            b"0",
            &[TABLE_CLOSE, VAR],
            b"EXITS",
            &[VAL, TABLE_OPEN, VAR],
            b"east",
            &[VAL],
            b"14101",
            &[TABLE_CLOSE, TABLE_CLOSE],
        ]);
        let vars = parse_variables(&payload);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].0, "ROOM");
        assert_eq!(
            vars[0].1,
            json!({
                "VNUM": "14100",
                "NAME": "A Small Island Beach",
                "COORDS": { "X": "0", "Y": "0" },
                "EXITS": { "east": "14101" },
            })
        );
    }

    #[test]
    fn marked_arrays_and_gw2_marker_less_multi_val_both_decode_as_arrays() {
        let marked = bytes(&[
            &[VAR],
            b"REPORTABLE_VARIABLES",
            &[VAL, ARRAY_OPEN, VAL],
            b"ROOM",
            &[VAL],
            b"AREA_NAME",
            &[ARRAY_CLOSE],
        ]);
        assert_eq!(
            parse_variables(&marked),
            vec![("REPORTABLE_VARIABLES".to_string(), json!(["ROOM", "AREA_NAME"]))]
        );

        // God Wars II sends top-level lists as bare repeated VALs.
        let marker_less = bytes(&[
            &[VAR],
            b"COMMANDS",
            &[VAL],
            b"LIST",
            &[VAL],
            b"REPORT",
            &[VAL],
            b"SEND",
        ]);
        assert_eq!(
            parse_variables(&marker_less),
            vec![("COMMANDS".to_string(), json!(["LIST", "REPORT", "SEND"]))]
        );
    }

    #[test]
    fn liberal_decode_survives_malformed_payloads() {
        // Unterminated table closes at end of payload.
        let unterminated = bytes(&[&[VAR], b"ROOM", &[VAL, TABLE_OPEN, VAR], b"VNUM", &[VAL], b"1"]);
        assert_eq!(
            parse_variables(&unterminated),
            vec![("ROOM".to_string(), json!({ "VNUM": "1" }))]
        );

        // Garbage lead-in is skipped; a VAR with no VAL is null; empty payload is empty.
        let lead_in = bytes(&[b"junk", &[VAR], b"PING"]);
        assert_eq!(parse_variables(&lead_in), vec![("PING".to_string(), Value::Null)]);
        assert!(parse_variables(&[]).is_empty());

        // Invalid UTF-8 decodes lossily rather than dropping the variable.
        let invalid = bytes(&[&[VAR], b"NAME", &[VAL], &[0xFF, 0xFE], b"end"]);
        let vars = parse_variables(&invalid);
        assert_eq!(vars.len(), 1);
        assert!(matches!(&vars[0].1, Value::String(s) if s.ends_with("end")));
    }

    #[test]
    fn handshake_frames_list_then_baseline_report() {
        let mut framed = Vec::new();
        frame_handshake(&mut framed);
        let expected_head: Vec<u8> = bytes(&[
            &[IAC, SB, option::MSDP, VAR],
            b"LIST",
            &[VAL],
            b"REPORTABLE_VARIABLES",
            &[IAC, SE],
        ]);
        assert!(framed.starts_with(&expected_head), "LIST frame leads");
        let text = String::from_utf8_lossy(&framed);
        for variable in BASELINE_REPORTS {
            assert!(text.contains(variable), "baseline reports {variable}");
        }
    }
}
