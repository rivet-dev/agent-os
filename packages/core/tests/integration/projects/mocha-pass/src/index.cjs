const { execFile } = require("node:child_process");
const path = require("node:path");
const { promisify } = require("node:util");

const execFileAsync = promisify(execFile);
const projectRoot = path.resolve(__dirname, "..");

async function main() {
  const cli = require.resolve("mocha/bin/mocha.js", { paths: [projectRoot] });
  const { stdout } = await execFileAsync(
    process.execPath,
    [cli, "--reporter=json", "test/**/*.test.cjs"],
    { cwd: projectRoot, env: { ...process.env, NO_COLOR: "1", FORCE_COLOR: "0" } },
  );
  const report = JSON.parse(stdout);
  console.log(JSON.stringify({
    suites: report.stats.suites,
    tests: report.stats.tests,
    passes: report.stats.passes,
    failures: report.stats.failures,
  }));
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
