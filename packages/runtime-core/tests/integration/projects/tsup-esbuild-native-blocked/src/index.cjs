const { execFile } = require("node:child_process");
const { readFile } = require("node:fs/promises");
const path = require("node:path");
const { promisify } = require("node:util");

const execFileAsync = promisify(execFile);
const projectRoot = path.resolve(__dirname, "..");

execFileAsync(
  process.execPath,
  [require.resolve("tsup/dist/cli-default.js"), "src/input.ts", "--format", "esm", "--out-dir", "dist", "--clean", "--silent"],
  { cwd: projectRoot },
).then(async () => {
  const output = await readFile(path.join(projectRoot, "dist", "input.mjs"), "utf8");
  console.log(output.includes("42"));
}).catch((error) => {
  console.error(`TSUP_ESBUILD_NATIVE_UNSUPPORTED: ${error?.stderr || error?.message || error}`);
  process.exit(1);
});
