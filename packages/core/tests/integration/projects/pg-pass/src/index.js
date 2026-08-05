"use strict";

const { Pool, Client, types } = require("pg");

const result = {
	poolExists: typeof Pool === "function",
	clientExists: typeof Client === "function",
	typesExists: typeof types === "object" && types !== null,
	poolMethods: [
		"connect",
		"end",
		"query",
		"on",
	].filter((m) => typeof Pool.prototype[m] === "function"),
	clientMethods: [
		"connect",
		"end",
		"query",
		"on",
	].filter((m) => typeof Client.prototype[m] === "function"),
};

// Verify type parsers exist
result.hasSetTypeParser = typeof types.setTypeParser === "function";
result.hasGetTypeParser = typeof types.getTypeParser === "function";

// Verify query builder can produce query config objects
const { Query } = require("pg");
result.queryExists = typeof Query === "function";

// Verify pg-pool defaults class exists and can be configured
const defaults = require("pg/lib/defaults");
result.defaultsExists = typeof defaults === "object" && defaults !== null;
result.defaultPort = defaults.port;
result.defaultHost = defaults.host;

console.log(JSON.stringify(result));
