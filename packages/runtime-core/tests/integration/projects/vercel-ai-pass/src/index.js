import { createAnthropic } from "@ai-sdk/anthropic";
import { createGateway } from "@ai-sdk/gateway";
import { createOpenAI } from "@ai-sdk/openai";
import { jsonSchema, tool } from "ai";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import * as workflow from "workflow";

const openai = createOpenAI({ apiKey: "test", baseURL: "http://127.0.0.1:1/v1" });
const anthropic = createAnthropic({ apiKey: "test", baseURL: "http://127.0.0.1:1" });
const gateway = createGateway({ apiKey: "test", baseURL: "http://127.0.0.1:1/v1/ai" });
const weather = tool({
  description: "Look up local weather",
  inputSchema: jsonSchema({
    type: "object",
    properties: { city: { type: "string" } },
    required: ["city"],
    additionalProperties: false,
  }),
});
const markup = renderToStaticMarkup(
  React.createElement("section", { "data-runtime": "agentos" }, "Vercel AI"),
);

console.log(JSON.stringify({
  openai: openai("gpt-5-mini").modelId,
  anthropic: anthropic("claude-sonnet-4-5").modelId,
  gateway: gateway("openai/gpt-5-mini").modelId,
  toolDescription: weather.description,
  react: markup,
  workflowExports: Object.keys(workflow).length > 0,
}));
