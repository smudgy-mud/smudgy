import assert from "node:assert/strict";
import test from "node:test";
import {
  compareLayoutQuality,
  DIRECTIONAL_VIOLATION_WEIGHT,
  LEVEL_VIOLATION_WEIGHT,
  type LayoutQuality,
} from "./layout.ts";

const FIELDS = [
  "levelViolations",
  "cardinalRayViolations",
  "reciprocalRayViolations",
  "routingViolations",
  "exitPortViolations",
  "reciprocalExitPortViolations",
  "roomObstructions",
  "linkCrossings",
  "levelSlack",
  "footprintArea",
  "footprintPerimeter",
  "cardinalSlack",
] as const satisfies readonly (keyof LayoutQuality)[];

/** Directional violations, routing violations and crossings; every other field the same. */
function quality(
  directional: number,
  routing: number,
  crossings: number,
  rest: Partial<LayoutQuality> = {},
): LayoutQuality {
  return {
    cardinalRayViolations: directional,
    reciprocalRayViolations: 0,
    routingViolations: routing,
    exitPortViolations: 0,
    reciprocalExitPortViolations: 0,
    roomObstructions: routing,
    linkCrossings: crossings,
    footprintArea: 100,
    footprintPerimeter: 40,
    cardinalSlack: 0,
    ...rest,
  };
}

const better = (a: LayoutQuality, b: LayoutQuality): boolean => compareLayoutQuality(a, b) > 0;

test("one directional violation is worth eight routing violations and crossings", () => {
  assert.equal(DIRECTIONAL_VIOLATION_WEIGHT, 8);
});

test("a directional violation removed for many more routing violations and crossings is worse", () => {
  // One fewer directional violation for 18 more routing violations and 45
  // more crossings: 8 x 28 + 242 + 119 = 585 against 8 x 27 + 260 + 164 = 640.
  const kept = quality(28, 242, 119);
  const traded = quality(27, 260, 164);
  assert.ok(better(kept, traded));
  assert.ok(!better(traded, kept));
  // Within the weight, removing the directional violation still wins.
  assert.ok(better(quality(27, 245, 120), kept));
});

test("the order is symmetric: one more directional violation that saves more than eight is better", () => {
  assert.ok(better(quality(6, 10, 10), quality(5, 15, 14)));
  assert.ok(!better(quality(5, 15, 14), quality(6, 10, 10)));
  // Routing violations and crossings weigh the same in the score.
  assert.ok(better(quality(6, 0, 20), quality(5, 20, 9)));
  assert.ok(better(quality(6, 20, 0), quality(5, 9, 20)));
});

test("exactly eight more ties on the score and the fewer directional violations win; nine more lose", () => {
  const before = quality(5, 0, 0);
  const eightMore = quality(4, 5, 3);
  const nineMore = quality(4, 5, 4);
  assert.ok(better(eightMore, before));
  assert.ok(!better(before, eightMore));
  assert.ok(better(before, nineMore));
  assert.ok(!better(nineMore, before));
  // From the other side: one more directional violation that saves exactly
  // eight loses the tie; one that saves nine wins.
  assert.ok(!better(quality(6, 0, 0), quality(5, 4, 4)));
  assert.ok(better(quality(6, 0, 0), quality(5, 4, 5)));
});

test("equal scores fall back to the field order", () => {
  // Same score and directional violations: fewer routing violations first.
  assert.ok(better(quality(5, 0, 10), quality(5, 10, 0)));
  // Then reciprocal rays before routing.
  assert.ok(better(
    quality(5, 4, 0, { reciprocalRayViolations: 1 }),
    quality(5, 4, 0, { reciprocalRayViolations: 2 }),
  ));
  // Footprint and slack only on an equal score and equal defects.
  assert.ok(better(quality(5, 4, 0, { footprintArea: 99 }), quality(5, 4, 0)));
  assert.ok(better(quality(5, 4, 0, { cardinalSlack: 1 }), quality(5, 4, 0, { cardinalSlack: 2 })));
  // Compactness never pays for a defect.
  assert.ok(better(quality(5, 4, 0), quality(5, 4, 1, { footprintArea: 1, cardinalSlack: 0 })));
  assert.equal(compareLayoutQuality(quality(5, 4, 3), quality(5, 4, 3)), 0);
});

test("a mis-levelled exit weighs seventeen: more than sixteen crossings or two exits drawn wrong", () => {
  assert.equal(LEVEL_VIOLATION_WEIGHT, 17);
  const misLevelled = quality(1, 0, 0, { levelViolations: 1 });
  assert.ok(better(quality(0, 0, 16), misLevelled));
  assert.ok(better(misLevelled, quality(0, 0, 18)));
  // Seventeen crossings tie on the score, and the fewer mis-levelled exits win.
  assert.ok(better(quality(0, 0, 17), misLevelled));
  assert.ok(better(quality(2, 0, 0), misLevelled));
  assert.ok(better(misLevelled, quality(3, 0, 0)));
  // Routing violations weigh the same as crossings against it.
  assert.ok(better(quality(0, 10, 6), misLevelled));
});

test("equal scores rank fewer mis-levelled exits first, before directional violations", () => {
  // 8 x 2 + 17 = 33 against 8 x 4 + 1 = 33.
  const oneMisLevelled = quality(3, 0, 0, { levelViolations: 1 });
  const fourOnTheirLevel = quality(4, 1, 0);
  assert.ok(better(fourOnTheirLevel, oneMisLevelled));
  assert.ok(!better(oneMisLevelled, fourOnTheirLevel));
});

test("after every defect, a map spanning fewer extra levels ranks first, then a smaller one", () => {
  const stretched = quality(0, 0, 0, { levelSlack: 2, footprintArea: 50 });
  const level = quality(0, 0, 0, { levelSlack: 0, footprintArea: 200 });
  assert.ok(better(level, stretched));
  // Size never pays for a defect: one crossing outweighs any stretch.
  assert.ok(better(quality(0, 0, 0, { levelSlack: 9 }), quality(0, 0, 1)));
  assert.ok(better(quality(0, 0, 0, { levelSlack: 9 }), quality(1, 0, 0)));
});

test("a missing optional field reads as zero", () => {
  const withoutReciprocal: LayoutQuality = {
    cardinalRayViolations: 1,
    routingViolations: 0,
    exitPortViolations: 0,
    roomObstructions: 0,
    linkCrossings: 0,
    footprintArea: 1,
    footprintPerimeter: 4,
    cardinalSlack: 0,
  };
  assert.equal(compareLayoutQuality(withoutReciprocal, { ...withoutReciprocal, reciprocalRayViolations: 0 }), 0);
  assert.ok(better(withoutReciprocal, { ...withoutReciprocal, reciprocalRayViolations: 1 }));
  assert.equal(compareLayoutQuality(withoutReciprocal, { ...withoutReciprocal, levelViolations: 0 }), 0);
  assert.ok(better(withoutReciprocal, { ...withoutReciprocal, levelViolations: 1 }));
  assert.equal(compareLayoutQuality(withoutReciprocal, { ...withoutReciprocal, levelSlack: 0 }), 0);
  assert.ok(better(withoutReciprocal, { ...withoutReciprocal, levelSlack: 1 }));
});

/** A deterministic 32-bit generator (mulberry32). */
function random(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

test("the order is a consistent total order, so every sort agrees with it", () => {
  const next = random(0x5eed);
  const small = (limit: number): number => Math.floor(next() * limit);
  // Narrow ranges make equal scores, and equal tuples, common.
  const values: LayoutQuality[] = Array.from({ length: 160 }, () => {
    const obstructions = small(4);
    const ports = small(3);
    const directional = small(4);
    return {
      cardinalRayViolations: directional,
      // Mis-levelled exits are among the directional violations.
      levelViolations: small(directional + 1),
      reciprocalRayViolations: small(2),
      routingViolations: obstructions + ports,
      exitPortViolations: ports,
      reciprocalExitPortViolations: small(2),
      roomObstructions: obstructions,
      linkCrossings: small(12),
      levelSlack: small(2),
      footprintArea: 20 + small(3),
      footprintPerimeter: 18 + small(2),
      cardinalSlack: small(2),
    };
  });
  const sign = (value: number): number => Math.sign(value) + 0;
  const same = (a: LayoutQuality, b: LayoutQuality): boolean =>
    FIELDS.every((field) => (a[field] ?? 0) === (b[field] ?? 0));
  for (const a of values) {
    assert.equal(compareLayoutQuality(a, a), 0);
    for (const b of values) {
      const ab = compareLayoutQuality(a, b);
      assert.equal(sign(ab) + sign(compareLayoutQuality(b, a)), 0, "antisymmetric");
      assert.equal(ab === 0, same(a, b), "only identical tuples tie");
    }
  }
  for (const a of values) {
    for (const b of values) {
      if (compareLayoutQuality(a, b) < 0) continue;
      for (const c of values) {
        if (compareLayoutQuality(b, c) < 0) continue;
        assert.ok(compareLayoutQuality(a, c) >= 0, "transitive");
        if (compareLayoutQuality(a, b) > 0 || compareLayoutQuality(b, c) > 0) {
          assert.ok(compareLayoutQuality(a, c) > 0, "strictly transitive");
        }
      }
    }
  }
  // Sorting any arrangement of the same tuples gives one sequence, best first.
  const key = (value: LayoutQuality): string => FIELDS.map((field) => value[field] ?? 0).join(",");
  const sorted = [...values].sort((a, b) => compareLayoutQuality(b, a)).map(key);
  for (let round = 0; round < 8; round += 1) {
    const shuffled = [...values];
    for (let index = shuffled.length - 1; index > 0; index -= 1) {
      const other = small(index + 1);
      [shuffled[index], shuffled[other]] = [shuffled[other], shuffled[index]];
    }
    assert.deepEqual(shuffled.sort((a, b) => compareLayoutQuality(b, a)).map(key), sorted);
  }
});
