import { parse, stringify, parseDocument } from "yaml";

// Parse a YAML string
const yamlStr = `
name: agent-os
version: 1.0.0
features:
  - sandboxing
  - isolation
  - compatibility
config:
  timeout: 30
  retries: 3
  nested:
    enabled: true
    level: 2
`;

const parsed = parse(yamlStr);

// Stringify a JS object back to YAML
const obj = {
	database: {
		host: "localhost",
		port: 5432,
		credentials: {
			user: "admin",
			pass: "secret",
		},
	},
	tags: ["prod", "us-east"],
};

const stringified = stringify(obj);

// Re-parse the stringified output to verify round-trip
const roundTrip = parse(stringified);

// Parse a document for node-level access
const doc = parseDocument("key: value\nlist:\n  - a\n  - b");
const docJSON = doc.toJSON();

const result = {
	parsed,
	stringified,
	roundTrip,
	roundTripMatch: JSON.stringify(obj) === JSON.stringify(roundTrip),
	docJSON,
};

console.log(JSON.stringify(result));
