const { execFile } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const { promisify } = require("node:util");

const execFileAsync = promisify(execFile);
const projectRoot = path.resolve(__dirname, "..");
const manifestPath = require.resolve("turbo/package.json");
const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
const cli = path.resolve(path.dirname(manifestPath), manifest.bin.turbo);

execFileAsync(process.execPath, [cli, "--version"], { cwd: projectRoot })
  .then(({ stdout }) => console.log(stdout.trim()))
  .catch((error) => {
    console.error(`TURBO_NATIVE_UNSUPPORTED: ${error?.stderr || error?.message || error}`);
    process.exit(1);
  });
