//! What a UUID identity costs on the Rust/V8 boundary, per candidate wire
//! shape. Run with `cargo run -p smudgy_bench --example id_wire --release`.
//!
//! Map identities (`AreaId`, `ExitId`, `ConnectionId`, `AtlasId`) used to cross
//! the op boundary as a `(u64, u64)` tuple, which `serde_v8` renders as a
//! 2-element JS array whose halves become `BigInt` above
//! `Number.MAX_SAFE_INTEGER`. The RFC-4122 variant bits put the LOW half above
//! that bound always; the high half lands under it about one id in 2048 and
//! arrives as a `Number`, so the pair was polymorphic as well. That shape was
//! awkward in script (no `===`, no `Map` key, `JSON.stringify` throws) and it
//! was not even cheap: every id is three V8 heap objects, two BigInts and the
//! array holding them. This harness priced the alternatives against that
//! baseline, and settled the two decisions the change rests on: ids are
//! canonical UUID strings, and they arrive as `#[string]` op params rather than
//! through `serde` (compare the two `inbound only` string cells -- serde's
//! string path allocates, transcodes, and blocks V8's fast-call path).
//!
//! Everything runs on a bare `JsRuntime` with a purpose-built op extension --
//! no session, no mapper, no snapshot -- so the figures isolate identity
//! marshalling and nothing else. Each op does the same real work: reconstruct
//! a `Uuid` from what JS sent, and hand a `Uuid` back in the same encoding.
//!
//! Cells:
//! - `roundtrip/*`: one id in, one id out, per call, over a 256-id corpus.
//!   READ THIS GROUP WITH CARE: `nop_smi` and `smi` are `#[op2(fast)]` while the
//!   cells that return a `v8::Local` or go through serde cannot be, and op
//!   dispatch mode is worth tens of ns on its own. Compare only cells of the
//!   same mode; `nop_smi` bounds the crossing, not the encoding. `pair_fast` is
//!   a LOWER BOUND rather than a round trip -- a fast op cannot return a pair,
//!   so it hands back one half.
//! - `inbound only/*`: the commonest mapper op shape, a setter that takes an
//!   id and returns a scalar (`setRoomTitle`, `createRoom`, `deleteArea`).
//!   Only half the encoding is paid, and the op stays fast-callable if its
//!   params allow one -- which a `serde` tuple never does.
//! - `bulk/*`: `BULK_IDS` ids returned in one array from one call, the shape
//!   `getRoomExits`/`getAreaConnections` pay: the id count dominates the
//!   record count, and ids repeat heavily (every exit in an area names the
//!   same `from_area_id`).
//! - `js/*`: no ops at all -- the script-side tax each encoding imposes on the
//!   two things authors actually do with an id, an equality test and a `Map`
//!   lookup. `pair` pays the elementwise compare and the `${hi}:${lo}` key
//!   build that every mapper package in `packages/` currently hand-rolls.
//!
//! Encodings under test: `pair` (status quo, through `serde_v8`), `pair_v8`
//! (the same `[hi, lo]` array built by hand, which prices the encoding apart
//! from the serializer), `pair_fast` (the halves as declared `#[bigint]`
//! params), `u128` (one 128-bit BigInt), `str` (canonical hyphenated string),
//! `str_stack` (the same, reading into a stack buffer instead of a `String`),
//! `str_interned`/`str_best` (memoized per isolate as a `v8::Global` -- a hit
//! still costs a hash and a handle, and the harness shows the memo losing to a
//! fresh 36-byte string on the single-id path), `buf` (16-byte Uint8Array), and
//! `smi` (a dense u32 handle into a per-isolate id table -- the fastest thing
//! that could replace an id, though its outbound side pays a hash to intern).
//! `str_stack` is the shape that actually shipped: stack-buffer parse in, a
//! `Normal` one-byte string out, no memo.
//!
//! Every encoding is gated on a round-trip assertion before any timing runs: a
//! cell that dropped or truncated an identity would otherwise just look fast.
//! The gate checks one id per encoding, so it catches a wrong or truncated
//! encoding, not a cell that returns a constant.
//!
//! Timing is wall-clock around `execute_script` on a warmed loop; every cell
//! runs `WARMUP_PASSES` unmeasured passes first so V8 has tiered the loop up
//! and the fast-call path is live where the op signature allows one.

use std::{collections::HashMap, hint::black_box, time::Instant};

use deno_core::{FastString, JsRuntime, OpState, RuntimeOptions, op2, v8};
use smudgy_cloud::Uuid;

/// Distinct ids in the corpus every cell draws from. Big enough that the
/// caches under test cannot degenerate into a single hot entry, small enough
/// to stay resident like a real area's identity working set.
const CORPUS: usize = 256;
/// Op calls per `roundtrip` pass.
const ROUNDTRIP_CALLS: usize = 200_000;
/// Ids returned by one `bulk` call -- roughly a large area's exit identity load.
const BULK_IDS: usize = 1_024;
/// `bulk` calls per pass.
const BULK_CALLS: usize = 2_000;
/// Iterations per `js` pass.
const JS_ITERS: usize = 1_000_000;
/// Unmeasured passes before timing, so V8 has optimized the loop.
const WARMUP_PASSES: usize = 3;

// ---- host state -------------------------------------------------------------

/// The per-isolate id table behind the `smi` encoding: a dense handle space
/// plus the reverse lookup a host needs to answer "which handle is this id?".
#[derive(Default)]
struct HandleTable {
    ids: Vec<Uuid>,
    index: HashMap<Uuid, u32>,
}

impl HandleTable {
    fn intern(&mut self, id: Uuid) -> u32 {
        if let Some(handle) = self.index.get(&id) {
            return *handle;
        }
        let handle = self.ids.len() as u32;
        self.ids.push(id);
        self.index.insert(id, handle);
        handle
    }

    fn resolve(&self, handle: u32) -> Uuid {
        self.ids[handle as usize]
    }
}

/// Memoized canonical-string spellings, one `v8::Global` per id. A hit is a
/// `Local::new` off an existing handle -- no formatting, no V8 allocation.
#[derive(Default)]
struct StringCache {
    strings: HashMap<Uuid, v8::Global<v8::String>>,
}

impl StringCache {
    fn get<'a>(&mut self, scope: &mut v8::PinScope<'a, '_>, id: Uuid) -> v8::Local<'a, v8::String> {
        if let Some(cached) = self.strings.get(&id) {
            return v8::Local::new(scope, cached);
        }
        let local = new_uuid_string(scope, id, v8::NewStringType::Internalized);
        self.strings.insert(id, v8::Global::new(scope, local));
        local
    }
}

/// The corpus every cell addresses by index, so no cell pays for id generation.
struct Corpus {
    ids: Vec<Uuid>,
}

impl Corpus {
    fn at(&self, i: usize) -> Uuid {
        self.ids[i % self.ids.len()]
    }
}

// ---- encodings --------------------------------------------------------------

/// A canonical hyphenated spelling built straight into V8 from a stack buffer:
/// one-byte (the alphabet is ASCII), no intermediate `String`.
fn new_uuid_string<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    id: Uuid,
    kind: v8::NewStringType,
) -> v8::Local<'a, v8::String> {
    let mut buf = [0u8; uuid::fmt::Hyphenated::LENGTH];
    let text = id.hyphenated().encode_lower(&mut buf);
    v8::String::new_from_one_byte(scope, text.as_bytes(), kind).expect("36-byte string fits")
}

fn read_uuid_string(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Uuid {
    let text = value.to_rust_string_lossy(scope);
    Uuid::parse_str(&text).expect("corpus ids are canonical")
}

/// The same read with no heap on the way in: a canonical id is 36 one-byte
/// characters, so it lands in a stack buffer.
fn read_uuid_string_stack(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Uuid {
    let text = v8::Local::<v8::String>::try_from(value).expect("id is a string");
    let mut buf = [0u8; uuid::fmt::Hyphenated::LENGTH];
    text.write_one_byte_v2(scope, 0, &mut buf, v8::WriteFlags::empty());
    Uuid::try_parse_ascii(&buf).expect("corpus ids are canonical")
}

/// The status-quo `[hi, lo]` shape built by hand rather than through
/// `serde_v8`, so the encoding can be priced apart from the serializer.
fn new_uuid_pair<'a>(scope: &mut v8::PinScope<'a, '_>, id: Uuid) -> v8::Local<'a, v8::Array> {
    let (hi, lo) = id.as_u64_pair();
    let hi = v8::BigInt::new_from_u64(scope, hi);
    let lo = v8::BigInt::new_from_u64(scope, lo);
    // `new_with_elements`, not `Array::new` + `set_index`: the latter walks the
    // generic property-store path per element and would price the harness's
    // array building rather than the encoding under test. This is the path
    // `serde_v8` itself takes, so every builder here is on equal footing.
    v8::Array::new_with_elements(scope, &[hi.into(), lo.into()])
}

fn read_uuid_pair(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> Uuid {
    let array = v8::Local::<v8::Array>::try_from(value).expect("id is a 2-element array");
    let hi = array.get_index(scope, 0).expect("hi half present");
    let lo = array.get_index(scope, 1).expect("lo half present");
    let half = |value: v8::Local<v8::Value>| {
        v8::Local::<v8::BigInt>::try_from(value)
            .expect("half is a BigInt")
            .u64_value()
            .0
    };
    Uuid::from_u64_pair(half(hi), half(lo))
}

fn new_uuid_bigint<'a>(scope: &mut v8::PinScope<'a, '_>, id: Uuid) -> v8::Local<'a, v8::BigInt> {
    let (hi, lo) = id.as_u64_pair();
    v8::BigInt::new_from_words(scope, false, &[lo, hi]).expect("two words is in range")
}

fn read_uuid_bigint(value: v8::Local<v8::Value>) -> Uuid {
    let bigint = v8::Local::<v8::BigInt>::try_from(value).expect("id is a BigInt");
    let mut words = [0u64; 2];
    let (_sign, words) = bigint.to_words_array(&mut words);
    let lo = words.first().copied().unwrap_or(0);
    let hi = words.get(1).copied().unwrap_or(0);
    Uuid::from_u64_pair(hi, lo)
}

// ---- ops --------------------------------------------------------------------

/// The floor: an op crossing carrying one `u32`, no identity work.
#[op2(fast)]
#[smi]
fn op_id_nop_smi(#[smi] value: u32) -> u32 {
    black_box(value)
}

/// Status quo: `(u64, u64)` through `serde_v8` -- a JS array of two BigInts.
#[op2]
#[serde]
fn op_id_pair(#[serde] id: (u64, u64)) -> (u64, u64) {
    black_box(Uuid::from_u64_pair(id.0, id.1)).as_u64_pair()
}

/// The status-quo halves as declared fast-call `#[bigint]` params instead of a
/// serde array -- the cheapest the pair shape can be made inbound.
#[op2(fast)]
#[bigint]
fn op_id_pair_fast(#[bigint] hi: u64, #[bigint] lo: u64) -> u64 {
    black_box(Uuid::from_u64_pair(hi, lo)).as_u64_pair().0
}

/// The status-quo shape without `serde_v8`: the same `[hi, lo]` array, built
/// and read by hand. The gap to `op_id_pair` is the serializer, not the wire.
#[op2]
fn op_id_pair_v8<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    id: v8::Local<v8::Value>,
) -> v8::Local<'a, v8::Array> {
    let parsed = black_box(read_uuid_pair(scope, id));
    new_uuid_pair(scope, parsed)
}

/// One 128-bit BigInt.
#[op2]
fn op_id_u128<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    id: v8::Local<v8::Value>,
) -> v8::Local<'a, v8::BigInt> {
    new_uuid_bigint(scope, black_box(read_uuid_bigint(id)))
}

/// Canonical string, formatted per call in both directions.
#[op2]
fn op_id_str<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    id: v8::Local<v8::Value>,
) -> v8::Local<'a, v8::String> {
    let parsed = black_box(read_uuid_string(scope, id));
    new_uuid_string(scope, parsed, v8::NewStringType::Normal)
}

/// Canonical string with no heap allocation in either direction -- the shape a
/// production implementation would actually ship.
#[op2]
fn op_id_str_stack<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    id: v8::Local<v8::Value>,
) -> v8::Local<'a, v8::String> {
    let parsed = black_box(read_uuid_string_stack(scope, id));
    new_uuid_string(scope, parsed, v8::NewStringType::Normal)
}

/// Both string optimizations at once -- stack-buffer read in, per-isolate memo
/// out. This is the shape a production string encoding would actually ship, so
/// it is the cell the decision rests on.
#[op2]
fn op_id_str_best<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    id: v8::Local<v8::Value>,
) -> v8::Local<'a, v8::String> {
    let parsed = black_box(read_uuid_string_stack(scope, id));
    let mut cache = std::mem::take(state.borrow_mut::<StringCache>());
    let local = cache.get(scope, parsed);
    *state.borrow_mut::<StringCache>() = cache;
    local
}

/// Canonical string with the per-isolate memo on the outbound side.
#[op2]
fn op_id_str_interned<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    id: v8::Local<v8::Value>,
) -> v8::Local<'a, v8::String> {
    let parsed = black_box(read_uuid_string(scope, id));
    let mut cache = std::mem::take(state.borrow_mut::<StringCache>());
    let local = cache.get(scope, parsed);
    *state.borrow_mut::<StringCache>() = cache;
    local
}

/// 16 raw bytes as a Uint8Array.
#[op2]
#[buffer]
fn op_id_buf(#[buffer] id: &[u8]) -> Vec<u8> {
    black_box(Uuid::from_slice(id).expect("16 bytes"))
        .as_bytes()
        .to_vec()
}

/// A dense u32 handle resolved through the per-isolate id table.
#[op2(fast)]
#[smi]
fn op_id_smi(state: &mut OpState, #[smi] handle: u32) -> u32 {
    let id = black_box(state.borrow::<HandleTable>().resolve(handle));
    state.borrow_mut::<HandleTable>().intern(id)
}

// ---- inbound-only ops -------------------------------------------------------
//
// The commonest mapper op shape is a setter -- `setRoomTitle(area, room, ...)`
// -- which takes an id and returns a scalar. Only the inbound half of the
// encoding is paid, and the op can stay fast-callable if its params allow it,
// which the `serde` pair never does.

/// Status-quo inbound: a serde `(u64, u64)`, which forces the slow path.
#[op2]
#[smi]
fn op_id_in_pair(#[serde] id: (u64, u64)) -> u32 {
    black_box(Uuid::from_u64_pair(id.0, id.1)).as_fields().0
}

/// Inbound as two declared `#[bigint]` halves -- fast-callable.
#[op2(fast)]
#[smi]
fn op_id_in_pair_fast(#[bigint] hi: u64, #[bigint] lo: u64) -> u32 {
    black_box(Uuid::from_u64_pair(hi, lo)).as_fields().0
}

/// Inbound as a canonical string through deno's own `#[string]` conversion --
/// also fast-callable, since the id alphabet is one-byte.
#[op2(fast)]
#[smi]
fn op_id_in_str(#[string] id: &str) -> u32 {
    black_box(Uuid::try_parse(id).expect("corpus ids are canonical"))
        .as_fields()
        .0
}

/// Inbound as a canonical string carried through `serde_v8` rather than
/// deno's `#[string]` conversion -- what a `ScriptUuid` newtype in a serde
/// payload actually costs, and the reason the ops can stay on `#[serde]`.
#[op2]
#[smi]
fn op_id_in_str_serde(#[serde] id: SerdeUuid) -> u32 {
    black_box(id.0).as_fields().0
}

/// Inbound as a 128-bit BigInt -- also fast-callable in this direction.
#[op2(fast)]
#[smi]
fn op_id_in_u128(id: v8::Local<v8::Value>) -> u32 {
    black_box(read_uuid_bigint(id)).as_fields().0
}

/// Inbound as a handle: a `Vec` index, no hashing -- the true floor.
#[op2(fast)]
#[smi]
fn op_id_in_smi(state: &mut OpState, #[smi] handle: u32) -> u32 {
    black_box(state.borrow::<HandleTable>().resolve(handle))
        .as_fields()
        .0
}

/// Mirrors the shipping `ScriptUuid`: a canonical string on the wire, parsed
/// in the visitor with no intermediate `String`.
struct SerdeUuid(Uuid);

impl<'de> serde::Deserialize<'de> for SerdeUuid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = SerdeUuid;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a canonical UUID string")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Uuid::try_parse(value).map(SerdeUuid).map_err(E::custom)
            }
        }
        d.deserialize_str(V)
    }
}

// ---- corpus ops (JS-side setup, never timed) --------------------------------

#[op2]
#[serde]
fn op_id_corpus_pair(state: &mut OpState) -> Vec<(u64, u64)> {
    state
        .borrow::<Corpus>()
        .ids
        .iter()
        .map(Uuid::as_u64_pair)
        .collect()
}

#[op2]
fn op_id_corpus_u128<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
) -> v8::Local<'a, v8::Array> {
    let ids = state.borrow::<Corpus>().ids.clone();
    let elements: Vec<v8::Local<v8::Value>> = ids
        .into_iter()
        .map(|id| new_uuid_bigint(scope, id).into())
        .collect();
    v8::Array::new_with_elements(scope, &elements)
}

#[op2]
fn op_id_corpus_str<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
) -> v8::Local<'a, v8::Array> {
    let ids = state.borrow::<Corpus>().ids.clone();
    let elements: Vec<v8::Local<v8::Value>> = ids
        .into_iter()
        .map(|id| new_uuid_string(scope, id, v8::NewStringType::Normal).into())
        .collect();
    v8::Array::new_with_elements(scope, &elements)
}

#[op2]
#[serde]
fn op_id_corpus_buf(state: &mut OpState) -> Vec<Vec<u8>> {
    state
        .borrow::<Corpus>()
        .ids
        .iter()
        .map(|id| id.as_bytes().to_vec())
        .collect()
}

#[op2]
#[serde]
fn op_id_corpus_smi(state: &mut OpState) -> Vec<u32> {
    let ids = state.borrow::<Corpus>().ids.clone();
    let table = state.borrow_mut::<HandleTable>();
    ids.into_iter().map(|id| table.intern(id)).collect()
}

// ---- bulk ops ---------------------------------------------------------------

/// The bulk shape a listing op pays: `n` ids in one array. Ids repeat over the
/// corpus exactly as an area's exits repeat their `from_area_id`.
#[op2]
#[serde]
fn op_id_bulk_pair(state: &mut OpState, #[smi] n: u32) -> Vec<(u64, u64)> {
    let corpus = state.borrow::<Corpus>();
    (0..n as usize)
        .map(|i| corpus.at(i).as_u64_pair())
        .collect()
}

/// The status-quo bulk shape without `serde_v8`.
#[op2]
fn op_id_bulk_pair_v8<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    #[smi] n: u32,
) -> v8::Local<'a, v8::Array> {
    let ids: Vec<Uuid> = {
        let corpus = state.borrow::<Corpus>();
        (0..n as usize).map(|i| corpus.at(i)).collect()
    };
    let elements: Vec<v8::Local<v8::Value>> = ids
        .into_iter()
        .map(|id| new_uuid_pair(scope, id).into())
        .collect();
    v8::Array::new_with_elements(scope, &elements)
}

#[op2]
fn op_id_bulk_u128<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    #[smi] n: u32,
) -> v8::Local<'a, v8::Array> {
    let ids: Vec<Uuid> = {
        let corpus = state.borrow::<Corpus>();
        (0..n as usize).map(|i| corpus.at(i)).collect()
    };
    let elements: Vec<v8::Local<v8::Value>> = ids
        .into_iter()
        .map(|id| new_uuid_bigint(scope, id).into())
        .collect();
    v8::Array::new_with_elements(scope, &elements)
}

#[op2]
fn op_id_bulk_str<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    #[smi] n: u32,
) -> v8::Local<'a, v8::Array> {
    let ids: Vec<Uuid> = {
        let corpus = state.borrow::<Corpus>();
        (0..n as usize).map(|i| corpus.at(i)).collect()
    };
    let elements: Vec<v8::Local<v8::Value>> = ids
        .into_iter()
        .map(|id| new_uuid_string(scope, id, v8::NewStringType::Normal).into())
        .collect();
    v8::Array::new_with_elements(scope, &elements)
}

#[op2]
fn op_id_bulk_str_interned<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    #[smi] n: u32,
) -> v8::Local<'a, v8::Array> {
    let ids: Vec<Uuid> = {
        let corpus = state.borrow::<Corpus>();
        (0..n as usize).map(|i| corpus.at(i)).collect()
    };
    let mut cache = std::mem::take(state.borrow_mut::<StringCache>());
    let elements: Vec<v8::Local<v8::Value>> = ids
        .into_iter()
        .map(|id| cache.get(scope, id).into())
        .collect();
    *state.borrow_mut::<StringCache>() = cache;
    v8::Array::new_with_elements(scope, &elements)
}

#[op2]
fn op_id_bulk_buf<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    #[smi] n: u32,
) -> v8::Local<'a, v8::Array> {
    let ids: Vec<Uuid> = {
        let corpus = state.borrow::<Corpus>();
        (0..n as usize).map(|i| corpus.at(i)).collect()
    };
    // Real `Uint8Array`s. Through `#[serde]` a `Vec<Vec<u8>>` becomes an Array of
    // Arrays of Numbers -- sixteen elements per id, and not the encoding named.
    let elements: Vec<v8::Local<v8::Value>> = ids
        .into_iter()
        .map(|id| {
            let store = v8::ArrayBuffer::new_backing_store_from_vec(id.as_bytes().to_vec());
            let buffer = v8::ArrayBuffer::with_backing_store(scope, &store.make_shared());
            v8::Uint8Array::new(scope, buffer, 0, 16)
                .expect("16-byte view fits")
                .into()
        })
        .collect();
    v8::Array::new_with_elements(scope, &elements)
}

#[op2]
fn op_id_bulk_smi<'a>(
    scope: &mut v8::PinScope<'a, '_>,
    state: &mut OpState,
    #[smi] n: u32,
) -> v8::Local<'a, v8::Array> {
    let ids: Vec<Uuid> = {
        let corpus = state.borrow::<Corpus>();
        (0..n as usize).map(|i| corpus.at(i)).collect()
    };
    let table = state.borrow_mut::<HandleTable>();
    let handles: Vec<u32> = ids.into_iter().map(|id| table.intern(id)).collect();
    // Hand-built like every other bulk cell: routing this one through `#[serde]`
    // while its rivals use `new_with_elements` would price the serializer, not
    // the encoding -- and it inverted this group's ordering when it did.
    let elements: Vec<v8::Local<v8::Value>> = handles
        .into_iter()
        .map(|h| v8::Integer::new_from_unsigned(scope, h).into())
        .collect();
    v8::Array::new_with_elements(scope, &elements)
}

deno_core::extension!(
    id_wire,
    ops = [
        op_id_nop_smi,
        op_id_pair,
        op_id_pair_fast,
        op_id_pair_v8,
        op_id_u128,
        op_id_str,
        op_id_str_stack,
        op_id_str_interned,
        op_id_str_best,
        op_id_buf,
        op_id_smi,
        op_id_in_pair,
        op_id_in_pair_fast,
        op_id_in_str,
        op_id_in_str_serde,
        op_id_in_u128,
        op_id_in_smi,
        op_id_corpus_pair,
        op_id_corpus_u128,
        op_id_corpus_str,
        op_id_corpus_buf,
        op_id_corpus_smi,
        op_id_bulk_pair,
        op_id_bulk_pair_v8,
        op_id_bulk_u128,
        op_id_bulk_str,
        op_id_bulk_str_interned,
        op_id_bulk_buf,
        op_id_bulk_smi,
    ],
    state = |state| {
        // A fixed seed keeps the corpus identical run to run; the v4 layout
        // keeps both halves above 2^53, which is what forces BigInt in the
        // status quo.
        let mut seed = 0x5EED_1234_ABCD_0001u64;
        let mut ids = Vec::with_capacity(CORPUS);
        for _ in 0..CORPUS {
            let mut bytes = [0u8; 16];
            for chunk in bytes.chunks_mut(8) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                chunk.copy_from_slice(&seed.to_le_bytes());
            }
            ids.push(uuid::Builder::from_random_bytes(bytes).into_uuid());
        }
        state.put(Corpus { ids });
        state.put(HandleTable::default());
        state.put(StringCache::default());
    },
);

// ---- harness ----------------------------------------------------------------

/// Run one named cell: warm it, then time `PASSES` measured passes and keep
/// the fastest (the least scheduler noise), reported per identity.
fn cell(runtime: &mut JsRuntime, label: &str, call: &str, ops_per_pass: usize) {
    const PASSES: usize = 5;
    let script = format!("{call};");
    for _ in 0..WARMUP_PASSES {
        runtime
            .execute_script("<id_wire warmup>", FastString::from(script.clone()))
            .expect("warmup pass runs");
    }
    let mut best = f64::MAX;
    for _ in 0..PASSES {
        let start = Instant::now();
        runtime
            .execute_script("<id_wire pass>", FastString::from(script.clone()))
            .expect("timed pass runs");
        best = best.min(start.elapsed().as_secs_f64());
    }
    let per_op_ns = best / ops_per_pass as f64 * 1e9;
    println!("  {label:<26} {per_op_ns:>8.1} ns/id");
}

const PRELUDE: &str = r#"
const {
    op_id_nop_smi, op_id_pair, op_id_pair_fast, op_id_pair_v8, op_id_u128,
    op_id_str, op_id_str_stack, op_id_str_interned, op_id_str_best, op_id_buf, op_id_smi,
    op_id_in_pair, op_id_in_pair_fast, op_id_in_str, op_id_in_str_serde, op_id_in_u128, op_id_in_smi,
    op_id_corpus_pair, op_id_corpus_u128, op_id_corpus_str, op_id_corpus_buf,
    op_id_corpus_smi,
    op_id_bulk_pair, op_id_bulk_pair_v8, op_id_bulk_u128, op_id_bulk_str,
    op_id_bulk_str_interned, op_id_bulk_buf, op_id_bulk_smi,
} = Deno.core.ops;

globalThis.cPair = op_id_corpus_pair();
globalThis.cU128 = op_id_corpus_u128();
globalThis.cStr = op_id_corpus_str();
globalThis.cBuf = op_id_corpus_buf().map((b) => new Uint8Array(b));
globalThis.cSmi = op_id_corpus_smi();
globalThis.L = cPair.length;

globalThis.rtNop = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += op_id_nop_smi(i & 1023); } return a; };
globalThis.rtPair = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_pair(cPair[i % L]) === null) a++; } return a; };
globalThis.rtPairFast = (n) => { let a = 0; for (let i = 0; i < n; i++) { const p = cPair[i % L]; if (op_id_pair_fast(p[0], p[1]) === 0n) a++; } return a; };
globalThis.rtPairV8 = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_pair_v8(cPair[i % L]) === null) a++; } return a; };
globalThis.rtU128 = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_u128(cU128[i % L]) === 0n) a++; } return a; };
globalThis.rtStr = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_str(cStr[i % L]) === "") a++; } return a; };
globalThis.rtStrStack = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_str_stack(cStr[i % L]) === "") a++; } return a; };
globalThis.rtStrInt = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_str_interned(cStr[i % L]) === "") a++; } return a; };
globalThis.rtStrBest = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_str_best(cStr[i % L]) === "") a++; } return a; };
globalThis.rtBuf = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (op_id_buf(cBuf[i % L]) === null) a++; } return a; };
globalThis.rtSmi = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += op_id_smi(cSmi[i % L]); } return a; };

// Every encoding must hand back the id it was given: a cell that silently
// dropped or truncated an identity would otherwise just look fast.
globalThis.sanity = () => {
    const fail = (what) => { throw new Error(`id_wire: ${what} does not round-trip`); };
    const p = op_id_pair(cPair[0]);
    if (p[0] !== cPair[0][0] || p[1] !== cPair[0][1]) fail("pair");
    const pv = op_id_pair_v8(cPair[0]);
    if (pv[0] !== cPair[0][0] || pv[1] !== cPair[0][1]) fail("pair_v8");
    if (op_id_pair_fast(cPair[0][0], cPair[0][1]) !== cPair[0][0]) fail("pair_fast");
    if (op_id_u128(cU128[0]) !== cU128[0]) fail("u128");
    if (op_id_str(cStr[0]) !== cStr[0]) fail("str");
    if (op_id_str_stack(cStr[0]) !== cStr[0]) fail("str_stack");
    if (op_id_str_interned(cStr[0]) !== cStr[0]) fail("str_interned");
    if (op_id_str_best(cStr[0]) !== cStr[0]) fail("str_best");
    const b = op_id_buf(cBuf[0]);
    if (b.length !== 16 || b.some((v, i) => v !== cBuf[0][i])) fail("buf");
    if (op_id_smi(cSmi[0]) !== cSmi[0]) fail("smi");
    const want = op_id_in_smi(cSmi[0]);
    if (op_id_in_pair(cPair[0]) !== want) fail("in_pair");
    if (op_id_in_pair_fast(cPair[0][0], cPair[0][1]) !== want) fail("in_pair_fast");
    if (op_id_in_str(cStr[0]) !== want) fail("in_str");
    if (op_id_in_str_serde(cStr[0]) !== want) fail("in_str_serde");
    if (op_id_in_u128(cU128[0]) !== want) fail("in_u128");
    // The bulk builders must agree with the corpus they draw from.
    if (op_id_bulk_pair(4)[3][0] !== cPair[3][0]) fail("bulk_pair");
    if (op_id_bulk_pair_v8(4)[3][0] !== cPair[3][0]) fail("bulk_pair_v8");
    if (op_id_bulk_u128(4)[3] !== cU128[3]) fail("bulk_u128");
    if (op_id_bulk_str(4)[3] !== cStr[3]) fail("bulk_str");
    if (op_id_bulk_str_interned(4)[3] !== cStr[3]) fail("bulk_str_interned");
    if (op_id_bulk_smi(4)[3] !== cSmi[3]) fail("bulk_smi");
    const bb = op_id_bulk_buf(4)[3];
    if (!(bb instanceof Uint8Array) || bb.length !== 16 || bb.some((v, i) => v !== cBuf[3][i])) fail("bulk_buf");
    return "ok";
};

globalThis.inPair = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += op_id_in_pair(cPair[i % L]); } return a; };
globalThis.inPairFast = (n) => { let a = 0; for (let i = 0; i < n; i++) { const p = cPair[i % L]; a += op_id_in_pair_fast(p[0], p[1]); } return a; };
globalThis.inStr = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += op_id_in_str(cStr[i % L]); } return a; };
globalThis.inStrSerde = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += op_id_in_str_serde(cStr[i % L]); } return a; };
globalThis.inU128 = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += op_id_in_u128(cU128[i % L]); } return a; };
globalThis.inSmi = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += op_id_in_smi(cSmi[i % L]); } return a; };

globalThis.bulk = (fn, calls, ids) => { let a = 0; for (let i = 0; i < calls; i++) { a += fn(ids).length; } return a; };

// Script-side tax: the equality test and the Map lookup an author writes.
globalThis.mPair = new Map(cPair.map((p, i) => [`${p[0]}:${p[1]}`, i]));
globalThis.mU128 = new Map(cU128.map((v, i) => [v, i]));
globalThis.mStr = new Map(cStr.map((v, i) => [v, i]));
globalThis.mSmi = new Map(cSmi.map((v, i) => [v, i]));

globalThis.eqPair = (n) => { let a = 0; for (let i = 0; i < n; i++) { const x = cPair[i % L], y = cPair[(i + 1) % L]; if (x[0] === y[0] && x[1] === y[1]) a++; } return a; };
globalThis.eqU128 = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (cU128[i % L] === cU128[(i + 1) % L]) a++; } return a; };
globalThis.eqStr = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (cStr[i % L] === cStr[(i + 1) % L]) a++; } return a; };
globalThis.eqSmi = (n) => { let a = 0; for (let i = 0; i < n; i++) { if (cSmi[i % L] === cSmi[(i + 1) % L]) a++; } return a; };

globalThis.keyPair = (n) => { let a = 0; for (let i = 0; i < n; i++) { const p = cPair[i % L]; a += mPair.get(`${p[0]}:${p[1]}`); } return a; };
globalThis.keyU128 = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += mU128.get(cU128[i % L]); } return a; };
globalThis.keyStr = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += mStr.get(cStr[i % L]); } return a; };
globalThis.keySmi = (n) => { let a = 0; for (let i = 0; i < n; i++) { a += mSmi.get(cSmi[i % L]); } return a; };
"#;

fn main() {
    // V8 posts delayed tasks (GC pacing among them) and refuses to honor them
    // outside a tokio context, so the isolate must be built inside one.
    let tokio = tokio::runtime::Runtime::new().expect("tokio runtime starts");
    let _guard = tokio.enter();

    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![id_wire::init()],
        ..Default::default()
    });
    runtime
        .execute_script("<id_wire prelude>", FastString::from_static(PRELUDE))
        .expect("prelude evaluates");
    runtime
        .execute_script("<id_wire sanity>", FastString::from_static("sanity();"))
        .expect("every encoding round-trips its id");

    println!(
        "id_wire: {CORPUS} distinct ids; roundtrip {ROUNDTRIP_CALLS} calls/pass, \
         bulk {BULK_IDS} ids x {BULK_CALLS} calls/pass, js {JS_ITERS} iters/pass"
    );

    println!("\nroundtrip (one id in, one id out, per op call)");
    let rt = ROUNDTRIP_CALLS;
    for (label, call) in [
        ("nop_smi (op floor)", "rtNop"),
        ("pair (status quo)", "rtPair"),
        ("pair_fast (bigint args)", "rtPairFast"),
        ("pair_v8 (no serde)", "rtPairV8"),
        ("u128", "rtU128"),
        ("str", "rtStr"),
        ("str_stack (no heap)", "rtStrStack"),
        ("str_interned", "rtStrInt"),
        ("str_best (stack+memo)", "rtStrBest"),
        ("buf", "rtBuf"),
        ("smi", "rtSmi"),
    ] {
        cell(&mut runtime, label, &format!("{call}({rt})"), rt);
    }

    println!("\ninbound only (id in, scalar out -- the setter-op shape)");
    for (label, call) in [
        ("pair (status quo)", "inPair"),
        ("pair_fast (bigint args)", "inPairFast"),
        ("str (#[string], fast)", "inStr"),
        ("str (#[serde])", "inStrSerde"),
        ("u128", "inU128"),
        ("smi (table index)", "inSmi"),
    ] {
        cell(&mut runtime, label, &format!("{call}({rt})"), rt);
    }

    println!("\nbulk (ids returned in one array, cost per id)");
    let bulk_ops = BULK_IDS * BULK_CALLS;
    for (label, op) in [
        ("pair (status quo)", "op_id_bulk_pair"),
        ("pair_v8 (no serde)", "op_id_bulk_pair_v8"),
        ("u128", "op_id_bulk_u128"),
        ("str", "op_id_bulk_str"),
        ("str_interned", "op_id_bulk_str_interned"),
        ("buf", "op_id_bulk_buf"),
        ("smi", "op_id_bulk_smi"),
    ] {
        cell(
            &mut runtime,
            label,
            &format!("bulk({op}, {BULK_CALLS}, {BULK_IDS})"),
            bulk_ops,
        );
    }

    println!("\njs equality (script-side, no ops)");
    for (label, f) in [
        ("pair (elementwise)", "eqPair"),
        ("u128", "eqU128"),
        ("str", "eqStr"),
        ("smi", "eqSmi"),
    ] {
        cell(&mut runtime, label, &format!("{f}({JS_ITERS})"), JS_ITERS);
    }

    println!("\njs Map lookup (script-side, no ops)");
    for (label, f) in [
        ("pair (built key)", "keyPair"),
        ("u128", "keyU128"),
        ("str", "keyStr"),
        ("smi", "keySmi"),
    ] {
        cell(&mut runtime, label, &format!("{f}({JS_ITERS})"), JS_ITERS);
    }
}
