let test;
try {
  ({ test } = await import("node:test"));
} catch (error) {
  console.error(`NODE_TEST_RUNNER_UNSUPPORTED: ${error?.message ?? error}`);
  process.exit(1);
}

const assert = await import("node:assert/strict");
test("AgentOS node:test probe", async () => {
  await Promise.resolve();
  assert.equal(40 + 2, 42);
});
