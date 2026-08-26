export const movedMessage =
	"The Dynamic Apps builder moved to @rivet-dev/dynamic-apps-builder. Install it and import from \"@rivet-dev/dynamic-apps-builder\".";

throw new Error(movedMessage);

/** @deprecated Install and import from `@rivet-dev/dynamic-apps-builder`. */
export interface SoftwarePackageRef {
	packagePath: string;
}

/** @deprecated Install and import from `@rivet-dev/dynamic-apps-builder`. */
export const appsBuilderVersion = "moved";

/** @deprecated Install and import from `@rivet-dev/dynamic-apps-builder`. */
export const appBundleManifestVersion = 1;

const packageRef: SoftwarePackageRef = { packagePath: "" };
export default packageRef;
