const { test } = require("node:test");
const assert = require("node:assert/strict");
const { transform } = require("./subject.cjs");

test("transform", () => {
    for (const value of [-2, -1, 0, 1, 2, 3]) {
        if (value < 0) {
            assert.equal(transform(value), -value);
        } else {
            assert.equal(transform(value), value * 2);
        }
    }
});
