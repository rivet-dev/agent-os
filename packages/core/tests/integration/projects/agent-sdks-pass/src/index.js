import OpenAI from "openai";
import Anthropic from "@anthropic-ai/sdk";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { RunnableLambda } from "@langchain/core/runnables";
import { google } from "googleapis";
import { Octokit } from "octokit";

const openai = new OpenAI({ apiKey: "test", baseURL: "http://127.0.0.1:1" });
const anthropic = new Anthropic({ apiKey: "test", baseURL: "http://127.0.0.1:1" });

const server = new Server({ name: "fixture-server", version: "1.0.0" }, { capabilities: {} });
const client = new Client({ name: "fixture-client", version: "1.0.0" });
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);
const mcpVersion = (await client.getServerVersion()).name;
await client.close();
await server.close();

const runnable = RunnableLambda.from(async value => ({ doubled: value.count * 2 }));
const langchainResult = await runnable.invoke({ count: 4 });

const oauth = new google.auth.OAuth2("client", "secret", "http://localhost/callback");
const authUrl = oauth.generateAuthUrl({ access_type: "offline", scope: ["scope.one"] });
const octokit = new Octokit({ auth: "token" });
const endpoint = octokit.request.endpoint("GET /repos/{owner}/{repo}", { owner: "rivet-dev", repo: "agent-os" });

console.log(JSON.stringify({
  openai: openai.baseURL,
  anthropic: anthropic.baseURL,
  mcpVersion,
  langchain: langchainResult.doubled,
  google: authUrl.includes("client_id=client"),
  octokit: endpoint.url,
}));
