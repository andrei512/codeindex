const { test } = require("node:test");
const assert = require("node:assert/strict");
const { transform } = require("./subject.cjs");

test("negative", () => {
    assert.equal(transform(-2), 2);
    assert.equal(transform(-1), 1);
});

test("nonnegative", () => {
    assert.equal(transform(0), 0);
    assert.equal(transform(1), 2);
    assert.equal(transform(2), 4);
    assert.equal(transform(3), 6);
});
