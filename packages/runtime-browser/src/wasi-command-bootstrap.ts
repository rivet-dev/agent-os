export interface WasiCommandBootstrapOptions {
	/** Main WASI command source URL, usually a staged browser asset such as /commands/sh. */
	commandSource: string;
	/** argv[0] for the main command. */
	command: string;
	/** Additional argv entries for the main command. */
	args?: string[];
	/** Guest executable paths mapped to WASM sources for host_process.proc_spawn. */
	commands?: Record<string, string>;
	env?: Record<string, string>;
	cwd?: string;
	preopens?: Record<string, string>;
	bootMessage?: string;
	bytesMessagePrefix?: string;
	startMessage?: string;
	exitMessagePrefix?: string;
	errorMessagePrefix?: string;
}

function json(value: unknown): string {
	return JSON.stringify(value);
}

/**
 * Builds the guest JavaScript that launches a real WASI command in the browser
 * executor with the production `agentos:wasi-command-host` spawn/FD bridge.
 */
export function createWasiCommandBootstrapScript(
	options: WasiCommandBootstrapOptions,
): string {
	const env = options.env ?? {};
	const cwd = options.cwd ?? "/";
	const preopens = options.preopens ?? { "/": "/" };
	const args = [options.command, ...(options.args ?? [])];
	const commands = options.commands ?? {};
	const bootMessage = options.bootMessage ?? "";
	const bytesMessagePrefix = options.bytesMessagePrefix ?? "";
	const startMessage = options.startMessage ?? "";
	const exitMessagePrefix = options.exitMessagePrefix ?? "WASI_EXIT:";
	const errorMessagePrefix =
		options.errorMessagePrefix ?? "WASI_COMMAND_ERROR:";

	return `
	(async () => {
		try {
			const commandSource = ${json(options.commandSource)};
			if (${json(bootMessage)}) process.stdout.write(${json(bootMessage)} + "\\n");
			const response = await fetch(commandSource);
			if (!response.ok) {
				throw new Error("failed to fetch real command wasm " + commandSource + ": " + response.status);
			}
			let bytes = new Uint8Array(await response.arrayBuffer());
			if (response.headers.get("x-body-encoding") === "base64") {
				const encoded = new TextDecoder().decode(bytes);
				bytes = Uint8Array.from(atob(encoded), (char) => char.charCodeAt(0));
			}
			if (${json(bytesMessagePrefix)}) {
				process.stdout.write(${json(bytesMessagePrefix)} + bytes.byteLength + "\\n");
			}
			const { WASI } = require("node:wasi");
			const { createWasiCommandHost } = require("agentos:wasi-command-host");
			const commandHost = await createWasiCommandHost({
				WASI,
				commands: ${json(commands)},
				env: ${json(env)},
				cwd: ${json(cwd)},
			});
			commandHost.installBlockingStdin(process);
			const wasi = new WASI({
				returnOnExit: true,
				args: ${json(args)},
				env: ${json(env)},
				preopens: ${json(preopens)},
			});
			commandHost.setParentWasi(wasi);
			const { instance } = await WebAssembly.instantiate(bytes, {
				wasi_snapshot_preview1: wasi.wasiImport,
				...commandHost.imports,
			});
			commandHost.setMemory(instance.exports.memory);
			if (${json(startMessage)}) process.stdout.write(${json(startMessage)} + "\\n");
			const exitCode = wasi.start(instance);
			process.stdout.write(${json(exitMessagePrefix)} + exitCode + "\\n");
		} catch (error) {
			process.stderr.write(${json(errorMessagePrefix)} + (error && error.stack ? error.stack : String(error)) + "\\n");
			process.exit(1);
		}
	})();
`;
}
