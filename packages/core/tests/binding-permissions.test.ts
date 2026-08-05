import common from "@agentos-software/common";
import { afterEach, describe, expect, test, vi } from "vitest";
import { z } from "zod";
import { AgentOs, binding, bindings } from "../src/index.js";
import { SidecarProcess } from "../src/sidecar/rpc-client.js";

// ---------------------------------------------------------------------------
// Adversarial host_callback RPC tests (security review: aos-ts N-001/N-002).
//
// THREAT MODEL: untrusted guest / agent code can emit a raw `host_callback`
// sidecar-request frame whose `callback_key` and `input` it fully controls.
// The CLI command path (`agentos-<collection> <binding>`) is gated by `bindingPermissionMode`
// via `invokeBinding`, but the raw `host_callback` RPC path is handled by
// `handleHostCallback` in agent-os.ts. These tests play the guest and assert
// the system DENIES the call (execute must never run) when policy denies or the
// binding is out of the granted pattern scope.
//
// We capture the real `SidecarRequestHandler` that `AgentOs.create()` installs
// on the sidecar client (via a prototype spy), then feed it forged
// `host_callback` frames — exactly the bytes an untrusted guest controls.
// ---------------------------------------------------------------------------

type CapturedHandler = (request: any) => Promise<any> | any;

async function createVmCapturingHandler(
	options: Parameters<typeof AgentOs.create>[0],
): Promise<{ vm: AgentOs; handler: CapturedHandler }> {
	let captured: CapturedHandler | null = null;
	const original =
		SidecarProcess.prototype.setSidecarRequestHandler;
	const spy = vi
		.spyOn(SidecarProcess.prototype, "setSidecarRequestHandler")
		.mockImplementation(function (
			this: SidecarProcess,
			handler: any,
		) {
			if (handler) {
				captured = handler as CapturedHandler;
			}
			// Still install on the real client so the VM behaves normally.
			return original.call(this, handler);
		});
	try {
		const vm = await AgentOs.create(options);
		if (!captured) {
			throw new Error(
				"AgentOs.create did not install a sidecar request handler",
			);
		}
		return { vm, handler: captured };
	} finally {
		spy.mockRestore();
	}
}

function hostCallbackFrame(callbackKey: string, input: unknown) {
	return {
		frame_type: "sidecar_request" as const,
		request_id: 1,
		payload: {
			type: "host_callback" as const,
			invocation_id: "guest-forged-1",
			callback_key: callbackKey,
			input,
			timeout_ms: 30_000,
		},
	};
}

// A forged *command-shaped* host_callback. `handleHostCallback` dispatches any
// input that parses as `{type:'command',command,args,cwd}` through the SECOND
// branch (`handleHostCommandCallback` -> `handleAgentOsBindingCommand` ->
// `invokeBinding`), bypassing the `callback_key`/Zod path entirely. We forge a
// CLI-style command frame to confirm THAT branch also enforces binding.invoke.
function commandHostCallbackFrame(command: string, args: string[]) {
	return {
		frame_type: "sidecar_request" as const,
		request_id: 1,
		payload: {
			type: "host_callback" as const,
			invocation_id: "guest-forged-cmd-1",
			// callback_key is irrelevant on the command branch; set it to a binding
			// that DOES exist to prove the command branch is what runs.
			callback_key: "math:add",
			input: {
				type: "command",
				command,
				args,
				cwd: "/home/agentos",
			},
			timeout_ms: 30_000,
		},
	};
}

const mathBindings = bindings({
	name: "math",
	description: "Math utilities",
	bindings: {
		add: binding({
			description: "Add two numbers",
			inputSchema: z.object({
				a: z.number(),
				b: z.number(),
			}),
			execute: ({ a, b }) => ({ sum: a + b }),
		}),
	},
});

const duplicateMathBindings = bindings({
	name: "math",
	description: "Duplicate math utilities",
	bindings: {
		multiply: binding({
			description: "Multiply two numbers",
			inputSchema: z.object({
				a: z.number(),
				b: z.number(),
			}),
			execute: ({ a, b }) => ({ product: a * b }),
		}),
	},
});

async function runCommand(vm: AgentOs, command: string, args: string[]) {
	const stdoutChunks: string[] = [];
	const stderrChunks: string[] = [];
	const { pid } = vm.spawn(command, args, {
		onStdout: (chunk) => {
			stdoutChunks.push(new TextDecoder().decode(chunk));
		},
		onStderr: (chunk) => {
			stderrChunks.push(new TextDecoder().decode(chunk));
		},
	});

	return {
		exitCode: await vm.waitProcess(pid),
		stdout: stdoutChunks.join(""),
		stderr: stderrChunks.join(""),
	};
}

describe("binding collection permissions", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		await vm?.dispose();
		vm = null;
	});

	test("rejects duplicate binding collection registration with a conflict", async () => {
		await expect(
			AgentOs.create({
				bindings: [mathBindings, duplicateMathBindings],
			}),
		).rejects.toThrow(/conflict: binding collection already registered: math/);
	});

	test("allows binding collection invocation with default permissions", async () => {
		vm = await AgentOs.create({
			software: [common],
			bindings: [mathBindings],
		});

		const result = await runCommand(vm, "agentos-math", [
			"add",
			"--a",
			"2",
			"--b",
			"3",
		]);
		expect(result.exitCode).toBe(0);
		expect(JSON.parse(result.stdout)).toEqual({
			ok: true,
			result: { sum: 5 },
		});
	});

	test("denies binding collection invocation by default until binding permissions are granted", async () => {
		vm = await AgentOs.create({
			software: [common],
			bindings: [mathBindings],
			permissions: {
				fs: "allow",
				childProcess: "allow",
			},
		});

		const result = await runCommand(vm, "agentos-math", [
			"add",
			"--a",
			"5",
			"--b",
			"7",
		]);
		expect(result.exitCode).toBe(1);
		expect(result.stdout).toBe("");
		expect(result.stderr).toContain("binding.invoke");
		expect(result.stderr).toContain("math:add");
	});

	test("allows binding collection invocation when a matching binding permission is granted", async () => {
		vm = await AgentOs.create({
			software: [common],
			bindings: [mathBindings],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				binding: {
					default: "deny",
					rules: [
						{
							mode: "allow",
							operations: ["invoke"],
							patterns: ["math:add"],
						},
					],
				},
			},
		});

		const result = await runCommand(vm, "agentos-math", [
			"add",
			"--a",
			"5",
			"--b",
			"7",
		]);
		expect(result.exitCode).toBe(0);
		expect(JSON.parse(result.stdout)).toEqual({
			ok: true,
			result: { sum: 12 },
		});
	});
});

describe("binding collection permissions — raw host_callback RPC path", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		await vm?.dispose();
		vm = null;
	});

	// N-001 (J.1/J.2): host_callback RPC must honor binding.invoke deny.
	test("denies host_callback RPC binding invocation when binding.invoke policy is deny (not just the CLI path)", async () => {
		const executed: unknown[] = [];
		const spyBindings = bindings({
			name: "math",
			description: "Math utilities",
			bindings: {
				add: binding({
					description: "Add two numbers",
					inputSchema: z.object({ a: z.number(), b: z.number() }),
					execute: ({ a, b }) => {
						executed.push({ a, b });
						return { sum: a + b };
					},
				}),
			},
		});

		const created = await createVmCapturingHandler({
			// No `software` needed: this exercises the raw host_callback RPC
			// handler directly (the guest-controlled path), which does not spawn
			// any in-VM CLI. Keeping the VM minimal makes the safeguard fast.
			bindings: [spyBindings],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				// Deny-by-default: no binding.invoke grant for math:add.
				binding: { default: "deny", rules: [] },
			},
		});
		vm = created.vm;

		const response = await created.handler(
			hostCallbackFrame("math:add", { a: 2, b: 3 }),
		);

		// The attacker must be denied: execute MUST NOT have run, and the
		// response must surface a policy denial rather than a result.
		expect(executed).toHaveLength(0);
		expect(response.type).toBe("host_callback_result");
		expect(response.result).toBeUndefined();
		expect(typeof response.error).toBe("string");
		expect(response.error).toMatch(/binding\.invoke|EACCES|denied|permission/i);
	});

	// N-002 (J.2): host_callback RPC must respect binding.invoke pattern scope.
	test("host_callback RPC respects binding.invoke pattern scope and denies a non-matching binding", async () => {
		const executed: string[] = [];
		const dangerBindings = bindings({
			name: "math",
			description: "Math utilities with a dangerous binding",
			bindings: {
				safe: binding({
					description: "Safe op",
					inputSchema: z.object({ x: z.number() }),
					execute: ({ x }) => {
						executed.push("safe");
						return { x };
					},
				}),
				danger: binding({
					description: "Dangerous op",
					inputSchema: z.object({ x: z.number() }),
					execute: ({ x }) => {
						executed.push("danger");
						return { x };
					},
				}),
			},
		});

		const created = await createVmCapturingHandler({
			bindings: [dangerBindings],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				// Only math:safe is allowed; math:danger is out of scope -> deny.
				binding: {
					default: "deny",
					rules: [
						{ mode: "allow", operations: ["invoke"], patterns: ["math:safe"] },
					],
				},
			},
		});
		vm = created.vm;

		const response = await created.handler(
			hostCallbackFrame("math:danger", { x: 1 }),
		);

		expect(executed).not.toContain("danger");
		expect(response.type).toBe("host_callback_result");
		expect(response.result).toBeUndefined();
		expect(typeof response.error).toBe("string");
		expect(response.error).toMatch(/binding\.invoke|EACCES|denied|permission/i);
	});

	// AOSFS-1 (P1, J.1/J.2): the raw host_callback RPC path is fully
	// guest-controlled, including the `input` object. The guest can stuff extra
	// keys, a `__proto__` payload, and a `constructor` key into `input` to try to
	// (a) leak raw unvalidated fields into the host-side `execute`, or (b) pollute
	// Object.prototype on the host. The handler runs `binding.inputSchema.safeParse`
	// and passes ONLY `parsed.data` to execute; a strict/stripping Zod object must
	// hand `execute` exactly the declared keys and nothing else, and no prototype
	// pollution may occur. Asserts the system strips the hostile/extra keys.
	test("host_callback strips hostile/extra input keys; execute receives only validated Zod data and no prototype pollution", async () => {
		const seen: unknown[] = [];
		const collection = bindings({
			name: "math",
			description: "Math utilities",
			bindings: {
				add: binding({
					description: "Add two numbers",
					inputSchema: z.object({ a: z.number(), b: z.number() }),
					execute: (input) => {
						// Capture exactly what execute is handed.
						seen.push(input);
						const { a, b } = input;
						return { sum: a + b };
					},
				}),
			},
		});

		const created = await createVmCapturingHandler({
			bindings: [collection],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				binding: {
					default: "deny",
					rules: [
						{ mode: "allow", operations: ["invoke"], patterns: ["math:add"] },
					],
				},
			},
		});
		vm = created.vm;

		// Hostile input: declared keys + extra fields + a prototype-pollution
		// payload. Build via JSON so __proto__ is a real own enumerable key (the
		// exact shape an untrusted guest sends over the wire).
		const hostileInput = JSON.parse(
			'{"a":2,"b":3,"evilField":"leak-me","secret":"do-not-pass","__proto__":{"polluted":"yes"},"constructor":{"prototype":{"polluted2":"yes"}}}',
		);

		const response = await created.handler(
			hostCallbackFrame("math:add", hostileInput),
		);

		// The binding ran (policy allows math:add) and produced the correct result.
		expect(response.type).toBe("host_callback_result");
		expect(response.error).toBeUndefined();
		expect(response.result).toEqual({ sum: 5 });

		// execute saw EXACTLY the declared keys — no leaked hostile/extra fields.
		expect(seen).toHaveLength(1);
		const handed = seen[0] as Record<string, unknown>;
		expect(Object.keys(handed).sort()).toEqual(["a", "b"]);
		expect(handed.a).toBe(2);
		expect(handed.b).toBe(3);
		expect(handed).not.toHaveProperty("evilField");
		expect(handed).not.toHaveProperty("secret");

		// No prototype pollution of Object.prototype on the host.
		expect(({} as Record<string, unknown>).polluted).toBeUndefined();
		expect(({} as Record<string, unknown>).polluted2).toBeUndefined();
		expect(
			Object.prototype.hasOwnProperty.call(Object.prototype, "polluted"),
		).toBe(false);
	});

	// AOSFS-2 (P2): a guest can send schema-failing input on the raw host_callback
	// RPC path (which does NOT go through the CLI argv parser / sidecar-binding
	// dispatch validation at sidecar-binding-dispatch:108). The handler must
	// safeParse and return a validation error WITHOUT invoking execute.
	test("host_callback rejects schema-failing input without invoking execute", async () => {
		const executed: unknown[] = [];
		const collection = bindings({
			name: "math",
			description: "Math utilities",
			bindings: {
				add: binding({
					description: "Add two numbers",
					inputSchema: z.object({ a: z.number(), b: z.number() }),
					execute: ({ a, b }) => {
						executed.push({ a, b });
						return { sum: a + b };
					},
				}),
			},
		});

		const created = await createVmCapturingHandler({
			bindings: [collection],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				binding: {
					default: "deny",
					rules: [
						{ mode: "allow", operations: ["invoke"], patterns: ["math:add"] },
					],
				},
			},
		});
		vm = created.vm;

		// `a` is the wrong type; `b` is missing entirely.
		const response = await created.handler(
			hostCallbackFrame("math:add", { a: "not-a-number" }),
		);

		expect(executed).toHaveLength(0);
		expect(response.type).toBe("host_callback_result");
		expect(response.result).toBeUndefined();
		expect(typeof response.error).toBe("string");
		// Zod validation message (number expected / required), not a thrown crash.
		expect(response.error).toMatch(/number|expected|required|invalid|nan/i);
	});

	// AOS-SESS-4 (N-014, P2, J.1/J.2): the *command-shaped* host_callback dispatch
	// branch (handleHostCommandCallback -> invokeBinding) must ALSO honor
	// binding.invoke deny — defense-in-depth on the second dispatch path that the
	// callback_key/Zod branch does not cover. (Hold-as-regression; not a
	// re-discovery — assert the gate holds on this branch.)
	test("forged {type:'command'} host_callback is denied by binding.invoke on the command dispatch branch", async () => {
		const executed: unknown[] = [];
		const spyBindings = bindings({
			name: "math",
			description: "Math utilities",
			bindings: {
				add: binding({
					description: "Add two numbers",
					inputSchema: z.object({ a: z.number(), b: z.number() }),
					execute: ({ a, b }) => {
						executed.push({ a, b });
						return { sum: a + b };
					},
				}),
			},
		});

		const created = await createVmCapturingHandler({
			bindings: [spyBindings],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				// Deny-by-default: no binding.invoke grant for math:add.
				binding: { default: "deny", rules: [] },
			},
		});
		vm = created.vm;

		// Forge `agentos-math add --a 2 --b 3` as a command host_callback.
		const response = await created.handler(
			commandHostCallbackFrame("agentos-math", ["add", "--a", "2", "--b", "3"]),
		);

		// The attacker must be denied on the command branch too: execute MUST NOT
		// have run and the response must surface a policy denial, not a result.
		expect(executed).toHaveLength(0);
		expect(response.type).toBe("host_callback_result");
		expect(response.result).toBeUndefined();
		expect(typeof response.error).toBe("string");
		expect(response.error).toMatch(/binding\.invoke|EACCES|denied|permission/i);
	});
});
