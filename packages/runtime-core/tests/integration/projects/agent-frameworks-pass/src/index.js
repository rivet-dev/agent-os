import http from "node:http";

const load = async name => {
  return import(name);
};
const { App } = await load("@slack/bolt");
const { WebClient } = await load("@slack/web-api");
const codeInterpreter = await load("@e2b/code-interpreter");
const { createStep, createWorkflow } = await load("@mastra/core/workflows");
const e2b = await load("e2b");
const { Effect } = await load("effect");
const { z } = await load("zod");

const server = http.createServer((request, response) => {
  let body = "";
  request.setEncoding("utf8");
  request.on("data", chunk => { body += chunk; });
  request.on("end", () => {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({
      ok: true,
      team_id: "T_AGENTOS",
      request: request.url,
      hasToken: body.includes("token=test-token"),
    }));
  });
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const address = server.address();
if (!address || typeof address === "string") throw new Error("Slack mock did not bind");

try {
  const slack = new WebClient("test-token", {
    slackApiUrl: `http://127.0.0.1:${address.port}/api/`,
  });
  const auth = await slack.auth.test();
  const effectResult = await Effect.runPromise(
    Effect.succeed(20).pipe(Effect.map(value => value + 22)),
  );

  const step = createStep({
    id: "double",
    inputSchema: z.object({ value: z.number() }),
    outputSchema: z.object({ value: z.number() }),
    execute: async ({ inputData }) => ({ value: inputData.value * 2 }),
  });
  const workflow = createWorkflow({
    id: "agentos-workflow",
    inputSchema: z.object({ value: z.number() }),
    outputSchema: z.object({ value: z.number() }),
  }).then(step).commit();
  const run = await workflow.createRun();
  const result = await run.start({ inputData: { value: 21 } });

  console.log(JSON.stringify({
    slack: auth.team_id,
    boltFramework: typeof App === "function",
    effect: effectResult,
    mastra: result.status === "success" ? result.result.value : null,
    e2b: typeof e2b.Sandbox === "function",
    codeInterpreter: typeof codeInterpreter.Sandbox === "function",
  }));
} finally {
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
}
