let test;
try {
  ({ test } = await import("node:test"));
} catch (error) {
  console.error(`NODE_TEST_RUNNER_UNSUPPORTED: ${error?.message ?? error}`);
  process.exit(1);
}

const assert = await import("node:assert/strict");
let testRan = false;
test("AgentOS node:test probe", async () => {
  await Promise.resolve();
  assert.equal(40 + 2, 42);
  testRan = true;
});

await new Promise((resolve) => setImmediate(resolve));
if (!testRan) {
  console.error(
    "NODE_TEST_RUNNER_UNSUPPORTED: imported node:test did not automatically execute the registered test",
  );
  process.exit(1);
}
