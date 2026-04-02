"use strict";

const { add, multiply } = require("@workspace-test/lib");

const results = [
	{ op: "add", a: 2, b: 3, result: add(2, 3) },
	{ op: "multiply", a: 4, b: 5, result: multiply(4, 5) },
	{ op: "add", a: 0, b: 0, result: add(0, 0) },
];

console.log(JSON.stringify(results));
