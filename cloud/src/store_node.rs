//! The session store's value tree (`smudgy/docs/interop.md` §2): an immutable,
//! structurally-shared JSON tree (`smudgy/docs/interop.md` §2).
//!
//! `Node` lives **here** for the same crate-DAG reason as [`StoreBindingCell`]
//! (`crate::store_bindings`): the widget binding cells hold `Node` snapshots, their readers are
//! in the leaf `smudgy_widgets` crate, the writer is `core`'s session store, and `smudgy_cloud`
//! is the one crate all three already depend on.
//!
//! Shape and sharing:
//! - **Interior edges are plain `Arc`, never `ArcSwap`.** The session store mutates on one
//!   thread through [`Node::set_at`], which walks the written spine with [`Arc::make_mut`] —
//!   in place where a node is uniquely owned, a clone only where a binding cell or retained
//!   generation pinned it (pay-for-what-you-retain). `ArcSwap` belongs at the genuine
//!   cross-thread slots (the binding cells), where concurrent readers actually exist.
//! - **Objects are insertion-ordered, fold-keyed maps.** Entry order preserved-as-published
//!   and case-preserving/case-insensitive keying are documented contracts (interop.md §2):
//!   entries key by the ASCII-folded key and retain the first-published spelling, which is
//!   what enumeration and serialization emit.
//! - **Every container memoizes its [`Usage`]**, maintained through [`Node::set_at`], so the
//!   store's budget probe reads a replaced subtree's size in O(1) at the end of an O(spine)
//!   walk instead of re-measuring the subtree.
//! - **Serialization is byte-identical to `serde_json`.** Numbers stay [`serde_json::Number`]
//!   (inline — no float coercion) and [`Node`]'s `Serialize` forwards scalar-for-scalar and
//!   key-for-key, so `Node::to_string` emits exactly what the `serde_json::Value` it was built
//!   from would — the store's watcher snapshots and op-boundary payloads assert this text.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use indexmap::IndexMap;
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

/// Size of one value tree for the session store's per-producer budget accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// JSON nodes (every value — object, array, or leaf — counts one).
    pub entries: u64,
    /// Approximate serialized bytes (values + keys + structural overhead).
    pub bytes: u64,
}

impl Usage {
    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            entries: self.entries.saturating_sub(other.entries),
            bytes: self.bytes.saturating_sub(other.bytes),
        }
    }

    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            entries: self.entries.saturating_add(other.entries),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }
}

/// Serialized-size approximations used by [`Usage`] accounting: per-key quoting/colon overhead
/// and per-container brace overhead. Approximate is fine — budgets bound runaway producers,
/// they don't bill exact bytes — but the same constants are used everywhere (node construction,
/// spine maintenance, the store's created-intermediates charge) so accounting stays
/// self-consistent.
pub const KEY_OVERHEAD_BYTES: u64 = 4;
/// See [`KEY_OVERHEAD_BYTES`].
pub const CONTAINER_OVERHEAD_BYTES: u64 = 2;

/// One immutable JSON value. Scalars are inline (strings behind `Arc<str>` so clones are
/// pointer bumps); containers share their payload behind `Arc`, making [`Node::clone`] O(1)
/// whatever the subtree size — the property the store's binding-cell snapshots and per-turn
/// head seeding rely on.
#[derive(Clone, Debug)]
pub enum Node {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(Arc<str>),
    Array(Arc<ArrayNode>),
    Object(Arc<ObjectNode>),
}

/// An array payload: items plus the memoized subtree [`Usage`]. Arrays are addressed whole
/// by the store's path grammar (no index segments), so there is no per-item mutation path —
/// an array is only ever replaced.
#[derive(Clone, Debug)]
pub struct ArrayNode {
    items: Vec<Node>,
    usage: Usage,
}

impl ArrayNode {
    #[must_use]
    pub fn items(&self) -> &[Node] {
        &self.items
    }
}

/// An object payload: insertion-ordered entries keyed by ASCII-folded key, each retaining its
/// first-published spelling, plus the memoized subtree [`Usage`].
#[derive(Clone, Debug)]
pub struct ObjectNode {
    entries: IndexMap<FoldedKey, ObjectEntry>,
    usage: Usage,
}

/// One object entry: the first-published key spelling and the child node.
#[derive(Clone, Debug)]
struct ObjectEntry {
    key: Arc<str>,
    node: Node,
}

/// An ASCII-folded (lowercase) object key — the identity keys match under. Hashes and compares
/// by its folded bytes; [`Fold`] is the borrowed lookup adapter that folds while hashing so a
/// mixed-case path segment resolves without allocating.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FoldedKey(Arc<str>);

impl FoldedKey {
    /// Fold `spelling`, reusing its `Arc` when it is already lowercase (the common case).
    fn from_spelling(spelling: &Arc<str>) -> Self {
        if spelling.bytes().any(|b| b.is_ascii_uppercase()) {
            Self(Arc::from(spelling.to_ascii_lowercase()))
        } else {
            Self(Arc::clone(spelling))
        }
    }
}

impl Hash for FoldedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Already folded: hash the bytes as-is, with the same terminator `Fold` writes.
        state.write(self.0.as_bytes());
        state.write_u8(0xff);
    }
}

/// Borrowed fold-on-the-fly lookup key: hashes as if its content were ASCII-lowercased
/// (through a stack buffer — no allocation for segments up to [`FOLD_STACK_BYTES`]) and
/// compares case-insensitively, so `map.get(&Fold(segment))` finds the [`FoldedKey`] whatever
/// the segment's casing. Key lookup is a hot path (every proxy trap and every write walks
/// one lookup per path segment).
struct Fold<'a>(&'a str);

/// Stack-buffer size for [`Fold`]'s fold-while-hashing. Real store keys are far shorter;
/// longer mixed-case keys take a one-off allocation.
const FOLD_STACK_BYTES: usize = 64;

impl Hash for Fold<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let bytes = self.0.as_bytes();
        if bytes.iter().any(u8::is_ascii_uppercase) {
            if bytes.len() <= FOLD_STACK_BYTES {
                let mut buf = [0u8; FOLD_STACK_BYTES];
                for (slot, byte) in buf.iter_mut().zip(bytes) {
                    *slot = byte.to_ascii_lowercase();
                }
                state.write(&buf[..bytes.len()]);
            } else {
                state.write(self.0.to_ascii_lowercase().as_bytes());
            }
        } else {
            state.write(bytes);
        }
        state.write_u8(0xff);
    }
}

impl indexmap::Equivalent<FoldedKey> for Fold<'_> {
    fn equivalent(&self, key: &FoldedKey) -> bool {
        self.0.eq_ignore_ascii_case(&key.0)
    }
}

impl ObjectNode {
    /// Entry count (own keys).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Own keys in publish order, first-published spelling.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.values().map(|entry| &*entry.key)
    }

    /// `(first-published key spelling, child)` pairs in publish order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.entries
            .values()
            .map(|entry| (&*entry.key, &entry.node))
    }

    /// Fold-aware child lookup.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entries.get(&Fold(key)).map(|entry| &entry.node)
    }

    /// Replace (fold-matched key keeps its stored spelling and position) or append (a new key
    /// is inserted as written, at the end) one child, maintaining the memoized usage.
    fn set_entry(&mut self, key: &str, node: Node) {
        if let Some(entry) = self.entries.get_mut(&Fold(key)) {
            let old = entry.node.usage();
            let new = node.usage();
            entry.node = node;
            self.usage = self.usage.saturating_sub(old).saturating_add(new);
        } else {
            let spelling: Arc<str> = Arc::from(key);
            let child_usage = node.usage();
            // A new key adds its child's usage plus the key text, the per-key overhead, and
            // one length byte — exactly what full measurement charges per entry.
            self.usage = self.usage.saturating_add(Usage {
                entries: child_usage.entries,
                bytes: (spelling.len() as u64) + KEY_OVERHEAD_BYTES + 1 + child_usage.bytes,
            });
            self.entries.insert(
                FoldedKey::from_spelling(&spelling),
                ObjectEntry {
                    key: spelling,
                    node,
                },
            );
        }
    }

    /// Recompute the memoized usage delta after mutating the child at `key` in place.
    fn apply_child_delta(&mut self, old: Usage, new: Usage) {
        self.usage = self.usage.saturating_sub(old).saturating_add(new);
    }
}

impl Node {
    /// An empty object — the shape a producer subtree root starts as.
    #[must_use]
    pub fn empty_object() -> Self {
        Self::Object(Arc::new(ObjectNode {
            entries: IndexMap::new(),
            usage: Usage {
                entries: 1,
                bytes: CONTAINER_OVERHEAD_BYTES,
            },
        }))
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[must_use]
    pub fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// The object payload, when this node is an object.
    #[must_use]
    pub fn as_object(&self) -> Option<&ObjectNode> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }

    /// Numeric reading, mirroring `serde_json::Value::as_f64` (numbers only — no coercion).
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(number) => number.as_f64(),
            _ => None,
        }
    }

    /// String reading, mirroring `serde_json::Value::as_str`.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    /// Fold-aware key lookup. Non-objects have no keys — path segments never index arrays.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_object()?.get(key)
    }

    /// Walk `path` with fold-aware lookups.
    #[must_use]
    pub fn extract(&self, path: &[String]) -> Option<&Self> {
        let mut node = self;
        for segment in path {
            node = node.get(segment)?;
        }
        Some(node)
    }

    /// This subtree's [`Usage`] — memoized on containers, computed on scalars. Every node
    /// counts one entry; byte charges use the shared overhead constants.
    #[must_use]
    pub fn usage(&self) -> Usage {
        match self {
            Self::Null => Usage {
                entries: 1,
                bytes: 4,
            },
            Self::Bool(_) => Usage {
                entries: 1,
                bytes: 5,
            },
            Self::Number(number) => Usage {
                entries: 1,
                bytes: number_json_len(number),
            },
            Self::String(text) => Usage {
                entries: 1,
                bytes: text.len() as u64 + 2,
            },
            Self::Array(array) => array.usage,
            Self::Object(object) => object.usage,
        }
    }

    /// Replace the subtree at `path` with `value`, creating intermediate objects as needed.
    /// Key matching folds case; a matched key keeps its stored spelling and position, a new
    /// key is appended as written. An intermediate that exists as a non-object is replaced by
    /// an object — set-at-path owns the shape of everything at and below the deepest existing
    /// object. Mutation runs [`Arc::make_mut`] down the written spine (in place where uniquely
    /// owned, a clone only where the node is shared) and maintains each spine node's memoized
    /// usage; untouched siblings stay shared.
    pub fn set_at(&mut self, path: &[String], value: Self) {
        let Some((first, rest)) = path.split_first() else {
            *self = value;
            return;
        };
        if !self.is_object() {
            *self = Self::empty_object();
        }
        let Self::Object(arc) = self else {
            unreachable!("just ensured an object");
        };
        let object = Arc::make_mut(arc);
        if rest.is_empty() {
            object.set_entry(first, value);
            return;
        }
        // Descend into an existing fold-matched child in place (delta-maintaining the memoized
        // usage), or graft a freshly built spine for the missing remainder.
        if let Some(entry) = object.entries.get_mut(&Fold(first)) {
            let old = entry.node.usage();
            entry.node.set_at(rest, value);
            let new = entry.node.usage();
            object.apply_child_delta(old, new);
        } else {
            object.set_entry(first, Self::spine(rest, value));
        }
    }

    /// A chain of single-entry objects wrapping `value` under `path` — the intermediates a
    /// write conjures below the deepest existing object.
    fn spine(path: &[String], value: Self) -> Self {
        path.iter().rev().fold(value, |child, segment| {
            let spelling: Arc<str> = Arc::from(segment.as_str());
            let child_usage = child.usage();
            let usage = Usage {
                entries: 1 + child_usage.entries,
                bytes: CONTAINER_OVERHEAD_BYTES
                    + 1
                    + (spelling.len() as u64)
                    + KEY_OVERHEAD_BYTES
                    + child_usage.bytes,
            };
            let mut entries = IndexMap::with_capacity(1);
            entries.insert(
                FoldedKey::from_spelling(&spelling),
                ObjectEntry {
                    key: spelling,
                    node: child,
                },
            );
            Self::Object(Arc::new(ObjectNode { entries, usage }))
        })
    }

    /// Convert a published `serde_json::Value`, reporting whether any object carried two
    /// case-fold-equal spellings of one key (collapsed to one entry — first spelling and
    /// position, last value — the store's teaching-diagnostic condition). [`From`] is this
    /// conversion with the report discarded.
    #[must_use]
    pub fn from_value_reporting(value: serde_json::Value) -> (Self, bool) {
        let mut collapsed = false;
        let node = from_value(value, &mut collapsed);
        (node, collapsed)
    }

    /// Compact JSON text, byte-identical to what the `serde_json::Value` form of the same
    /// tree serializes to. This is the hot-path spelling — one `Vec` buffer, no `fmt`
    /// machinery — used by the store's per-delivery watcher snapshots and op-boundary reads;
    /// [`Node`]'s `Display` yields the same bytes for `format!`-style contexts.
    ///
    /// # Panics
    ///
    /// Never: a string-keyed JSON tree always serializes.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a string-keyed JSON tree always serializes")
    }

    /// Materialize this subtree as a `serde_json::Value` (an O(subtree) copy — the boundary
    /// conversion for consumers that still traffic in `Value`).
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(flag) => serde_json::Value::Bool(*flag),
            Self::Number(number) => serde_json::Value::Number(number.clone()),
            Self::String(text) => serde_json::Value::String(text.to_string()),
            Self::Array(array) => {
                serde_json::Value::Array(array.items.iter().map(Self::to_value).collect())
            }
            Self::Object(object) => {
                let mut map = serde_json::Map::with_capacity(object.entries.len());
                for entry in object.entries.values() {
                    map.insert(entry.key.to_string(), entry.node.to_value());
                }
                serde_json::Value::Object(map)
            }
        }
    }
}

/// Serialized length of a JSON number, counted through a discarding writer rather than
/// materialized via `to_string`. [`Node::usage`] runs several times per store write (the
/// incoming value, the budget probe's replaced subtree, and the turn head's spine deltas),
/// and numeric leaves are the canonical per-line write shape — a `String` here would be
/// avoidable heap traffic on the hot write path. Byte-exact with serialization by
/// construction: the count streams through the same serializer that emits the text.
fn number_json_len(number: &serde_json::Number) -> u64 {
    struct CountingWriter(u64);
    impl std::io::Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len() as u64;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = CountingWriter(0);
    serde_json::to_writer(&mut counter, number).expect("a JSON number always serializes");
    counter.0
}

fn from_value(value: serde_json::Value, collapsed: &mut bool) -> Node {
    match value {
        serde_json::Value::Null => Node::Null,
        serde_json::Value::Bool(flag) => Node::Bool(flag),
        serde_json::Value::Number(number) => Node::Number(number),
        serde_json::Value::String(text) => Node::String(Arc::from(text)),
        serde_json::Value::Array(items) => {
            let items: Vec<Node> = items
                .into_iter()
                .map(|item| from_value(item, collapsed))
                .collect();
            let usage = items.iter().fold(
                Usage {
                    entries: 1,
                    bytes: CONTAINER_OVERHEAD_BYTES + items.len() as u64,
                },
                |acc, item| acc.saturating_add(item.usage()),
            );
            Node::Array(Arc::new(ArrayNode { items, usage }))
        }
        serde_json::Value::Object(map) => {
            let mut entries: IndexMap<FoldedKey, ObjectEntry> = IndexMap::with_capacity(map.len());
            for (key, child) in map {
                let child = from_value(child, collapsed);
                // Fold-duplicate spellings within one published object collapse to one entry:
                // first spelling and position, last value.
                if let Some(existing) = entries.get_mut(&Fold(&key)) {
                    *collapsed = true;
                    existing.node = child;
                } else {
                    let spelling: Arc<str> = Arc::from(key);
                    entries.insert(
                        FoldedKey::from_spelling(&spelling),
                        ObjectEntry {
                            key: spelling,
                            node: child,
                        },
                    );
                }
            }
            let usage = entries.values().fold(
                Usage {
                    entries: 1,
                    bytes: CONTAINER_OVERHEAD_BYTES + entries.len() as u64,
                },
                |acc, entry| {
                    acc.saturating_add(Usage {
                        entries: 0,
                        bytes: entry.key.len() as u64 + KEY_OVERHEAD_BYTES,
                    })
                    .saturating_add(entry.node.usage())
                },
            );
            Node::Object(Arc::new(ObjectNode { entries, usage }))
        }
    }
}

impl From<serde_json::Value> for Node {
    fn from(value: serde_json::Value) -> Self {
        Self::from_value_reporting(value).0
    }
}

impl Serialize for Node {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(flag) => serializer.serialize_bool(*flag),
            Self::Number(number) => number.serialize(serializer),
            Self::String(text) => serializer.serialize_str(text),
            Self::Array(array) => {
                let mut seq = serializer.serialize_seq(Some(array.items.len()))?;
                for item in &array.items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Object(object) => {
                let mut map = serializer.serialize_map(Some(object.entries.len()))?;
                for entry in object.entries.values() {
                    map.serialize_entry(&*entry.key, &entry.node)?;
                }
                map.end()
            }
        }
    }
}

impl fmt::Display for Node {
    /// Compact JSON, byte-identical to the `serde_json::Value` form of the same tree.
    /// Delegates to [`Node::to_json`] so there is exactly one serialization path; a streaming
    /// formatter adapter would save this call's intermediate `String` but pays per-chunk
    /// UTF-8 revalidation, which is the wrong trade for the hot flush path — hot callers use
    /// `to_json` directly.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json())
    }
}

/// Structural equality against a `serde_json::Value` (fold-insensitive on nothing — keys
/// compare by their stored spelling, order-insensitively like `Value`'s own object equality).
impl PartialEq<serde_json::Value> for Node {
    fn eq(&self, other: &serde_json::Value) -> bool {
        match (self, other) {
            (Self::Null, serde_json::Value::Null) => true,
            (Self::Bool(a), serde_json::Value::Bool(b)) => a == b,
            (Self::Number(a), serde_json::Value::Number(b)) => a == b,
            (Self::String(a), serde_json::Value::String(b)) => &**a == b.as_str(),
            (Self::Array(a), serde_json::Value::Array(b)) => {
                a.items.len() == b.len()
                    && a.items
                        .iter()
                        .zip(b.iter())
                        .all(|(item, other)| item == other)
            }
            (Self::Object(a), serde_json::Value::Object(b)) => {
                a.entries.len() == b.len()
                    && a.entries
                        .values()
                        .all(|entry| b.get(&*entry.key).is_some_and(|child| entry.node == *child))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Recompute a subtree's usage from scratch — the invariant the memoized values must
    /// match after any sequence of mutations.
    fn measured(node: &Node) -> Usage {
        match node {
            Node::Array(array) => array.items.iter().fold(
                Usage {
                    entries: 1,
                    bytes: CONTAINER_OVERHEAD_BYTES + array.items.len() as u64,
                },
                |acc, item| acc.saturating_add(measured(item)),
            ),
            Node::Object(object) => object.entries.values().fold(
                Usage {
                    entries: 1,
                    bytes: CONTAINER_OVERHEAD_BYTES + object.entries.len() as u64,
                },
                |acc, entry| {
                    acc.saturating_add(Usage {
                        entries: 0,
                        bytes: entry.key.len() as u64 + KEY_OVERHEAD_BYTES,
                    })
                    .saturating_add(measured(&entry.node))
                },
            ),
            scalar => scalar.usage(),
        }
    }

    fn segments(path: &[&str]) -> Vec<String> {
        path.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn serialization_is_byte_identical_to_value() {
        let value = json!({
            "Z": 1, "a": -2.5, "m": [true, null, "x\"y\u{1f}\\z", 1e30, 18_446_744_073_709_551_615_u64],
            "nested": { "Kx": { "deep": [0.1, -0] } },
            "s": "plain"
        });
        let node = Node::from(value.clone());
        assert_eq!(node.to_string(), value.to_string());
        assert_eq!(node.to_value().to_string(), value.to_string());
        assert!(
            node == value,
            "structural equality matches the source value"
        );
    }

    #[test]
    fn number_usage_is_the_serialized_length() {
        // The counting-writer length must equal what serialization actually emits, for every
        // internal `Number` representation (u64 / i64 / f64).
        for value in [
            json!(0),
            json!(7),
            json!(-1),
            json!(18_446_744_073_709_551_615_u64),
            json!(-9_223_372_036_854_775_808_i64),
            json!(-2.5),
            json!(0.1),
            json!(1e30),
        ] {
            let node = Node::from(value);
            assert_eq!(
                node.usage().bytes,
                node.to_json().len() as u64,
                "usage bytes match serialization for {node}"
            );
        }
    }

    #[test]
    fn conversion_reports_and_collapses_fold_duplicate_keys() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{ "Foo": 1, "foo": 2, "bar": 3 }"#).expect("parse");
        let (node, collapsed) = Node::from_value_reporting(value);
        assert!(collapsed);
        assert_eq!(
            node.to_string(),
            r#"{"Foo":2,"bar":3}"#,
            "first spelling, last value"
        );
        let (_, clean) = Node::from_value_reporting(json!({ "a": 1, "b": { "C": 1, "d": 2 } }));
        assert!(!clean);
    }

    #[test]
    fn lookups_fold_case_without_respelling() {
        let node = Node::from(json!({ "Char": { "Vitals": { "hp": 10 } } }));
        let hp = node
            .extract(&segments(&["CHAR", "vitals", "HP"]))
            .expect("fold-insensitive walk");
        assert!(*hp == json!(10));
        // A long mixed-case segment exercises the fold-hash heap fallback.
        let long_key = "K".repeat(FOLD_STACK_BYTES + 8);
        let node = Node::from(json!({ long_key.clone(): 1 }));
        assert!(node.get(&long_key.to_ascii_uppercase()).is_some());
        assert!(node.get(&long_key.to_ascii_lowercase()).is_some());
    }

    #[test]
    fn set_at_matches_folded_keys_and_conjures_spines() {
        let mut node = Node::from(json!({ "Char": { "Vitals": { "hp": 10 } } }));
        node.set_at(&segments(&["CHAR", "VITALS", "hp"]), Node::from(json!(11)));
        assert_eq!(node.to_string(), r#"{"Char":{"Vitals":{"hp":11}}}"#);
        node.set_at(
            &segments(&["Char", "Vitals", "mp", "deep"]),
            Node::from(json!(1)),
        );
        assert_eq!(
            node.to_string(),
            r#"{"Char":{"Vitals":{"hp":11,"mp":{"deep":1}}}}"#
        );
        // Writing through a scalar replaces it with an object spine.
        node.set_at(
            &segments(&["Char", "Vitals", "hp", "sub"]),
            Node::from(json!(2)),
        );
        assert_eq!(
            node.to_string(),
            r#"{"Char":{"Vitals":{"hp":{"sub":2},"mp":{"deep":1}}}}"#
        );
        assert_eq!(
            measured(&node),
            node.usage(),
            "memoized usage tracks the tree"
        );
    }

    #[test]
    fn memoized_usage_matches_full_measurement_through_mutation() {
        let mut node = Node::empty_object();
        assert_eq!(measured(&node), node.usage());
        node.set_at(&segments(&["a", "b", "c"]), Node::from(json!([1, 2, 3])));
        assert_eq!(measured(&node), node.usage());
        node.set_at(&segments(&["a", "b", "c"]), Node::from(json!("shorter")));
        assert_eq!(measured(&node), node.usage());
        node.set_at(
            &segments(&["a", "b"]),
            Node::from(json!({ "x": 1, "Y": [null, true] })),
        );
        assert_eq!(measured(&node), node.usage());
        node.set_at(&[], Node::from(json!(7)));
        assert_eq!(measured(&node), node.usage());
    }

    #[test]
    fn set_at_shares_untouched_siblings_and_mutates_unique_spines_in_place() {
        let mut head = Node::from(json!({
            "left": { "deep": [1, 2, 3] },
            "right": { "deep": [4, 5, 6] }
        }));
        let pinned = head.clone(); // an O(1) clone, like a binding cell retaining a snapshot
        head.set_at(&segments(&["right", "deep"]), Node::from(json!(0)));
        // The written spine diverged; the untouched sibling is still the same allocation.
        let (Node::Object(head_root), Node::Object(pinned_root)) = (&head, &pinned) else {
            panic!("roots are objects");
        };
        assert!(
            !Arc::ptr_eq(head_root, pinned_root),
            "the written spine diverged"
        );
        let (Some(Node::Object(head_left)), Some(Node::Object(pinned_left))) =
            (head.get("left"), pinned.get("left"))
        else {
            panic!("left subtrees are objects");
        };
        assert!(
            Arc::ptr_eq(head_left, pinned_left),
            "the untouched sibling subtree stays shared"
        );
        assert!(
            pinned
                .get("right")
                .is_some_and(|n| *n == json!({ "deep": [4, 5, 6] }))
        );
        assert!(
            head.get("right")
                .is_some_and(|n| *n == json!({ "deep": 0 }))
        );
        // With the pin dropped the next write finds unique spines and mutates in place.
        drop(pinned);
        let before = match head.get("right") {
            Some(Node::Object(right)) => Arc::as_ptr(right),
            _ => panic!("right is an object"),
        };
        head.set_at(&segments(&["right", "deep"]), Node::from(json!(9)));
        let after = match head.get("right") {
            Some(Node::Object(right)) => Arc::as_ptr(right),
            _ => panic!("right is an object"),
        };
        assert_eq!(
            before, after,
            "a uniquely-owned spine node is reused in place"
        );
    }

    #[test]
    fn entry_order_is_preserved_as_published_across_writes() {
        let mut node = Node::from(json!({ "z": 1, "a": 2, "m": 3 }));
        node.set_at(&segments(&["A"]), Node::from(json!(9)));
        node.set_at(&segments(&["new"]), Node::from(json!(4)));
        assert_eq!(node.to_string(), r#"{"z":1,"a":9,"m":3,"new":4}"#);
        let object = node.as_object().expect("an object");
        assert_eq!(object.keys().collect::<Vec<_>>(), ["z", "a", "m", "new"]);
    }
}
