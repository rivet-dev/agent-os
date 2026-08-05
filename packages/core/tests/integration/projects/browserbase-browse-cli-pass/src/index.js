import { execFile } from "node:child_process";
import path from "node:path";
import { promisify } from "node:util";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const packagePath = require.resolve("browse/package.json");
const packageJson = require(packagePath);
const executable = path.resolve(path.dirname(packagePath), packageJson.bin.browse);
const { stdout } = await promisify(execFile)(process.execPath, [executable, "--help"], {
  env: { ...process.env, NO_COLOR: "1" },
  timeout: 15_000,
});

console.log(JSON.stringify({
  version: packageJson.version,
  help: stdout.includes("Unified Browserbase CLI") && stdout.includes("USAGE"),
  cloud: stdout.includes("cloud"),
  skills: stdout.includes("skills"),
}));
