import type { FilesystemEntry } from "./filesystem-snapshot.js";

export interface BaseFilesystemEnvironment {
	env: Record<string, string>;
	prompt: string;
}

export type BaseFilesystemEntry = FilesystemEntry;

export interface BaseFilesystemSnapshot {
	source?: {
		snapshotPath?: string;
		image?: string;
		snapshotCreatedAt?: string;
		builtAt?: string;
		transforms?: string[];
	};
	environment: BaseFilesystemEnvironment;
	filesystem: {
		entries: BaseFilesystemEntry[];
	};
}

/**
 * The base VM environment, baked in as a constant (verbatim from the single
 * `base-filesystem.json` the sidecar embeds). The host no longer reads that JSON
 * — the sidecar owns the base filesystem, and there is exactly one committed copy
 * of it (`agentos/crates/vfs/assets/base-filesystem.json`). Regenerate both
 * this constant and that file together with the build-tools snapshot script.
 */
const BASE_ENVIRONMENT: Readonly<Record<string, string>> = Object.freeze({
	CHARSET: "UTF-8",
	HOME: "/home/agentos",
	HOSTNAME: "agentos",
	LANG: "C.UTF-8",
	LC_COLLATE: "C",
	LOGNAME: "agentos",
	PAGER: "less",
	PATH: "/usr/local/sbin:/usr/local/bin:/opt/agentos/bin:/usr/sbin:/usr/bin:/sbin:/bin",
	MANPATH: "/opt/agentos/share/man:/usr/local/share/man:/usr/share/man",
	SHELL: "/bin/sh",
	USER: "agentos",
	PS1: "\\h:\\w\\$ ",
});

/** The default VM environment (a fresh, mutable copy per call). */
export function getBaseEnvironment(): Record<string, string> {
	return { ...BASE_ENVIRONMENT };
}
