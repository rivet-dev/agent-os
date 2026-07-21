import { fileURLToPath } from "node:url";
import { AgentOs, nodeModulesMount } from "@rivet-dev/agentos";

// docs:start loading-modules
// Boot a fully virtualized VM. Module resolution runs entirely inside the
// kernel - `import` and `require` resolve against the guest's virtual
// filesystem, never the host's.
const rt = await AgentOs.create();

try {
	// Guest code runs as an ES module inside the VM, so it can `import` Node
	// builtins directly. It can also build a CommonJS `require` with
	// `createRequire` to load builtins the classic way. Both resolve through the
	// kernel's module loader.
	const {
		stdout = "",
		stderr = "",
		exitCode,
	} = await rt.javascript.execute(
		`
    // ESM import of a Node builtin.
    import { basename, join } from "node:path";

    // CommonJS require, created from the current module URL.
    import { createRequire } from "node:module";
    const require = createRequire(import.meta.url);
    const os = require("node:os");

    const resolved = {
      basename: basename("/workspace/data/report.txt"),
      joined: join("/workspace", "data", "report.txt"),
      platform: os.platform(),
    };

    console.log("resolved node:path via import ->", resolved.joined);
    console.log("resolved node:os via require ->", resolved.platform);
    console.log(JSON.stringify(resolved));
	  `,
		{ output: { capture: "all" } },
	);

	console.log("exitCode:", exitCode);
	if (stderr.trim()) console.log("guest stderr:\n" + stderr.trim());
	console.log("guest stdout:");
	console.log(stdout.trim());
} finally {
	await rt.dispose();
}
// docs:end loading-modules

// --- Loading real npm packages from the host ------------------------------

// docs:start npm-packages
// Point `nodeModules` at a host `node_modules` directory and the whole tree is
// projected into the VM in one call. Any package inside resolves the way Node
// would over a real filesystem, symlinks and all. Here we mount this repo's
// root node_modules, which includes the tiny `is-number` package.
const hostNodeModules = fileURLToPath(
	new URL("../../../../node_modules", import.meta.url),
);

const mounted = await AgentOs.create({
	mounts: [nodeModulesMount(hostNodeModules)],
});

try {
	// The guest resolves `is-number` from the mounted host node_modules the same
	// way Node would over a real filesystem, then uses the real package's code.
	const {
		stdout = "",
		stderr = "",
		exitCode,
	} = await mounted.javascript.execute(
		`
    // ESM import of the real, host-mounted npm package.
    import isNumber from "is-number";

    // The same package also resolves through a CommonJS require.
    import { createRequire } from "node:module";
    const require = createRequire(import.meta.url);
    const isNumberCjs = require("is-number");

    const result = {
      "isNumber(42)": isNumber(42),
      'isNumber("3.14")': isNumber("3.14"),
      'isNumber("nope")': isNumber("nope"),
      sameModule: isNumber === isNumberCjs,
    };

    console.log("loaded real npm package is-number");
    console.log(JSON.stringify(result));
	  `,
		{ output: { capture: "all" } },
	);

	console.log("exitCode:", exitCode);
	if (stderr.trim()) console.log("guest stderr:\n" + stderr.trim());
	console.log("guest stdout:");
	console.log(stdout.trim());
} finally {
	await mounted.dispose();
}
// docs:end npm-packages
