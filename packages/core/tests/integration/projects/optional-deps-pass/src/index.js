"use strict";

const semver = require("semver");

let optionalAvailable;
try {
	require("@anthropic-internal/nonexistent-optional-pkg");
	optionalAvailable = true;
} catch (e) {
	optionalAvailable = false;
}

const result = {
	semverValid: semver.valid("1.0.0") !== null,
	optionalAvailable: optionalAvailable
};

console.log(JSON.stringify(result));
