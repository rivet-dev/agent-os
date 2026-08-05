"use strict";

const main = require("@cond-test/lib");
const feature = require("@cond-test/lib/feature");

const result = {
	mainEntry: main.entry,
	mainVersion: main.version,
	featureName: feature.name,
	featureEnabled: feature.enabled
};

console.log(JSON.stringify(result));
