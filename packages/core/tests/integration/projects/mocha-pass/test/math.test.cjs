const assert = require("node:assert/strict");

describe("math", function () {
  let value;

  beforeEach(function () {
    value = 40;
  });

  it("runs synchronous tests", function () {
    assert.equal(value + 2, 42);
  });

  it("runs asynchronous tests", async function () {
    await Promise.resolve();
    assert.deepEqual([value, 2], [40, 2]);
  });
});
