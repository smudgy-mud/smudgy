import assert from "node:assert/strict";
import test from "node:test";

import { idsMatch } from "./ids.ts";

const areaId = (value: string) => value as AreaId;

test("matches complete mapper UUID strings", () => {
    const first = areaId("67e55044-10b1-426f-9247-bb680e5fe0c8");
    const samePrefix = areaId("67ffffff-ffff-4fff-8fff-ffffffffffff");

    assert.equal(idsMatch(first, first), true);
    assert.equal(idsMatch(first, samePrefix), false);
});

test("matches absent destinations only when both are absent", () => {
    const id = areaId("67e55044-10b1-426f-9247-bb680e5fe0c8");

    assert.equal(idsMatch(null, undefined), true);
    assert.equal(idsMatch(id, null), false);
    assert.equal(idsMatch(undefined, id), false);
});
