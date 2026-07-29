"use strict";

const fs = require("fs");
const path = require("path");
const { execFile } = require("child_process");

const projectDir = path.resolve(__dirname, "..");

async function main() {
  process.env.NEXT_TELEMETRY_DISABLED = "1";
  const nextBin = require.resolve("next/dist/bin/next");
  const result = await new Promise(resolve => {
    execFile(
      process.execPath,
      [nextBin, "build", "--turbopack"],
      {
        cwd: projectDir,
        env: { ...process.env, NEXT_TELEMETRY_DISABLED: "1" },
        maxBuffer: 4 * 1024 * 1024,
      },
      (error, stdout, stderr) => resolve({ error, stdout, stderr }),
    );
  });
  if (result.error) {
    const detail = String(result.stderr || result.error.message).trim();
    console.error(`TURBOPACK_NATIVE_UNSUPPORTED: ${detail}`);
    process.exitCode = 1;
    return;
  }

  const buildManifest = path.join(projectDir, ".next", "build-manifest.json");
  console.log(JSON.stringify({
    turbopackBuild: fs.existsSync(buildManifest),
    nextVersion: require("next/package.json").version,
  }));
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
