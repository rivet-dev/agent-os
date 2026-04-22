---
name: sanity-check
description: Run an E2E smoke test that installs agent-os packages from npm in an isolated temp project, spawns a Pi agent session, has it write a file and read it back with cat, then verifies the result. Use when the user asks to sanity check, smoke test, or verify the release works.
---

# Sanity Check

## Usage
- `/sanity-check` — run in a temp directory on the host
- `/sanity-check docker` — run inside a `node:22` Docker container
- `/sanity-check --with-dbt` — additionally exercise the `python.dbt` opt-in (boots a VM with the vendored Pyodide wheels, runs a trivial dbt project end-to-end)
- `/sanity-check <custom instructions>` — any extra instructions (e.g. "use rc.3", "use pnpm", "test on node 20")

## What it tests

1. `npm install` of `@rivet-dev/agent-os-core`, `@rivet-dev/agent-os-pi`, `@rivet-dev/agent-os-common` from the public npm registry
2. Boot a VM with WASM coreutils (bash, cat, sh, etc.) and the Pi SDK ACP adapter
3. Create a Pi agent session with a real Anthropic API key
4. Send a prompt that uses the **write tool** to create `/tmp/test.txt` with "Hello from Agent OS!" and the **bash tool** to run `cat /tmp/test.txt`
5. Verify the file contents from the host side via `vm.readFile()`

## Requirements

- `ANTHROPIC_API_KEY` must be set in the environment. If not set, load it from `~/misc/env.txt`.
- Node.js 22+ (or Docker with `node:22` image)

## Steps

### 1. Set up the test project

Create a temp directory (e.g. `/tmp/agent-os-sanity-XXXX`) with two files:

**package.json:**
```json
{
  "name": "agent-os-sanity-check",
  "private": true,
  "type": "module",
  "dependencies": {
    "@rivet-dev/agent-os-core": "*",
    "@rivet-dev/agent-os-pi": "*",
    "@rivet-dev/agent-os-common": "*",
    "@mariozechner/pi-coding-agent": "^0.60.0",
    "@agentclientprotocol/sdk": "^0.16.1"
  }
}
```

If the user specifies a version (e.g. "use rc.3"), pin `@rivet-dev/agent-os-core` and `@rivet-dev/agent-os-pi` to that version.

**test.mjs:**
```js
import { AgentOs } from "@rivet-dev/agent-os-core";
import common from "@rivet-dev/agent-os-common";
import pi from "@rivet-dev/agent-os-pi";

const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;
if (!ANTHROPIC_API_KEY) {
  console.error("ANTHROPIC_API_KEY is required");
  process.exit(1);
}

console.log("Creating VM with common + pi...");
const vm = await AgentOs.create({ software: [common, pi] });

console.log("Creating PI agent session...");
const { sessionId } = await vm.createSession("pi", {
  env: { ANTHROPIC_API_KEY },
});
console.log(`Session created: ${sessionId}`);

vm.onSessionEvent(sessionId, (event) => {
  const params = event.params;
  if (params?.update?.sessionUpdate === "agent_message_chunk") {
    process.stdout.write(params.update.content?.text ?? "");
  }
});

console.log("\nSending prompt...");
const response = await vm.prompt(
  sessionId,
  'Write the text "Hello from Agent OS!" to /tmp/test.txt using the write tool. Then use the bash tool to run `cat /tmp/test.txt` and tell me what it says.',
);
console.log(`\n\nPrompt completed: ${response.stopReason}`);

console.log("\nVerifying file...");
try {
  const data = await vm.readFile("/tmp/test.txt");
  const text = new TextDecoder().decode(data);
  console.log(`File contents: "${text.trim()}"`);
  if (text.includes("Hello from Agent OS!")) {
    console.log("\n✅ E2E TEST PASSED");
  } else {
    console.log("\n❌ E2E TEST FAILED: wrong content");
    process.exit(1);
  }
} catch (err) {
  console.log(`\n❌ E2E TEST FAILED: ${err.message}`);
  process.exit(1);
}

vm.closeSession(sessionId);
await vm.dispose();
```

### 2. Run the test

**Default (temp dir on host):**
```bash
cd /tmp/agent-os-sanity-XXXX
npm install
node test.mjs
```

**Docker mode:**
```bash
docker run --rm \
  -e ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
  -v /tmp/agent-os-sanity-XXXX:/app \
  -w /app \
  node:22 \
  bash -c "npm install && timeout 120 node test.mjs"
```

### 3. Verify results

- LLM response should stream to stdout showing the agent using write and bash tools
- Final output must include `✅ E2E TEST PASSED`
- If it fails, report the error and the stderr output

### 4. Clean up

Remove the temp directory after the test completes.

## Rules
- Always use a fresh temp directory — never run in the repo itself.
- Always install from the public npm registry — never use local links.
- If Docker mode, clean up the container's node_modules via `docker run --rm` before removing the host temp dir.
- Report the installed versions of `@rivet-dev/agent-os-core` and `@rivet-dev/agent-os-common` in the output.

## --with-dbt mode

When the user passes `--with-dbt`, additionally install
`@rivet-dev/agent-os-python-wheels` and run a trivial dbt project
end-to-end against in-memory DuckDB. This validates the L7 promise of
the dbt-on-Pyodide ralph plan.

**Extra package.json dependency:**
```json
"@rivet-dev/agent-os-python-wheels": "*"
```

**Additional test step (append to test.mjs after the main flow):**
```js
console.log("\n--- DBT smoke ---");
const dbtVm = await AgentOs.create({
  software: [common],
  python: { dbt: true },
});
try {
  await dbtVm.writeFiles([
    {
      path: "/root/dbt-projects/demo/dbt_project.yml",
      content: "name: 'demo'\nversion: '1.0.0'\nconfig-version: 2\nprofile: 'demo'\nmodel-paths: ['models']\ntarget-path: 'target'\n",
    },
    {
      path: "/root/dbt-projects/demo/models/example.sql",
      content: "{{ config(materialized='table') }}\nselect 1 as id, 'hello' as name\n",
    },
    {
      path: "/root/.dbt/profiles.yml",
      content: "demo:\n  target: dev\n  outputs:\n    dev:\n      type: duckdb\n      path: ':memory:'\n      threads: 1\n",
    },
  ]);
  await dbtVm.writeFile(
    "/tmp/run_dbt.py",
    "import os\nos.chdir('/root/dbt-projects/demo')\nfrom dbt.cli.main import dbtRunner\nres = dbtRunner().invoke(['run', '--threads', '1'])\nprint('success=', res.success)",
  );
  const dbtResult = await dbtVm.exec("python /tmp/run_dbt.py");
  console.log(dbtResult.stdout);
  if (!dbtResult.stdout.includes("success= True")) {
    console.log("❌ DBT SMOKE FAILED");
    process.exit(1);
  }
  const manifestExists = await dbtVm.exists("/root/dbt-projects/demo/target/manifest.json");
  if (!manifestExists) {
    console.log("❌ DBT SMOKE FAILED (no manifest)");
    process.exit(1);
  }
  console.log("✅ DBT SMOKE PASSED");
} finally {
  await dbtVm.dispose();
}
```

**Constraints to surface in --with-dbt mode:**
- Cold start adds ~10-15 seconds for the wheel preload
- The wheels package must be published to npm before this works against
  the public registry
- If `@rivet-dev/agent-os-python-wheels` is unpublished, the test will
  fail at `npm install` and the skill should report that the wheels
  package needs to be published first
