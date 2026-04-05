const semver = require("semver");

const result = {
	valid: semver.valid("1.2.3"),
	satisfies: semver.satisfies("1.2.3", "^1.0.0"),
	compare: semver.compare("1.2.3", "1.2.4"),
};

console.log(JSON.stringify(result));
