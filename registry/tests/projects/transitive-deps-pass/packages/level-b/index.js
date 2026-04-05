"use strict";

const levelC = require("@chain-test/level-c");

module.exports = {
	name: "level-b",
	depth: 2,
	child: levelC,
	greet(who) {
		return "level-b wraps: " + levelC.greet(who);
	}
};
