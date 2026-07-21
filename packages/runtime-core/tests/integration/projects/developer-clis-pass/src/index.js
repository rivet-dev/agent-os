import { execFile } from "node:child_process";
import { readFileSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const require = createRequire(import.meta.url);
const execFileAsync = promisify(execFile);
const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function resolveBin(packageName, binName) {
  const packagePath = path.join(projectRoot, "node_modules", ...packageName.split("/"), "package.json");
  const manifest = JSON.parse(readFileSync(packagePath, "utf8"));
  const relative = typeof manifest.bin === "string" ? manifest.bin : manifest.bin[binName];
  if (!relative) throw new Error(`${packageName} does not expose ${binName}`);
  return path.resolve(path.dirname(packagePath), relative);
}

async function runBin(packageName, binName, args, cwd, options = {}) {
  try {
    return await execFileAsync(process.execPath, [resolveBin(packageName, binName), ...args], {
      cwd,
      env: { ...process.env, CI: "1", NO_COLOR: "1", FORCE_COLOR: "0", ...options.env },
      timeout: options.timeout ?? 30_000,
      maxBuffer: 8 * 1024 * 1024,
    });
  } catch (error) {
    throw Object.assign(new Error(`${packageName}/${binName} failed: ${error.stderr || error.message}`), {
      code: error.code,
      stdout: error.stdout,
      stderr: error.stderr,
    });
  }
}

const root = await mkdtemp(path.join(os.tmpdir(), "agentos-developer-clis-"));
const results = {};

try {
  await writeFile(path.join(root, "argv.mjs"), "console.log(JSON.stringify(process.argv.slice(2)));\n");
  const argvProbe = await execFileAsync(process.execPath, [path.join(root, "argv.mjs"), "alpha", "beta"], { cwd: root });
  results.esmArgv = argvProbe.stdout.trim() === '["alpha","beta"]';
  await writeFile(path.join(root, "format.js"), "const value={answer:42}\n");
  await runBin("prettier", "prettier", ["--write", "format.js"], root);
  results.prettier = (await readFile(path.join(root, "format.js"), "utf8")).includes("{ answer: 42 }");

  await writeFile(path.join(root, "eslint.config.mjs"), `export default [{
    files: ["**/*.js"],
    ignores: ["dist/**", "coverage/**"],
    rules: { semi: ["error", "always"], quotes: ["error", "double"] },
  }];\n`);
  await writeFile(path.join(root, "lint.js"), "const message = 'ok'\nconsole.log(message)\n");
  await runBin("eslint", "eslint", ["--fix", "lint.js"], root);
  const lintOutput = await readFile(path.join(root, "lint.js"), "utf8");
  results.eslint = lintOutput.includes('"ok";') && lintOutput.includes("console.log(message);");

  await writeFile(path.join(root, "webpack-entry.js"), "const answer = 42; console.log(`bundle:${answer}`);\n");
  await writeFile(path.join(root, "webpack.config.cjs"), `const path = require("node:path");
module.exports = {
  mode: "production",
  entry: "./webpack-entry.js",
  output: { path: path.resolve(__dirname, "bundle"), filename: "app.js" },
  optimization: { minimize: false },
};\n`);
  await runBin("webpack", "webpack", ["--config", "webpack.config.cjs"], root);
  results.webpack = (await readFile(path.join(root, "bundle", "app.js"), "utf8")).includes("bundle:");

  await writeFile(path.join(root, "babel-input.js"), "const answer = () => 42;\n");
  const babelVisibility = await execFileAsync(
    process.execPath,
    ["-e", "console.log(require('node:fs').existsSync('babel-input.js'))"],
    { cwd: root },
  );
  if (babelVisibility.stdout.trim() !== "true") {
    throw new Error(`child process cannot see babel-input.js: ${babelVisibility.stdout.trim()}`);
  }
  await runBin("@babel/cli", "babel", ["babel-input.js", "--plugins", require.resolve("@babel/plugin-transform-arrow-functions"), "--out-file", "babel-output.js"], root);
  results.babel = (await readFile(path.join(root, "babel-output.js"), "utf8")).includes("function");

  await writeFile(path.join(root, "terser-input.js"), "function answer() { const unused = 1; return 40 + 2; } console.log(answer());\n");
  await runBin("terser", "terser", ["terser-input.js", "--compress", "--mangle", "--output", "terser-output.js"], root);
  results.terser = (await stat(path.join(root, "terser-output.js"))).size < (await stat(path.join(root, "terser-input.js"))).size;

  await mkdir(path.join(root, "remove-me"));
  await writeFile(path.join(root, "remove-me", "file.txt"), "remove\n");
  await runBin("rimraf", "rimraf", ["remove-me"], root);
  results.rimraf = await stat(path.join(root, "remove-me")).then(() => false, () => true);

  await writeFile(path.join(root, "print-env.cjs"), "console.log(process.env.AGENTOS_CLI_VALUE);\n");
  const crossEnv = await runBin("cross-env", "cross-env", ["AGENTOS_CLI_VALUE=42", process.execPath, "print-env.cjs"], root);
  results.crossEnv = crossEnv.stdout.trim() === "42";

  await writeFile(path.join(root, "data.json"), "{\"value\":41}\n");
  await runBin("json", "json", ["-I", "-f", "data.json", "-e", "this.value += 1"], root);
  const jsonOutput = await readFile(path.join(root, "data.json"), "utf8");
  results.json = JSON.parse(jsonOutput).value === 42;
  if (!results.json) throw new Error(`json CLI output mismatch: ${jsonOutput}`);

  const monorepo = path.join(root, "monorepo");
  await mkdir(path.join(monorepo, "packages", "a"), { recursive: true });
  await mkdir(path.join(monorepo, "packages", "b"), { recursive: true });
  await writeFile(path.join(monorepo, "package.json"), `${JSON.stringify({ name: "agentos-cli-monorepo", version: "1.0.0", private: true, packageManager: "pnpm@11.15.0", workspaces: ["packages/*"] }, null, 2)}\n`);
  await writeFile(path.join(monorepo, "packages", "a", "package.json"), "{\"name\":\"@fixture/a\",\"version\":\"1.0.0\"}\n");
  await writeFile(path.join(monorepo, "packages", "b", "package.json"), "{\"name\":\"@fixture/b\",\"version\":\"1.0.0\"}\n");
  await execFileAsync(process.execPath, [require.resolve("@manypkg/cli"), "check"], {
    cwd: monorepo,
    env: { ...process.env, CI: "1", NO_COLOR: "1", FORCE_COLOR: "0" },
  });
  results.manypkg = true;

  await mkdir(path.join(root, "graph"));
  await writeFile(path.join(root, "graph", "a.js"), "import './b.js';\n");
  await writeFile(path.join(root, "graph", "b.js"), "export const value = 42;\n");
  const madge = await runBin("madge", "madge", ["--json", "graph/a.js"], root);
  const graph = JSON.parse(madge.stdout);
  results.madge = Array.isArray(graph["a.js"]) && graph["a.js"].includes("b.js");

  if (Object.values(results).some(result => result !== true)) {
    throw new Error(`CLI checks failed: ${JSON.stringify(results)}`);
  }
  console.log(JSON.stringify(Object.keys(results).sort()));
} finally {
  await rm(root, { recursive: true, force: true });
}
