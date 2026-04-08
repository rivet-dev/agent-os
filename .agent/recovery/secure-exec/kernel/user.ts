/**
 * User/group identity manager.
 *
 * Provides configurable uid/gid and passwd-entry generation for the kernel.
 * OS-level concern — lives in the kernel so all runtimes share the same identity.
 */

export interface UserConfig {
	uid?: number;
	gid?: number;
	euid?: number;
	egid?: number;
	username?: string;
	homedir?: string;
	shell?: string;
	gecos?: string;
}

export class UserManager {
	readonly uid: number;
	readonly gid: number;
	readonly euid: number;
	readonly egid: number;
	readonly username: string;
	readonly homedir: string;
	readonly shell: string;
	readonly gecos: string;

	constructor(config?: UserConfig) {
		this.uid = config?.uid ?? 1000;
		this.gid = config?.gid ?? 1000;
		this.euid = config?.euid ?? this.uid;
		this.egid = config?.egid ?? this.gid;
		this.username = config?.username ?? "user";
		this.homedir = config?.homedir ?? "/home/user";
		this.shell = config?.shell ?? "/bin/sh";
		this.gecos = config?.gecos ?? "";
	}

	/** Generate a passwd-format string for the given uid. */
	getpwuid(uid: number): string {
		if (uid === this.uid) {
			return `${this.username}:x:${this.uid}:${this.gid}:${this.gecos}:${this.homedir}:${this.shell}`;
		}
		// Generic entry for unknown uids
		const name = `user${uid}`;
		return `${name}:x:${uid}:${uid}::/home/${name}:/bin/sh`;
	}
}
