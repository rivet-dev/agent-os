// Filesystem operations: write, read, mkdir, readdir, stat, move, delete.
//
// The VM creates an in-memory filesystem by default. Custom mounts
// (S3, host directories) can be configured at boot:
//
//   const vm = await AgentOs.create({
//     mounts: [{
//       path: "/data",
//       plugin: { id: "chunked_s3", config: { bucket: "my-bucket" } },
//     }],
//   });

import { AgentOs } from "@rivet-dev/agentos";

const vm = await AgentOs.create();

// Create a directory structure
await vm.filesystem.mkdir("/project");
await vm.filesystem.mkdir("/project/src");
await vm.filesystem.writeFile("/project/src/index.ts", 'console.log("hello");');
await vm.filesystem.writeFile("/project/README.md", "# My Project");

// List directory contents (filter out . and ..)
const entries = await vm.filesystem.readdir("/project");
console.log(
	"project/:",
	entries.filter((e) => e !== "." && e !== ".."),
);

// Stat a file
const info = await vm.filesystem.stat("/project/src/index.ts");
console.log("index.ts size:", info.size, "isDirectory:", info.isDirectory);

// Recursive directory listing
const tree = await vm.filesystem.readdirRecursive("/project", { maxDepth: 3 });
console.log("Recursive listing:", tree);

// Check existence
console.log("/project exists:", await vm.filesystem.exists("/project"));
console.log("/missing exists:", await vm.filesystem.exists("/missing"));

// Move a file
await vm.filesystem.move("/project/README.md", "/project/docs.md");
console.log("docs.md exists:", await vm.filesystem.exists("/project/docs.md"));

// Delete a file, then delete directory recursively
await vm.filesystem.remove("/project/docs.md");
await vm.filesystem.remove("/project", { recursive: true });
console.log("project exists after delete:", await vm.filesystem.exists("/project"));

await vm.dispose();
