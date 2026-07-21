import { AgentOs, nodeModulesMount } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

// The full AgentOs.create() configuration surface. The agentOS() actor accepts
// this same options object and layers persistence, sleep/wake, and preview URLs
// on top.
const vm = await AgentOs.create({
  // Filesystems to mount at boot. Use nodeModulesMount() to expose a host
  // node_modules tree at /root/node_modules.
  mounts: [nodeModulesMount("/path/to/project/node_modules")],
  // Software packages to install in the VM (see /docs/software)
  software: [pi],
  // Also install the default software bundle (sh + coreutils). Defaults to true;
  // set false for a bare VM with only the software you list.
  defaultSoftware: true,
  // Ports exempt from SSRF checks (for testing against host-side mock servers)
  loopbackExemptPorts: [3000],
  // Sidecar placement — defaults to the shared `default` pool
  sidecar: { kind: "shared" },
});
