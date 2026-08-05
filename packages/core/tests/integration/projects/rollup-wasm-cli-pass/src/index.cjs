const { execFile } = require("node:child_process");
const { readFile } = require("node:fs/promises");
const path = require("node:path");
const { promisify } = require("node:util");

const execFileAsync = promisify(execFile);
const projectRoot = path.resolve(__dirname, "..");

async function main() {
  const cli = path.join(projectRoot, "node_modules", "rollup", "dist", "bin", "rollup");
  await execFileAsync(
    process.execPath,
    [cli, "src/input.js", "--format", "esm", "--file", "dist/bundle.js", "--silent"],
    { cwd: projectRoot },
  );
  const bundle = await readFile(path.join(projectRoot, "dist", "bundle.js"), "utf8");
  console.log(JSON.stringify({
    bundled: bundle.includes("answer:") && bundle.includes("42"),
    treeShaken: !bundle.includes("tree-shake-me"),
  }));
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
