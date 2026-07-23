//! Shared corpora + helpers for the `smudgy_bench` criterion benchmarks.
//!
//! `benches/trigger_matching.rs` compares matcher *strategies* in isolation;
//! `benches/trigger_engine.rs` drives smudgy's real trigger engine. Both load
//! the same corpora and share the [`REGEX_TRIGGERS`] set, so keeping that here
//! (rather than copied into each bench) stops the two from drifting.
//!
//! Env var honored by the loaders: `SMUDGY_BENCH_LINES=n` truncates a corpus.

// These are bench-support helpers; panicking on missing/garbled corpora is the
// desired behavior and not worth a `# Panics` section on each one.
#![allow(clippy::missing_panics_doc)]

pub mod alloc;
pub mod atlas;
pub mod session;
pub mod wire;

/// Tiny deterministic PRNG (splitmix64) shared by the corpus generators in
/// [`wire`] and [`atlas`], inlined so the bench-support layer adds no RNG
/// crate to the workspace. Statistical quality is irrelevant for corpus
/// shaping; only determinism (fixed seeds, identical output every call) and
/// cheap mixing matter.
pub(crate) struct SplitMix64(u64);

impl SplitMix64 {
    pub(crate) const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Draw in `0..n`. Modulo bias is irrelevant at corpus-shaping scale.
    pub(crate) fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "below(0) is meaningless");
        let n64 = u64::try_from(n).expect("usize fits in u64 on supported targets");
        usize::try_from(self.next_u64() % n64).expect("bounded by n, which is a usize")
    }
}

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// Absolute path to a file under the bench crate, where the corpora live.
#[must_use]
pub fn data_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Reads `path` into lines, honoring `SMUDGY_BENCH_LINES=n` (truncates the
/// corpus for faster runs).
#[must_use]
pub fn read_lines(path: &Path) -> Vec<String> {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if let Ok(n) = std::env::var("SMUDGY_BENCH_LINES") {
        lines.truncate(n.parse().expect("SMUDGY_BENCH_LINES must be a number"));
    }
    lines
}

/// The default deterministic synthetic session corpus.
#[must_use]
pub fn load_log_lines() -> Vec<String> {
    read_lines(&data_path("logs/synthetic-long-session.log"))
}

/// Every file in `logs/`, as `(file_name, lines)`, sorted by name. Each entry
/// is benchmarked separately by `trigger_engine`.
#[must_use]
pub fn log_corpora() -> Vec<(String, Vec<String>)> {
    let mut paths: Vec<PathBuf> = fs::read_dir(data_path("logs"))
        .expect("bench/logs directory")
        .map(|entry| entry.expect("logs dir entry").path())
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p
                .file_name()
                .expect("log file has a name")
                .to_string_lossy()
                .into_owned();
            (name, read_lines(&p))
        })
        .collect()
}

/// 6,350 deterministic synthetic item names, deduplicated, as unanchored
/// literal substitution patterns (the "thousands of substitutions" workload).
#[must_use]
pub fn load_item_names() -> Vec<String> {
    load_unique_names("item_names.txt")
}

/// 10,000 deterministic synthetic item substitutions generated from the same
/// neutral vocabulary as [`load_item_names`], deduplicated. Exercises the
/// literal tier at a larger, release-representative scale; used by
/// `trigger_engine`.
#[must_use]
pub fn load_item_names_10k() -> Vec<String> {
    load_unique_names("item_names_10k.txt")
}

/// Reads a names file under the bench crate, trimming, dropping blanks, and
/// deduplicating while preserving first-seen order.
fn load_unique_names(rel: &str) -> Vec<String> {
    let text =
        fs::read_to_string(data_path(rel)).unwrap_or_else(|e| panic!("read bench/{rel}: {e}"));
    let mut seen = HashSet::new();
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| seen.insert(l.to_owned()))
        .map(str::to_owned)
        .collect()
}

/// Representative non-literal triggers a real profile might carry alongside
/// thousands of literal substitutions. Dozens of patterns spanning the
/// categories a MUD player actually automates — communication, combat (both
/// directions), death/loot, and spells/affects, with further categories held
/// in reserve as comments — to exercise the regex tier (`regex-filtered` /
/// `RegexSet`) at a realistic scale rather than the handful a smoke test
/// would use. Benches report `REGEX_TRIGGERS.len()` at startup rather than
/// assuming a fixed count. Each carries a literal
/// anchor word so it prefilters, and none is a pure literal (every one has an
/// anchor, group, class, or quantifier), so they all route to the regex tier.
pub const REGEX_TRIGGERS: &[&str] = &[
    // -- Communication --
    r"^(\w+) tells you '(.+)'",
    r"^(\w+) says '(.+)'",
    r"^(\w+) gossips '(.+)'",
    r"^(\w+) yells '(.+)'",
    r"^(\w+) shouts '(.+)'",
    r"^(\w+) whispers to you '(.+)'",
    r"^(\w+) auctions '(.+)'",
    r"^(\w+) chats '(.+)'",
    r"^(\w+) asks '(.+)'",
    r"^(\w+) replies '(.+)'",
    r"^You tell (\w+) '(.+)'",
    // r"^\[Newbie\] (\w+): (.+)",
    // -- Combat: you -> target --
    r"^Your (?:hit|slash|pierce|pound|crush) (?:scratches|injures|wounds|maims|decimates) (.+?)\.",
    r"^You (?:hit|slash|pierce|pound|crush) (.+?) (?:hard|very hard|extremely hard)\.",
    r"^You miss (.+?) with your (\w+)\.",
    r"^You barely (?:hit|scratch) (.+?)\.",
    r"^You massacre (.+?) to (?:bloody fragments|tiny pieces)\.",
    r"^You hit (.+?) for (\d+) damage\.",
    r"^You critically hit (.+?) for (\d+)!",
    r"^You backstab (.+?) for (\d+) damage\.",
    r"^Your killing blow (?:slays|destroys) (.+?)\.",
    r"^You parry (\w+)'s attack\.",
    r"^You dodge (\w+)'s attack\.",
    r"^You disarm (\w+)!",
    // -- Combat: target -> you --
    r"^(\w+) (?:hits|slashes|pierces|pounds) (you|\w+)",
    r"^(\w+)'s (\w+) (?:hits|grazes|misses) you\.",
    r"^(\w+) misses you\.",
    r"^(\w+) tries to (\w+) you, but you (?:parry|dodge|block)\.",
    r"^You are wounded for (\d+) damage by (\w+)\.",
    r"^(\w+) parries your attack\.",
    r"^(\w+) dodges your attack\.",
    r"^You take (\d+) damage from (\w+)\.",
    r"^(\w+) (?:flees|panics) and (?:runs|escapes)",
    r"^(\w+) is (?:mortally wounded|bleeding|badly hurt)\.",
    r"^You feel yourself being (?:drained|paralyzed) by (\w+)\.",
    r"^(\w+) (?:bashes|kicks) you and you are stunned!",
    // -- Death, experience, loot --
    r"^You receive (\d+) experience",
    r"^(\w+) is dead! R\.I\.P\.",
    r"^You have (?:slain|killed) (.+?)\.",
    r"^You gain (\d+) experience points?\.",
    r"^You receive (\d+) (?:gold|silver|copper) coins?\.",
    r"^You get (\d+) coins from the corpse of (.+?)\.",
    r"^(\w+) has been killed by (\w+)\.",
    r"^The corpse of (.+?) (?:decays|crumbles into dust)\.",
    r"^Congratulations! You are now level (\d+)!",
    r"^You split (\d+) coins? with your group\.",
    // -- Spells and affects --
    r"^You begin (?:casting|chanting) (.+?)\.",
    r"^You (?:utter|speak) the words, '(\w+)'",
    r"^Your (\w+) spell (?:hits|strikes) (.+?) for (\d+)\.",
    r"^(\w+) completes (?:his|her|its) spell\.",
    r"^You feel (?:better|worse)",
    // r"^You feel (?:more|less) (\w+)\.",
    // r"^Your (\w+) (?:wears off|fades away)\.",
    // r"^You are surrounded by an? (\w+) (?:shield|aura)\.",
    // r"^(\w+) is now (?:blinded|poisoned|cursed|sleeping)\.",
    // r"^You resist the effects of (.+?)\.",
    // r"^A (\w+) appears in a puff of smoke\.",
    // r"^You fail to (?:cast|concentrate) and lose your spell\.",
    // r"^You feel a (?:warm glow|surge of energy) flow through you\.",
    // // -- Movement --
    // r"^(\w+) flies in from the (north|south|east|west|above|below)\.",
    // r"^(\w+) (?:arrives|comes) (?:in )?from the (north|south|east|west|up|down)\.",
    // r"^(\w+) leaves (north|south|east|west|up|down)\.",
    // r"^(\w+) (?:walks|runs|sneaks) in from the (\w+)\.",
    // r"^You follow (\w+)\.",
    // r"^(\w+) starts following you\.",
    // r"^You can't go that way\.",
    // r"^The (\w+) (?:gate|door) is closed\.",
    // r"^You (?:open|close) the (\w+)\.",
    // r"^You are (?:too exhausted|too tired) to move\.",
    // // -- Prompt / status line --
    // r"(\d+)H (\d+)V .* Exits:(\w+)>",
    // r"(\d+)/(\d+) hp (\d+)/(\d+) mana (\d+)/(\d+) mv",
    // r"^< (\d+)hp (\d+)m (\d+)mv >",
    // r"Exits: \[([\w ]+)\]",
    // r"^You are (?:hungry|thirsty|starving)\.",
    // r"^\*(\d+)Hp (\d+)Mn (\d+)Mv (\d+)Tnl",
    // // -- Inventory / items --
    // r"^(\w+) gives you (.+)\.",
    // r"^You get (.+) from (.+)\.",
    // r"^You drop (.+?)\.",
    // r"^You give (.+?) to (\w+)\.",
    // r"^You (?:wear|wield|hold) (.+?)(?: on| in)? your (\w+)\.",
    // r"^You stop using (.+?)\.",
    // r"^You (?:eat|drink) (?:a|an|some) (.+?)\.",
    // r"^You can't carry that many items\.",
    // r"^You are now wielding (.+?)\.",
    // r"^You put (.+?) in (?:the )?(.+?)\.",
    // r"^(.+?) is now (?:glowing|humming) (?:softly|brightly)\.",
    // r"^Your (.+?) (?:glows|pulses|vibrates)\.",
    // // -- Shop, quest, group --
    // r"^You buy (.+?) for (\d+) (?:gold|silver) coins?\.",
    // r"^You sell (.+?) for (\d+) coins\.",
    // r"^The shopkeeper tells you, '(.+)'",
    // r"^You receive your quest reward: (.+?)\.",
    // r"^Quest complete! You earned (\d+) (?:quest points|qp)\.",
    // r"^(\w+) has joined (?:your group|the group)\.",
    // r"^(\w+) has left your group\.",
    // r"^You are now the leader of the group\.",
    // r"^(\w+) practices (.+?) and improves\.",
    // r"^You learn (?:more about|a new) (.+?)\.",
    // r"^You are getting (?:better|more skilled) at (.+?)\.",
    // r"^(\w+) reports: (\d+)/(\d+)hp (\d+)/(\d+)mana\.",
    // // -- Misc status --
    // r"\((?:flying|invisible)\)",
];
