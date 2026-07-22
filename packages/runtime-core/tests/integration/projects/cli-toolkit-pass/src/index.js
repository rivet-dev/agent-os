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
import { StringDecoder } from "node:string_decoder";

const command = new Command()
  .exitOverride()
  .option("--count <number>")
  .parse(["node", "tool", "--count", "2"]);
const parsed = yargs(["--name", "agentos"])
  .exitProcess(false)
  .option("name", { type: "string" })
  .parse();
if (
  new StringDecoder("utf8").write(new Uint8Array([112, 114, 111, 98, 101])) !==
  "probe"
) {
  throw new Error("node:string_decoder did not decode a Uint8Array");
}
const child = execaSync(
  process.execPath,
  [
    path.join(path.dirname(fileURLToPath(import.meta.url)), "child.js"),
    "alpha",
    "beta",
  ],
  { maxBuffer: 1024 * 1024 },
);
let childArgv;
try {
  childArgv = JSON.parse(child.stdout);
} catch (error) {
  throw new Error(
    `execa child stdout was not JSON: ${JSON.stringify(child.stdout)} (${error.message})`,
  );
}
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
    execa: childArgv,
    oraStopped: !spinner.isSpinning,
    glob: globFiles,
    fastGlob: fastGlobFiles,
  }));
} finally {
  await rm(root, { recursive: true, force: true });
}
