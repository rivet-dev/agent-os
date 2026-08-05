"use strict";

const levelA = require("@chain-test/level-a");

const chain = [];
let current = levelA;
while (current) {
	chain.push({ name: current.name, depth: current.depth });
	current = current.child;
}

const result = {
	chain: chain,
	greeting: levelA.greet("world"),
	levels: chain.length
};

console.log(JSON.stringify(result));
