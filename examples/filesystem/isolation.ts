import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({ endpoint: "http://localhost:6420" });
const agent = client.vm.getOrCreate("my-agent");

// Seed a file into the VFS at boot. Bytes are copied into the kernel's
// in-memory filesystem; the host filesystem is never exposed to the guest.
await agent.filesystem.writeFile("/home/agentos/seed.json", JSON.stringify({ ok: true }));

// Write another file from the host after boot.
await agent.filesystem.writeFile("/home/agentos/note.txt", "written from the host\n");

// The guest reads both files through the normal node:fs API.
const result = await agent.process.exec(`node -e '
  const { readFileSync } = require("node:fs");
  const seed = JSON.parse(readFileSync("/home/agentos/seed.json", "utf8"));
  const note = readFileSync("/home/agentos/note.txt", "utf8").trim();
  console.log("guest read seed:", JSON.stringify(seed));
  console.log("guest read note:", note);
'`);
console.log("guest stdout:", result.stdout.trim());

// Read a guest-written file back on the host.
const bytes = await agent.filesystem.readFile("/home/agentos/seed.json");
console.log("host readFile:", new TextDecoder().decode(bytes));

// The same path on the real host disk does not exist. The VFS is isolated.
const { existsSync } = await import("node:fs");
console.log(
  "host disk sees /home/agentos/seed.json?",
  existsSync("/home/agentos/seed.json") ? "YES (unexpected!)" : "NO - isolated from host",
);
