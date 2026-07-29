import path from "node:path";
import { fileURLToPath } from "node:url";
import { startVitest } from "vitest/node";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const testRoot = path.join(projectRoot, "src");
const context = await startVitest("test", [], {
  config: false,
  root: testRoot,
  run: true,
  watch: false,
  include: ["**/*.test.js"],
  pool: "forks",
  maxWorkers: 1,
  fileParallelism: false,
  reporters: [{ onInit() {}, onFinished() {} }],
  silent: true,
});

const files = context.state.getFiles();
const tests = [];
function collect(tasks) {
  for (const task of tasks) {
    if (task.type === "test") tests.push(task);
    if (Array.isArray(task.tasks)) collect(task.tasks);
  }
}
for (const file of files) collect(file.tasks);

const result = {
  files: files.length,
  passedFiles: files.filter((file) => file.result?.state === "pass").length,
  tests: tests.length,
  passedTests: tests.filter((test) => test.result?.state === "pass").length,
};
const expected = { files: 2, passedFiles: 2, tests: 3, passedTests: 3 };
if (JSON.stringify(result) !== JSON.stringify(expected)) {
  const diagnostics = {
    result,
    files: files.map((file) => ({
      filepath: file.filepath,
      result: file.result,
      taskCount: file.tasks?.length,
    })),
    errors: context.state.getUnhandledErrors().map((error) => ({
      name: error?.name,
      message: error?.message,
      stack: error?.stack,
    })),
  };
  throw new Error(`unexpected Vitest result: ${JSON.stringify(diagnostics)}`);
}
console.log(JSON.stringify(result));
