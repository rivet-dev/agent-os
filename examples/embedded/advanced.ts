import { AgentOs } from "@rivet-dev/agentos-core";

// An embedded application can use one dedicated sidecar process for multiple VMs.
const sidecar = await AgentOs.createSidecar();
const a = await AgentOs.create({
	sidecar: { kind: "explicit", handle: sidecar },
});
const b = await AgentOs.create({
	sidecar: { kind: "explicit", handle: sidecar },
});

await a.dispose(); // tears down VM a only
await b.dispose();
await sidecar.dispose(); // tears down the shared process
