import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const vm = client.vm.getOrCreate("persistent-agent");

// Files written before sleep are restored when the actor wakes.
const contents = await vm.filesystem.readFile("/home/agentos/notes.md");
console.log(new TextDecoder().decode(contents));
