"use strict";

const fs = require("fs");
const path = require("path");

const resolveFromProject = { paths: [__dirname] };
const nextDir = path.dirname(require.resolve("next/package.json", resolveFromProject));
const swcWasmDir = path.dirname(
  require.resolve("@next/swc-wasm-nodejs/package.json", resolveFromProject),
);
const fallbackDir = path.join(nextDir, "wasm", "@next", "swc-wasm-nodejs");
fs.mkdirSync(path.dirname(fallbackDir), { recursive: true });
fs.cpSync(swcWasmDir, fallbackDir, { recursive: true });
