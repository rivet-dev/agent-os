"use strict";

const levelB = require("@chain-test/level-b");

module.exports = {
	name: "level-a",
	depth: 1,
	child: levelB,
	greet(who) {
		return "level-a wraps: " + levelB.greet(who);
	}
};
