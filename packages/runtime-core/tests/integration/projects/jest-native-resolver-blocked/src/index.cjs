const path = require("node:path");
const fs = require("node:fs");

try {
  require("unrs-resolver");
} catch (error) {
  console.error(`JEST_NATIVE_RESOLVER_UNSUPPORTED: ${error?.message ?? error}`);
  process.exit(1);
}

require("jest-circus/runner");
require("jest-environment-node");
const runner = require.resolve("jest-circus/runner");
if (!fs.existsSync(runner)) throw new Error(`projected Jest runner is not visible to fs.existsSync: ${runner}`);
if (require.resolve(runner) !== runner) throw new Error(`absolute Jest runner resolution changed: ${runner}`);
const cli = path.resolve(__dirname, "..", "node_modules", "jest", "bin", "jest.js");
process.argv = [process.execPath, cli, "--runInBand", "--silent", "--no-cache", "sum.test.cjs"];
require(cli);
