//! The one spelling a UUID has on the script boundary: its canonical
//! lowercase hyphenated form, as a JS string.
//!
//! Map identities (`AreaId`, `ExitId`, `ConnectionId`, `AtlasId`, `LabelId`,
//! `ShapeId`, `OperationId`) used to cross as a `(u64, u64)` tuple, which
//! `serde_v8` renders as a 2-element array whose halves land above
//! `Number.MAX_SAFE_INTEGER` and therefore arrive as `BigInt`. That shape cost
//! scripts `===`, `Map`/`Set` keys and `JSON.stringify` -- so ids could not
//! ride the session store, store bindings or widget props -- and it was not
//! even fast, being three V8 heap objects per identity where a string is one.
//! `bench/examples/id_wire.rs` prices the alternatives: per identity returned
//! in bulk, the pair costs ~75 ns and the string ~18 ns, which is level with a
//! raw `u32` handle, the ceiling no encoding can beat.
//!
//! [`ScriptUuid`] is that string on the Rust side, serialized straight out of
//! a stack buffer (no `String`). It carries ids **outbound** and inside serde
//! payloads (an exit's four ids, a batch operation's target). Ids arriving as
//! a plain op argument do NOT come through here: those are `#[string]` params
//! parsed by `mapper_api::parse_id`, because serde's string path allocates and
//! transcodes per call and keeps the op off V8's fast-call path, costing
//! several times a `#[string]` param's ~18 ns per id
//! (`bench/examples/id_wire.rs`).
//!
//! The old pair spelling is gone rather than deprecated: it was an opaque
//! handle nothing could store, so no script could be holding one across the
//! change. The author-facing contract is
//! `models/script_typings/smudgy-mapper.d.ts` (`AreaId`).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use smudgy_cloud::Uuid;

/// Length of a canonical hyphenated UUID: 32 hex digits plus 4 hyphens. The
/// spelling is fixed-width, so formatting one never needs the heap.
const HYPHENATED_LEN: usize = 36;

/// A UUID as scripts see it: the canonical lowercase hyphenated string, in
/// both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ScriptUuid(pub Uuid);

impl ScriptUuid {
    pub(crate) fn into_uuid(self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for ScriptUuid {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

impl From<ScriptUuid> for Uuid {
    fn from(value: ScriptUuid) -> Self {
        value.0
    }
}

impl fmt::Display for ScriptUuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl Serialize for ScriptUuid {
    /// Always the canonical string, formatted into a stack buffer: the id
    /// alphabet is ASCII and the length is fixed, so no allocation is needed
    /// to hand one to V8.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut buf = [0u8; HYPHENATED_LEN];
        serializer.serialize_str(self.0.hyphenated().encode_lower(&mut buf))
    }
}

impl<'de> Deserialize<'de> for ScriptUuid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ScriptUuidVisitor;

        impl<'de> de::Visitor<'de> for ScriptUuidVisitor {
            type Value = ScriptUuid;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a canonical UUID string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Uuid::try_parse(value)
                    .map(ScriptUuid)
                    .map_err(|_| E::custom(format!("{value:?} is not a canonical UUID string")))
            }
        }

        deserializer.deserialize_str(ScriptUuidVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "67e55044-10b1-426f-9247-bb680e5fe0c8";

    #[test]
    fn serializes_to_the_canonical_lowercase_string() {
        let id = ScriptUuid(Uuid::try_parse(CANONICAL).unwrap());
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{CANONICAL}\"")
        );
    }

    #[test]
    fn round_trips_through_the_canonical_string() {
        let id = ScriptUuid(Uuid::try_parse(CANONICAL).unwrap());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<ScriptUuid>(&json).unwrap(), id);
    }

    /// Uppercase and braced spellings are accepted on the way in -- `try_parse`
    /// is liberal -- but never produced.
    #[test]
    fn accepts_noncanonical_spellings_inbound() {
        let upper = format!("\"{}\"", CANONICAL.to_uppercase());
        assert_eq!(
            serde_json::from_str::<ScriptUuid>(&upper)
                .unwrap()
                .to_string(),
            CANONICAL
        );
    }

    /// The pre-0.6 `[hi, lo]` pair is not an id any more; it must fail loudly
    /// rather than decode to something plausible.
    #[test]
    fn rejects_the_old_hi_lo_pair() {
        let (hi, lo) = Uuid::try_parse(CANONICAL).unwrap().as_u64_pair();
        assert!(serde_json::from_str::<ScriptUuid>(&format!("[{hi},{lo}]")).is_err());
    }

    #[test]
    fn rejects_a_non_uuid_string() {
        assert!(serde_json::from_str::<ScriptUuid>("\"not-an-id\"").is_err());
    }
}
