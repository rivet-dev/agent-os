"use strict";

const leftPad = require("left-pad");

const results = [
	{ input: "hello", width: 10, padded: leftPad("hello", 10) },
	{ input: "42", width: 5, fill: "0", padded: leftPad("42", 5, "0") },
	{ input: "", width: 3, padded: leftPad("", 3) },
];

console.log(JSON.stringify(results));
