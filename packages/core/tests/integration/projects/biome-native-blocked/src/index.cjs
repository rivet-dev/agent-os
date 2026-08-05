const { execFile } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const { promisify } = require("node:util");

const execFileAsync = promisify(execFile);
const projectRoot = path.resolve(__dirname, "..");
const manifestPath = require.resolve("@biomejs/biome/package.json");
const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
const cli = path.resolve(path.dirname(manifestPath), manifest.bin.biome);
const nativeCli = path.resolve(path.dirname(manifestPath), "..", "cli-linux-x64", "biome");

execFileAsync(nativeCli, ["--version"], { cwd: projectRoot })
  .then(({ stdout }) => console.log(stdout.trim()))
  .catch((error) => {
    console.error(`BIOME_NATIVE_UNSUPPORTED: ${error?.stderr || error?.message || error}`);
    process.exit(1);
  });
