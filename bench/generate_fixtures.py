#!/usr/bin/env python3
"""Generate the public, deterministic benchmark corpora.

The vocabulary below is deliberately self-contained. No source corpus, player
log, game output, or network access is used. Running this script with the same
Python major version produces byte-for-byte identical UTF-8/LF files.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
from itertools import product
from pathlib import Path


BASE_ITEM_COUNT = 6_350
LARGE_ITEM_COUNT = 10_000
LOG_LINE_COUNT = 300_000
MIN_LOG_BYTES = 16_396_134
ANSI_LINE_RESET = "\x1b[0m\x1b[39m\x1b[49m"

CONDITIONS = (
    "ancient", "bright", "carved", "dented", "dusty", "etched", "faded",
    "glimmering", "hardened", "inlaid", "lacquered", "mended", "ornate",
    "polished", "reinforced", "scuffed", "tempered", "weathered", "woven",
    "well-kept",
)
MATERIALS = (
    "ashwood", "bronze", "canvas", "cedar", "copper", "crystal", "ebony",
    "glass", "granite", "iron", "leather", "linen", "maple", "oak", "pewter",
    "quartz", "silver", "slate", "steel", "willow",
)
OBJECTS = (
    "amulet", "backpack", "belt", "book", "boots", "bottle", "bracer",
    "brooch", "buckler", "candle", "cloak", "compass", "crown", "flask",
    "gloves", "hammer", "helm", "journal", "key", "lantern", "map", "mask",
    "medallion", "pouch", "ring", "rope", "satchel", "scarf", "shield", "staff",
)
SUFFIXES = (
    "bearing a comet sigil", "bound with blue thread", "etched with small stars",
    "for a patient traveler", "from the cedar watch", "from the eastern workshop",
    "from the quiet market", "in a fitted case", "marked with seven dots",
    "of clear mornings", "of patient craft", "of quiet embers", "of steady hands",
    "set with cloudy glass", "stamped for field use", "tied with green cord",
    "trimmed in pale cloth", "with a brass clasp", "with a fox emblem",
    "wrapped for a long road",
)

ROOM_ADJECTIVES = (
    "Amber", "Breezy", "Copper", "Distant", "Echoing", "Fern-lined", "Golden",
    "High", "Ivied", "Lamplit", "Mossy", "Quiet", "Rain-washed", "Silver",
)
ROOM_TYPES = (
    "Arcade", "Bridge", "Causeway", "Courtyard", "Garden", "Hall", "Landing",
    "Library", "Observatory", "Orchard", "Passage", "Square", "Terrace", "Workshop",
)
ACTORS = (
    "Aster", "Bramble", "Cinder", "Dapple", "Ember", "Flint", "Grove", "Harbor",
    "Juniper", "Lumen", "Marlow", "Nettle", "Peregrine", "Quill", "Rowan", "Tansy",
)
CREATURES = (
    "clockwork beetle", "copper fox", "garden sentinel", "granite crab",
    "lantern moth", "moss golem", "paper drake", "practice automaton",
    "reed serpent", "silver rook", "sparring dummy", "wooden guardian",
)


def article_for(word: str) -> str:
    return "an" if word[0].lower() in "aeiou" else "a"


def build_item_names() -> list[str]:
    """Return a stable, varied ordering of wholly synthetic item names."""
    names = [
        f"{article_for(condition)} {condition} {material} {obj} {suffix}"
        for condition, material, obj, suffix in product(
            CONDITIONS, MATERIALS, OBJECTS, SUFFIXES
        )
    ]
    # Digest ordering distributes every vocabulary axis through short prefixes
    # of the corpus while remaining deterministic across Python versions.
    names.sort(key=lambda name: hashlib.sha256(name.encode("utf-8")).digest())
    assert len(names) == len(set(names))
    return names[:LARGE_ITEM_COUNT]


def build_session_log(names: list[str]) -> bytes:
    """Build a varied text session large enough for the hot-path benchmarks."""
    lines: list[str] = []
    for i in range(LOG_LINE_COUNT):
        if i == 0:
            lines.append("# synthetic smudgy benchmark session; no private game data")
            continue

        turn = i // 20
        phase = i % 20
        actor = ACTORS[turn % len(ACTORS)]
        creature = CREATURES[(turn * 5 + 3) % len(CREATURES)]
        room_adj = ROOM_ADJECTIVES[(turn * 7 + 1) % len(ROOM_ADJECTIVES)]
        room_type = ROOM_TYPES[(turn * 11 + 2) % len(ROOM_TYPES)]
        room_id = (turn * 37) % 12_000
        hp = 400 - (turn % 97)
        mana = 260 - (turn % 61)
        moves = 180 - (turn % 43)
        damage = 7 + (turn * 13) % 89
        item = names[(turn * 97 + 19) % len(names)]

        if phase == 0:
            line = f"\x1b[1;36m{room_adj} {room_type} [{room_id:05d}]\x1b[0m"
        elif phase == 1:
            line = "A broad practice road winds between painted waystones and cedar rails."
        elif phase == 2:
            line = "Obvious exits: north, east, southwest, up."
        elif phase == 3:
            line = f"{actor} says 'The benchmark caravan leaves after the next bell.'"
        elif phase == 4:
            line = f"You hit {creature} for {damage} damage."
        elif phase == 5:
            line = f"{actor} parries your attack."
        elif phase == 6:
            line = f"You take {damage // 2 + 1} damage from {creature}."
        elif phase == 7:
            line = f"You gain {100 + turn % 900} experience points."
        elif phase == 8:
            line = f"You get {item} from a canvas supply crate."
        elif phase == 9:
            line = f"{hp}H {mana}M {moves}V >"
        elif phase == 10:
            line = f"{actor} leaves north."
        elif phase == 11:
            line = f"{actor} arrives from the south."
        elif phase == 12:
            line = "You begin casting a lantern practice spell."
        elif phase == 13:
            line = f"Your spark spell hits {creature} for {damage + 11}."
        elif phase == 14:
            line = f"{actor} tells you 'Room {room_id:05d} is mapped and ready.'"
        elif phase == 15:
            line = f"GMCP Room.Info {{\"num\":{room_id},\"name\":\"{room_adj} {room_type}\"}}"
        elif phase == 16:
            line = f"GMCP Char.Vitals {{\"hp\":{hp},\"mana\":{mana},\"moves\":{moves}}}"
        elif phase == 17:
            line = f"You put {item} in a reinforced canvas backpack."
        elif phase == 18:
            line = "\x1b[33mA brass bell rings once in the distance.\x1b[0m"
        else:
            line = f"Map update: room={room_id:05d} x={turn % 401 - 200} y={(turn * 3) % 401 - 200}."
        # Recorded terminal streams commonly carry redundant SGR resets. Keep
        # them at the end so start-anchored trigger patterns still exercise
        # successful matches while the terminal parser sees realistic traffic.
        lines.append(line + ANSI_LINE_RESET)

    payload = ("\n".join(lines) + "\n").encode("utf-8")
    assert len(lines) >= LOG_LINE_COUNT
    assert len(payload) >= MIN_LOG_BYTES, (
        f"synthetic log is only {len(payload):,} bytes; expected at least "
        f"{MIN_LOG_BYTES:,}"
    )
    return payload


def checked_write(path: Path, expected: bytes, check: bool) -> bool:
    if check:
        try:
            actual = path.read_bytes()
        except FileNotFoundError:
            print(f"missing: {path}", file=sys.stderr)
            return False
        if actual != expected:
            print(f"out of date: {path}", file=sys.stderr)
            return False
    else:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(expected)

    digest = hashlib.sha256(expected).hexdigest()[:16]
    print(f"{'verified' if check else 'wrote'} {path.name}: {len(expected):,} bytes sha256={digest}…")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="verify committed fixtures byte-for-byte"
    )
    args = parser.parse_args()
    root = Path(__file__).resolve().parent

    names = build_item_names()
    base_items = ("\n".join(names[:BASE_ITEM_COUNT]) + "\n").encode("utf-8")
    large_items = ("\n".join(names) + "\n").encode("utf-8")
    session_log = build_session_log(names)

    results = (
        checked_write(root / "item_names.txt", base_items, args.check),
        checked_write(root / "item_names_10k.txt", large_items, args.check),
        checked_write(root / "logs" / "synthetic-long-session.log", session_log, args.check),
    )
    return 0 if all(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
