import type { SoftwarePackageRef } from "@agentos-software/manifest";
import packageJson from "../package.json" with { type: "json" };

const packagePath = new URL("./package.aospkg", import.meta.url).pathname;

export const appsBuilderVersion = packageJson.version;
export const appBundleManifestVersion = 1;

export default { packagePath } satisfies SoftwarePackageRef;
