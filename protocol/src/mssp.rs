//! MSSP wire-format helpers (protocol reference
//! <https://tintin.mudhalla.net/protocols/mssp/>): the variable layer over the telnet
//! subnegotiation framing. One subnegotiation carries the server's whole status block as
//! `MSSP_VAR <name> MSSP_VAL <value>` sequences, where a variable sent with several
//! `VAL`s is an array (the spec allows it for `PORT`, `CHARSET`, `REFERRAL`, …) and a
//! repeated `VAR` replaces the earlier value (last write wins). MSSP is inbound-only —
//! there is no client→server direction — so this module frames nothing.
//!
//! Every value is attacker-controlled input from the game: everything decoded here is
//! display/advisory metadata, never connection behavior. Decoding is total by design —
//! names and values may contain spaces (`MINIMUM AGE`) and any byte but the two markers
//! is text, decoded lossily against the server's *configured* encoding
//! ([`Transcode::configured_encoding`](super::transcode::Transcode::configured_encoding)):
//! MSSP can arrive before CHARSET negotiation settles, and the spec is silent on payload
//! encoding, so the live stream decoder is deliberately not involved.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use encoding_rs::Encoding;

/// MSSP variable/value markers (protocol constants; disjoint from telnet commands).
pub mod marker {
    pub const VAR: u8 = 1;
    pub const VAL: u8 = 2;
}

/// Cap on an inbound MSSP payload the client will process — same hardening rationale as
/// the GMCP cap (`docs/gmcp.md` §10): the server controls subnegotiation buffer
/// growth, so a payload past this is dropped (with a log) rather than parsed and stored.
pub const MAX_INBOUND_PAYLOAD: usize = 256 * 1024;

/// One decoded variable value: scalar text, or the multi-`VAL` array form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MsspValue {
    Text(String),
    List(Vec<String>),
}

impl MsspValue {
    /// The value as one scalar: the text itself, or — for the array form —
    /// the last element, per the spec's preferred-value-last convention
    /// (`PORT`). Empty when the array is empty.
    #[must_use]
    pub fn preferred_scalar(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::List(items) => items.last().map_or("", String::as_str),
        }
    }
}

/// Decode one inbound payload into its `(name, value)` variables, in first-seen order.
/// Bytes before the first `VAR` (a malformed lead-in, including any `VAL` with no
/// variable to belong to) are skipped; a `VAR` with no `VAL` carries empty text; a
/// nameless `VAR` is dropped whole; a repeated name keeps its position and takes the
/// later value.
#[must_use]
pub fn parse_variables(payload: &[u8], encoding: &'static Encoding) -> Vec<(String, MsspValue)> {
    let mut variables: Vec<(String, MsspValue)> = Vec::new();
    // Index side table for the repeated-name replacement: rescanning the Vec
    // per variable would go quadratic in distinct names, and a payload at the
    // cap can carry tens of thousands of them — this parse runs on the shared
    // socket runtime, where that is a cross-session stall.
    let mut index_by_name: HashMap<String, usize> = HashMap::new();
    let mut at = 0usize;
    // Skip to the first VAR.
    while payload.get(at).is_some_and(|&b| b != marker::VAR) {
        at += 1;
    }
    while payload.get(at) == Some(&marker::VAR) {
        at += 1;
        let name = take_text(payload, &mut at, encoding);
        let mut values = Vec::new();
        while payload.get(at) == Some(&marker::VAL) {
            at += 1;
            values.push(take_text(payload, &mut at, encoding));
        }
        if name.is_empty() {
            continue;
        }
        let value = match values.len() {
            0 => MsspValue::Text(String::new()),
            1 => MsspValue::Text(values.pop().unwrap_or_default()),
            _ => MsspValue::List(values),
        };
        match index_by_name.entry(name) {
            Entry::Occupied(entry) => variables[*entry.get()].1 = value,
            Entry::Vacant(entry) => {
                let name = entry.key().clone();
                entry.insert(variables.len());
                variables.push((name, value));
            }
        }
    }
    variables
}

/// The facts the TLS upgrade-offer decision weighs, gathered from the live
/// connection (transport mode, dialed endpoint), the variables the server sent
/// (`TLS`/`SSL` are aliases for the advertised port; `HOSTNAME` is its claim
/// of identity), the persisted per-server refusal, and the once-per-connection
/// banner latch. The guard state is connection-scoped by contract: the
/// advertised port and the latch die with the connection, so a stale answer
/// can never act on a different connection's offer.
#[derive(Clone, Copy, Debug)]
pub struct TlsOffer<'a> {
    /// Whether the current transport is already encrypted.
    pub encrypted: bool,
    /// The host the user's config dialed, verbatim.
    pub dialed_host: &'a str,
    /// The port the current connection dialed.
    pub dialed_port: u16,
    /// The raw value of the last `TLS`/`SSL` variable received.
    pub advertised: &'a str,
    /// The raw `HOSTNAME` variable, when the server sent one.
    pub advertised_hostname: Option<&'a str>,
    /// The persisted per-server refusal (`observed.json`'s
    /// `tls_offer_declined`): "no" means "stop asking for this server".
    pub offer_declined: bool,
    /// Whether the offer banner was already shown on this connection
    /// (a re-advertisement storm must not stack banners).
    pub already_offered: bool,
}

/// Decide whether to offer switching this server to an advertised TLS port,
/// returning the port to offer. The one place an MSSP variable is allowed to
/// influence connection config — behind explicit user consent — so the guard
/// set (Mudlet's, hard-won) is checked whole, in one place: the offer stands
/// only when the transport is unencrypted, the value is a real port (the
/// `"1"`/`"-1"`/`"0"` support flags are not) different from the one already
/// dialed, the dialed host is not an IP literal (a certificate cannot validate
/// against one), any advertised `HOSTNAME` names the dialed host (a mismatch
/// means the port is not this server's to offer), the user has not declined
/// for this server, and no offer was shown on this connection yet.
#[must_use]
pub fn tls_upgrade_port(offer: &TlsOffer<'_>) -> Option<u16> {
    if offer.encrypted || offer.offer_declined || offer.already_offered {
        return None;
    }
    // "1"/"-1"/"0" say "supported"/"unsupported", not a port number.
    let advertised = offer.advertised.trim();
    if matches!(advertised, "1" | "-1" | "0") {
        return None;
    }
    // u16 parsing rejects everything outside 0..=65535 (signs and non-digits
    // included); port 0 is unconnectable.
    let port: u16 = advertised.parse().ok().filter(|&port| port != 0)?;
    if port == offer.dialed_port {
        return None;
    }
    // The reconnect would present the dialed host as the TLS server name, and
    // a certificate cannot validate against an IP literal (bracketed IPv6
    // spellings included).
    let host = offer
        .dialed_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(offer.dialed_host);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    // An advertised HOSTNAME naming a different host means the offered port is
    // not the dialed server's to give (a hostile redirect, or a mirror's
    // misconfiguration). Absent or empty, no claim is made — the dialed host
    // stands.
    if offer
        .advertised_hostname
        .map(str::trim)
        .filter(|hostname| !hostname.is_empty())
        .is_some_and(|hostname| !hostname.eq_ignore_ascii_case(offer.dialed_host))
    {
        return None;
    }
    Some(port)
}

/// Text up to the next marker byte (or end), decoded lossily.
fn take_text(payload: &[u8], at: &mut usize, encoding: &'static Encoding) -> String {
    let start = *at;
    while payload
        .get(*at)
        .is_some_and(|&b| b != marker::VAR && b != marker::VAL)
    {
        *at += 1;
    }
    encoding
        .decode_without_bom_handling(&payload[start..*at])
        .0
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::marker::{VAL, VAR};
    use super::*;
    use encoding_rs::{UTF_8, WINDOWS_1252};

    fn bytes(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn text(value: &str) -> MsspValue {
        MsspValue::Text(value.to_string())
    }

    #[test]
    fn the_required_trio_decodes_in_wire_order() {
        let payload = bytes(&[
            &[VAR],
            b"NAME",
            &[VAL],
            b"ArcticMUD",
            &[VAR],
            b"PLAYERS",
            &[VAL],
            b"52",
            &[VAR],
            b"UPTIME",
            &[VAL],
            b"1745000000",
        ]);
        assert_eq!(
            parse_variables(&payload, UTF_8),
            vec![
                ("NAME".to_string(), text("ArcticMUD")),
                ("PLAYERS".to_string(), text("52")),
                ("UPTIME".to_string(), text("1745000000")),
            ]
        );
    }

    #[test]
    fn multiple_vals_under_one_var_decode_as_an_array() {
        // The spec explicitly allows arrays (PORT, CHARSET, REFERRAL, …); dropping a
        // multi-VAL variable — Mudlet's behavior — loses data.
        let payload = bytes(&[
            &[VAR],
            b"PORT",
            &[VAL],
            b"2700",
            &[VAL],
            b"6667",
            &[VAR],
            b"CHARSET",
            &[VAL],
            b"UTF-8",
        ]);
        assert_eq!(
            parse_variables(&payload, UTF_8),
            vec![
                (
                    "PORT".to_string(),
                    MsspValue::List(vec!["2700".to_string(), "6667".to_string()])
                ),
                ("CHARSET".to_string(), text("UTF-8")),
            ]
        );
    }

    #[test]
    fn a_repeated_var_takes_the_later_value_in_its_first_position() {
        let payload = bytes(&[
            &[VAR],
            b"PLAYERS",
            &[VAL],
            b"52",
            &[VAR],
            b"NAME",
            &[VAL],
            b"ArcticMUD",
            &[VAR],
            b"PLAYERS",
            &[VAL],
            b"53",
        ]);
        assert_eq!(
            parse_variables(&payload, UTF_8),
            vec![
                ("PLAYERS".to_string(), text("53")),
                ("NAME".to_string(), text("ArcticMUD")),
            ]
        );
    }

    #[test]
    fn names_and_values_containing_spaces_decode_verbatim() {
        // Spec variable names contain spaces (MINIMUM AGE, XTERM 256 COLORS); a space is
        // ordinary text on both sides of the pair.
        let payload = bytes(&[
            &[VAR],
            b"MINIMUM AGE",
            &[VAL],
            b"13",
            &[VAR],
            b"LOCATION",
            &[VAL],
            b"United States of America",
        ]);
        assert_eq!(
            parse_variables(&payload, UTF_8),
            vec![
                ("MINIMUM AGE".to_string(), text("13")),
                ("LOCATION".to_string(), text("United States of America")),
            ]
        );
    }

    #[test]
    fn hostile_shapes_decode_liberally() {
        // Empty payload is empty.
        assert!(parse_variables(&[], UTF_8).is_empty());

        // A VAR with no VAL carries empty text.
        let bare = bytes(&[&[VAR], b"CONTACT", &[VAR], b"NAME", &[VAL], b"x"]);
        assert_eq!(
            parse_variables(&bare, UTF_8),
            vec![
                ("CONTACT".to_string(), text("")),
                ("NAME".to_string(), text("x")),
            ]
        );

        // A VAL before any VAR has no variable to belong to and is skipped, like any
        // other malformed lead-in.
        let orphan = bytes(&[&[VAL], b"orphan", &[VAR], b"NAME", &[VAL], b"x"]);
        assert_eq!(
            parse_variables(&orphan, UTF_8),
            vec![("NAME".to_string(), text("x"))]
        );
        assert!(parse_variables(&bytes(&[&[VAL], b"orphan"]), UTF_8).is_empty());

        // A nameless VAR is dropped whole, values included.
        let nameless = bytes(&[&[VAR, VAL], b"lost", &[VAR], b"NAME", &[VAL], b"x"]);
        assert_eq!(
            parse_variables(&nameless, UTF_8),
            vec![("NAME".to_string(), text("x"))]
        );
    }

    #[test]
    fn non_utf8_bytes_decode_lossily_against_the_configured_encoding() {
        // 0xE9 is invalid UTF-8 alone: the fallback decode replaces it rather than
        // dropping the variable…
        let payload = bytes(&[&[VAR], b"NAME", &[VAL], b"Caf\xE9"]);
        assert_eq!(
            parse_variables(&payload, UTF_8),
            vec![("NAME".to_string(), text("Caf\u{fffd}"))]
        );
        // …while a server configured Latin-1 decodes it as the é it meant.
        assert_eq!(
            parse_variables(&payload, WINDOWS_1252),
            vec![("NAME".to_string(), text("Caf\u{e9}"))]
        );
    }

    /// A baseline offer every guard passes: plain transport against a DNS
    /// name, a sane distinct port, no hostname claim, no refusal, no latch.
    fn offer(advertised: &str) -> TlsOffer<'_> {
        TlsOffer {
            encrypted: false,
            dialed_host: "mud.example.org",
            dialed_port: 4000,
            advertised,
            advertised_hostname: None,
            offer_declined: false,
            already_offered: false,
        }
    }

    #[test]
    fn a_usable_advertised_port_is_offered() {
        assert_eq!(tls_upgrade_port(&offer("6667")), Some(6667));
        assert_eq!(
            tls_upgrade_port(&offer(" 6667 ")),
            Some(6667),
            "sloppy whitespace around a real port is tolerated"
        );
        assert_eq!(tls_upgrade_port(&offer("65535")), Some(65535));
    }

    #[test]
    fn support_flags_are_not_ports() {
        // "1"/"-1"/"0" mean "TLS supported"/"unsupported"; "1" would otherwise
        // parse as a (nonsense) port.
        assert_eq!(tls_upgrade_port(&offer("1")), None);
        assert_eq!(tls_upgrade_port(&offer("-1")), None);
        assert_eq!(tls_upgrade_port(&offer("0")), None);
    }

    #[test]
    fn port_lies_are_rejected() {
        assert_eq!(tls_upgrade_port(&offer("99999")), None, "out of range");
        assert_eq!(tls_upgrade_port(&offer("-5")), None, "negative");
        assert_eq!(tls_upgrade_port(&offer("")), None, "empty");
        assert_eq!(tls_upgrade_port(&offer("6667a")), None, "trailing junk");
        assert_eq!(
            tls_upgrade_port(&offer("4000")),
            None,
            "the port already dialed offers nothing"
        );
    }

    #[test]
    fn an_encrypted_connection_never_offers() {
        assert_eq!(
            tls_upgrade_port(&TlsOffer {
                encrypted: true,
                ..offer("6667")
            }),
            None
        );
    }

    #[test]
    fn a_persisted_refusal_or_shown_banner_suppresses_the_offer() {
        assert_eq!(
            tls_upgrade_port(&TlsOffer {
                offer_declined: true,
                ..offer("6667")
            }),
            None
        );
        assert_eq!(
            tls_upgrade_port(&TlsOffer {
                already_offered: true,
                ..offer("6667")
            }),
            None
        );
    }

    #[test]
    fn an_ip_literal_dial_never_offers() {
        // A certificate cannot validate against an IP literal, so the switch
        // could only dead-end at the handshake.
        for host in ["203.0.113.7", "::1", "[2001:db8::1]"] {
            assert_eq!(
                tls_upgrade_port(&TlsOffer {
                    dialed_host: host,
                    ..offer("6667")
                }),
                None,
                "{host}"
            );
        }
    }

    #[test]
    fn an_advertised_hostname_must_name_the_dialed_host() {
        assert_eq!(
            tls_upgrade_port(&TlsOffer {
                advertised_hostname: Some("other.example.org"),
                ..offer("6667")
            }),
            None,
            "a foreign hostname means the port is not this server's to offer"
        );
        assert_eq!(
            tls_upgrade_port(&TlsOffer {
                advertised_hostname: Some("MUD.Example.ORG"),
                ..offer("6667")
            }),
            Some(6667),
            "hostnames compare case-insensitively"
        );
        assert_eq!(
            tls_upgrade_port(&TlsOffer {
                advertised_hostname: Some("  "),
                ..offer("6667")
            }),
            Some(6667),
            "an empty HOSTNAME makes no claim"
        );
    }

    #[test]
    fn many_distinct_names_parse_in_linear_time_with_dedup_intact() {
        // A payload dense with distinct tiny names is the hostile shape that
        // punishes a quadratic repeated-name scan; the parse must stay linear
        // while replacement semantics hold across the whole set.
        let count = 50_000usize;
        let mut payload = Vec::new();
        for i in 0..count {
            payload.push(VAR);
            payload.extend_from_slice(format!("V{i}").as_bytes());
            payload.push(VAL);
            payload.extend_from_slice(b"a");
        }
        payload.push(VAR);
        payload.extend_from_slice(b"V0");
        payload.push(VAL);
        payload.extend_from_slice(b"b");
        let variables = parse_variables(&payload, UTF_8);
        assert_eq!(variables.len(), count);
        assert_eq!(variables[0], ("V0".to_string(), text("b")));
        assert_eq!(variables[count - 1], (format!("V{}", count - 1), text("a")));
    }

    #[test]
    fn a_payload_at_the_cap_parses_whole() {
        // The cap itself is enforced by the connection layer before parsing
        // (`on_subnegotiation`); at exactly the cap the payload is still parsed.
        let mut payload = bytes(&[&[VAR], b"NAME", &[VAL]]);
        payload.resize(MAX_INBOUND_PAYLOAD, b'a');
        let variables = parse_variables(&payload, UTF_8);
        assert_eq!(variables.len(), 1);
        assert_eq!(variables[0].0, "NAME");
        assert!(
            matches!(&variables[0].1, MsspValue::Text(s) if s.len() == MAX_INBOUND_PAYLOAD - 6)
        );
    }
}
