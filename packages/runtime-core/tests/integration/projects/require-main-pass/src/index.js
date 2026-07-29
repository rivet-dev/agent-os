"use strict";

var assert = require("assert");

assert.strictEqual(require.main, module);
assert.strictEqual(module.id, ".");
assert.strictEqual(module.parent, null);
assert.strictEqual(process.mainModule, module);

console.log("require-main-ok");
