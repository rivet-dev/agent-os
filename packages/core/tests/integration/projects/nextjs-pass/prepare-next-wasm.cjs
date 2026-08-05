"use strict";

var fs = require("fs");
var path = require("path");

var projectDir = __dirname;
var resolveFromProject = { paths: [projectDir] };
var nextDir = path.dirname(
	require.resolve("next/package.json", resolveFromProject),
);
var swcWasmDir = path.dirname(
	require.resolve("@next/swc-wasm-nodejs/package.json", resolveFromProject),
);
var fallbackDir = path.join(
	nextDir,
	"wasm",
	"@next",
	"swc-wasm-nodejs",
);

fs.mkdirSync(path.dirname(fallbackDir), { recursive: true });
fs.cpSync(swcWasmDir, fallbackDir, { recursive: true });
