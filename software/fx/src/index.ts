import type { SoftwarePackageRef } from "@agentos-software/manifest";

const packagePath = new URL("./package.aospkg", import.meta.url).pathname;

export default { packagePath } satisfies SoftwarePackageRef;
