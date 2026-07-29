/**
 * Overlay readdir benchmark.
 *
 * Skipped in agentos: the source benchmark measured Agent OS' TypeScript
 * overlay layer store API, which is not exposed by @rivet-dev/agentos-runtime-core.
 */

import { getHardware } from "../lib/perf-utils.js";

const reason =
	"Agent OS TypeScript overlay layer-store APIs (createInMemoryLayerStore/createSnapshotExport) have no agentos package equivalent";

console.error(`overlay-readdir skipped: ${reason}`);
console.log(
	JSON.stringify(
		{
			benchmark: "overlay-readdir",
			skipped: true,
			reason,
			hardware: getHardware(),
			results: [],
		},
		null,
		2,
	),
);
