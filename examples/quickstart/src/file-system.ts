// Filesystem operations: write, read, list.
//
// This example uses the default in-memory filesystem. For persistent
// storage, pass a custom mount:
//
//   import { createS3Backend } from "@rivet-dev/agent-os-s3";
//   const vm = await AgentOs.create({
//     mounts: [{
//       path: "/data",
//       plugin: createS3Backend({ bucket: "my-bucket" }),
//     }],
//   });

import { AgentOs } from "@rivet-dev/agent-os-core";

const os = await AgentOs.create();

await os.writeFile("/workspace/hello.txt", "Hello, world!");
const content = await os.readFile("/workspace/hello.txt");
const files = await os.readdir("/workspace");
