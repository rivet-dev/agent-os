import { getDownloadUrl } from "@vercel/blob";
import { createClient } from "@vercel/edge-config";
import { geolocation, ipAddress } from "@vercel/functions";
import { nodeFileTrace } from "@vercel/nft";
import { fileURLToPath } from "node:url";
import path from "node:path";

const entry = fileURLToPath(import.meta.url);
const projectDir = path.resolve(path.dirname(entry), "..");
const trace = await nodeFileTrace([entry], { base: projectDir });
const request = new Request("https://example.com/agent", {
  headers: {
    "x-forwarded-for": "203.0.113.10",
    "x-vercel-ip-city": "Portland",
    "x-vercel-ip-country": "US",
  },
});
const edgeConfig = createClient(
  "https://edge-config.vercel.com/ecfg_test?token=test-token",
);
const optionalModules = [];
for (const name of [
  "@vercel/express",
  "@vercel/fastify",
  "@vercel/flags",
  "flags",
  "@vercel/hono",
  "@vercel/og",
  "@vercel/oidc",
  "@vercel/otel",
  "@vercel/sandbox",
  "@vercel/sdk/sdk/user.js",
]) {
  optionalModules.push(await import(name));
}

console.log(JSON.stringify({
  blob: getDownloadUrl("https://example.public.blob.vercel-storage.com/report.pdf"),
  edgeConfig: typeof edgeConfig.get === "function",
  geo: geolocation(request),
  ip: ipAddress(request),
  tracedEntry: trace.fileList.has("src/index.js"),
  serverModules: optionalModules.every(module => Object.keys(module).length > 0),
}));
