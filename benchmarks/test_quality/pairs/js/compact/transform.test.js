const { test } = require("node:test");
const assert = require("node:assert/strict");
const { transform } = require("./subject.cjs");

test("transform", () => {
    const cases = [
            [-2, 2], [-1, 1], [0, 0], [1, 2], [2, 4], [3, 6]
        ];
    for (const [value, expected] of cases) {
        assert.equal(transform(value), expected);
    }
});
