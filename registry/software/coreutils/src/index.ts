import type { WasmCommandPackage } from "@rivet-dev/agent-os-registry-types";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pkg = {
	name: "coreutils",
	aptName: "coreutils",
	description:
		"GNU coreutils: sh, 80+ commands, stubs, checksums, base encoding",
	source: "rust" as const,
	commands: [
		// Shell
		{ name: "sh", permissionTier: "full" as const },
		{ name: "bash", permissionTier: "full" as const, aliasOf: "sh" },

		// Process control shims (need spawn capability)
		{ name: "env", permissionTier: "full" as const },
		{ name: "timeout", permissionTier: "full" as const },
		{ name: "nice", permissionTier: "full" as const },
		{ name: "nohup", permissionTier: "full" as const },
		{ name: "stdbuf", permissionTier: "full" as const },

		// File operations (read-write)
		{ name: "chmod", permissionTier: "read-write" as const },
		{ name: "cp", permissionTier: "read-write" as const },
		{ name: "dd", permissionTier: "read-write" as const },
		{ name: "link", permissionTier: "read-write" as const },
		{ name: "ln", permissionTier: "read-write" as const },
		{ name: "mkdir", permissionTier: "read-write" as const },
		{ name: "mktemp", permissionTier: "read-write" as const },
		{ name: "mv", permissionTier: "read-write" as const },
		{ name: "rm", permissionTier: "read-write" as const },
		{ name: "rmdir", permissionTier: "read-write" as const },
		{ name: "shred", permissionTier: "read-write" as const },
		{ name: "split", permissionTier: "read-write" as const },
		{ name: "touch", permissionTier: "read-write" as const },
		{ name: "truncate", permissionTier: "read-write" as const },
		{ name: "unlink", permissionTier: "read-write" as const },

		// File operations (read-only)
		{ name: "cat", permissionTier: "read-only" as const },
		{ name: "more", permissionTier: "read-only" as const, aliasOf: "cat" },
		{ name: "head", permissionTier: "read-only" as const },
		{ name: "tail", permissionTier: "read-only" as const },
		{ name: "ls", permissionTier: "read-only" as const },
		{ name: "dir", permissionTier: "read-only" as const, aliasOf: "ls" },
		{ name: "vdir", permissionTier: "read-only" as const, aliasOf: "ls" },
		{ name: "stat", permissionTier: "read-only" as const },
		{ name: "du", permissionTier: "read-only" as const },
		{ name: "dircolors", permissionTier: "read-only" as const },
		{ name: "readlink", permissionTier: "read-only" as const },
		{ name: "realpath", permissionTier: "read-only" as const },
		{ name: "pathchk", permissionTier: "read-only" as const },

		// Text processing
		{ name: "tee", permissionTier: "read-write" as const },
		{ name: "echo", permissionTier: "read-only" as const },
		{ name: "printf", permissionTier: "read-only" as const },
		{ name: "wc", permissionTier: "read-only" as const },
		{ name: "sort", permissionTier: "read-only" as const },
		{ name: "uniq", permissionTier: "read-only" as const },
		{ name: "cut", permissionTier: "read-only" as const },
		{ name: "tr", permissionTier: "read-only" as const },
		{ name: "paste", permissionTier: "read-only" as const },
		{ name: "comm", permissionTier: "read-only" as const },
		{ name: "join", permissionTier: "read-only" as const },
		{ name: "fold", permissionTier: "read-only" as const },
		{ name: "expand", permissionTier: "read-only" as const },
		{ name: "unexpand", permissionTier: "read-only" as const },
		{ name: "nl", permissionTier: "read-only" as const },
		{ name: "fmt", permissionTier: "read-only" as const },
		{ name: "od", permissionTier: "read-only" as const },
		{ name: "ptx", permissionTier: "read-only" as const },
		{ name: "numfmt", permissionTier: "read-only" as const },
		{ name: "column", permissionTier: "read-only" as const },
		{ name: "rev", permissionTier: "read-only" as const },
		{ name: "strings", permissionTier: "read-only" as const },
		{ name: "tac", permissionTier: "read-only" as const },
		{ name: "tsort", permissionTier: "read-only" as const },

		// Math and sequences
		{ name: "seq", permissionTier: "read-only" as const },
		{ name: "shuf", permissionTier: "read-only" as const },
		{ name: "factor", permissionTier: "read-only" as const },
		{ name: "expr", permissionTier: "read-only" as const },

		// Test and logic
		{ name: "test", permissionTier: "read-only" as const },
		{ name: "[", permissionTier: "read-only" as const, aliasOf: "test" },
		{ name: "true", permissionTier: "read-only" as const },
		{ name: "false", permissionTier: "read-only" as const },
		{ name: "yes", permissionTier: "read-only" as const },

		// System info
		{ name: "arch", permissionTier: "read-only" as const },
		{ name: "date", permissionTier: "read-only" as const },
		{ name: "nproc", permissionTier: "read-only" as const },
		{ name: "uname", permissionTier: "read-only" as const },
		{ name: "logname", permissionTier: "read-only" as const },
		{ name: "whoami", permissionTier: "read-only" as const },
		{ name: "printenv", permissionTier: "read-only" as const },
		{ name: "pwd", permissionTier: "read-only" as const },
		{ name: "basename", permissionTier: "read-only" as const },
		{ name: "dirname", permissionTier: "read-only" as const },
		{ name: "xu", permissionTier: "read-only" as const },
		{ name: "which", permissionTier: "read-only" as const },
		{ name: "sleep", permissionTier: "read-only" as const },

		// Checksums and encoding
		{ name: "md5sum", permissionTier: "read-only" as const },
		{ name: "sha1sum", permissionTier: "read-only" as const },
		{ name: "sha224sum", permissionTier: "read-only" as const },
		{ name: "sha256sum", permissionTier: "read-only" as const },
		{ name: "sha384sum", permissionTier: "read-only" as const },
		{ name: "sha512sum", permissionTier: "read-only" as const },
		{ name: "b2sum", permissionTier: "read-only" as const },
		{ name: "cksum", permissionTier: "read-only" as const },
		{ name: "sum", permissionTier: "read-only" as const },
		{ name: "base32", permissionTier: "read-only" as const },
		{ name: "base64", permissionTier: "read-only" as const },
		{ name: "basenc", permissionTier: "read-only" as const },

		// Stubs (unimplemented commands that return graceful errors)
		{ name: "_stubs", permissionTier: "read-only" as const },
		{ name: "chcon", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{
			name: "runcon",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{ name: "chgrp", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{ name: "chown", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{
			name: "chroot",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{ name: "df", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{
			name: "groups",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{ name: "id", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{
			name: "hostname",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{
			name: "hostid",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{
			name: "install",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{ name: "kill", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{
			name: "mkfifo",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{ name: "mknod", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{ name: "pinky", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{ name: "who", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{ name: "users", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{
			name: "uptime",
			permissionTier: "read-only" as const,
			aliasOf: "_stubs",
		},
		{ name: "stty", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{ name: "sync", permissionTier: "read-only" as const, aliasOf: "_stubs" },
		{ name: "tty", permissionTier: "read-only" as const, aliasOf: "_stubs" },
	],
	get commandDir() {
		return resolve(__dirname, "..", "wasm");
	},
} satisfies WasmCommandPackage;

export default pkg;
