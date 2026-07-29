import { Command } from "commander";
import { execaSync } from "execa";
import fastGlob from "fast-glob";
import { glob } from "glob";
import ora from "ora";
import yargs from "yargs/yargs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const command = new Command()
  .exitOverride()
  .option("--count <number>")
  .parse(["node", "tool", "--count", "2"]);
const parsed = yargs(["--name", "agentos"])
  .exitProcess(false)
  .option("name", { type: "string" })
  .parse();
const child = execaSync(
  process.execPath,
  [
    path.join(path.dirname(fileURLToPath(import.meta.url)), "child.js"),
    "alpha",
    "beta",
  ],
  { maxBuffer: 1024 * 1024 },
);
const spinner = ora({ isEnabled: false, isSilent: true }).start();
spinner.succeed();

const root = await mkdtemp(path.join(os.tmpdir(), "agentos-cli-toolkit-"));
try {
  await writeFile(path.join(root, "alpha.txt"), "a\n");
  await writeFile(path.join(root, "beta.txt"), "b\n");
  await writeFile(path.join(root, "ignored.js"), "export {};\n");
  const globFiles = (await glob("*.txt", { cwd: root })).sort();
  const fastGlobFiles = (await fastGlob("*.txt", { cwd: root })).sort();
  console.log(JSON.stringify({
    commander: command.opts().count,
    yargs: parsed.name,
    execa: JSON.parse(child.stdout),
    oraStopped: !spinner.isSpinning,
    glob: globFiles,
    fastGlob: fastGlobFiles,
  }));
} finally {
  await rm(root, { recursive: true, force: true });
}
