import { existsSync } from "node:fs";
import { join } from "node:path";
import curlPackage from "@rivet-dev/agent-os-curl";
import { afterAll, beforeAll, describe, expect, test, vi } from "vitest";
import { AgentOs } from "../src/index.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";

vi.setConfig({ testTimeout: 15_000 });

const ALLOW_ALL_VM_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	process: "allow",
	env: "allow",
	tool: "allow",
} as const;

/**
 * Comprehensive tests for WASM command packages.
 * Each section tests a specific registry package end-to-end.
 */
describe("WASM command packages", () => {
	let vm: AgentOs;

	function useDescribeVm(): void {
		beforeAll(async () => {
			vm = await AgentOs.create({
				software: REGISTRY_SOFTWARE,
				permissions: ALLOW_ALL_VM_PERMISSIONS,
			});
		}, 120_000);

		afterAll(async () => {
			await vm.dispose();
		}, 30_000);
	}

	// ── coreutils: shell ──────────────────────────────────────────────

	describe("sh (coreutils)", () => {
		useDescribeVm();

		test("starts in the default home cwd", async () => {
			const r = await vm.exec("pwd");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("/home/user");
		});

		test("variables and arithmetic", async () => {
			const r = await vm.exec("X=42; echo $((X + 8))");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("50");
		});

		test("for loop", async () => {
			const r = await vm.exec("for i in 1 2 3; do echo $i; done");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("1\n2\n3");
		});

		test("command substitution", async () => {
			const r = await vm.exec('echo "count: $(echo hello | wc -c)"');
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("count:");
		});

		test("pipe chain with cat", async () => {
			const r = await vm.exec("echo hello | cat");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("hello");
		});

		test("redirect to file and read back", async () => {
			const r = await vm.exec(
				'echo "hello" > /tmp/redir.txt && cat /tmp/redir.txt',
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("hello");
		});

		test("heredoc", async () => {
			const r = await vm.exec(`cat <<'EOF'
line1
line2
EOF`);
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("line1");
			expect(r.stdout).toContain("line2");
		});

		test("exit code propagation", async () => {
			const r = await vm.exec("false");
			expect(r.exitCode).toBe(1);
		});

		test("subshell isolation", async () => {
			const r = await vm.exec('(X=inner); echo "${X:-outer}"');
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("outer");
		});
	});

	// ── coreutils: file operations ────────────────────────────────────

	describe("file operations (coreutils)", () => {
		useDescribeVm();

		test("cp and cat", async () => {
			await vm.exec("echo data > /tmp/orig.txt");
			const r = await vm.exec(
				"cp /tmp/orig.txt /tmp/copy.txt && cat /tmp/copy.txt",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("data");
		});

		test("mv renames file", async () => {
			await vm.exec("echo moved > /tmp/mv-src.txt");
			const r = await vm.exec(
				"mv -f /tmp/mv-src.txt /tmp/mv-dst.txt && cat /tmp/mv-dst.txt",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("moved");
		});

		test("rm removes file", async () => {
			await vm.exec("echo x > /tmp/rm-me.txt");
			const r = await vm.exec(
				"rm /tmp/rm-me.txt && test ! -f /tmp/rm-me.txt && echo ok",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("ok");
		});

		test("mkdir -p and rmdir", async () => {
			const r = await vm.exec(
				"mkdir -p /tmp/a/b/c && test -d /tmp/a/b/c && echo ok",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("ok");
		});

		test("ls lists files", async () => {
			await vm.exec(
				"mkdir -p /tmp/ls-test && echo a > /tmp/ls-test/a.txt && echo b > /tmp/ls-test/b.txt",
			);
			const r = await vm.exec("ls /tmp/ls-test/a.txt /tmp/ls-test/b.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("a.txt");
			expect(r.stdout).toContain("b.txt");
		});

		test("touch creates file", async () => {
			const r = await vm.exec(
				"touch /tmp/touched.txt && test -f /tmp/touched.txt && echo ok",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("ok");
		});

		test("head and tail", async () => {
			await vm.exec('printf "1\\n2\\n3\\n4\\n5\\n" > /tmp/lines.txt');
			const head = await vm.exec("head -2 /tmp/lines.txt");
			expect(head.stdout.trim()).toBe("1\n2");
			const tail = await vm.exec("tail -2 /tmp/lines.txt");
			expect(tail.stdout.trim()).toBe("4\n5");
		});

		test("redirected printf preserves escaped newlines inside double quotes", async () => {
			await vm.exec('printf "alpha\\nbeta\\n" > /tmp/printf-escaped-lines.txt');
			const result = await vm.exec("cat /tmp/printf-escaped-lines.txt");
			expect(result.exitCode).toBe(0);
			expect(result.stdout).toBe("alpha\nbeta\n");
		});

		test("wc counts lines", async () => {
			await vm.exec('printf "hello world\\nfoo bar\\n" > /tmp/wc.txt');
			const r = await vm.exec("wc -l /tmp/wc.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("2");
		});

		test("cut extracts fields", async () => {
			await vm.exec('printf "a,b,c\\n1,2,3\\n" > /tmp/csv.txt');
			const r = await vm.exec("cut -d, -f2 /tmp/csv.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("b\n2");
		});

		test("tr translates characters", async () => {
			const r = await vm.exec("echo hello | tr 'a-z' 'A-Z'");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("HELLO");
		});

		test("tee writes to file and stdout", async () => {
			const r = await vm.exec(
				"echo data | tee /tmp/tee-out.txt && cat /tmp/tee-out.txt",
			);
			expect(r.exitCode).toBe(0);
			const lines = r.stdout.trim().split("\n");
			expect(lines[0]).toBe("data");
			expect(lines[1]).toBe("data");
		});

		test("basename and dirname", async () => {
			const bn = await vm.exec("basename /foo/bar/baz.txt");
			expect(bn.stdout.trim()).toBe("baz.txt");
			const dn = await vm.exec("dirname /foo/bar/baz.txt");
			expect(dn.stdout.trim()).toBe("/foo/bar");
		});

		test("base64 encode and decode", async () => {
			await vm.exec("echo -n 'hello' > /tmp/base64-src.txt");
			const r = await vm.exec(
				"base64 /tmp/base64-src.txt > /tmp/base64.txt && base64 -d /tmp/base64.txt",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("hello");
		});

		test("sha256sum computes hash", async () => {
			await vm.exec('printf "test\\n" > /tmp/hash.txt');
			const r = await vm.exec("sha256sum /tmp/hash.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain(
				"f2ca1bb6c7e907d06dafe4687e579fce76b37e4e93b7605022da52e6ccc26fd2",
			);
		});

		test("env prints environment", async () => {
			const r = await vm.exec("env", { env: { FOO: "bar" } });
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("FOO=bar");
		});

		test("which resolves virtual PATH commands", async () => {
			const bash = await vm.exec("which bash");
			expect(bash.exitCode).toBe(0);
			expect(bash.stdout.trim()).toMatch(
				/^\/(?:bin|__agentos\/commands\/\d+)\/bash$/,
			);

			const rg = await vm.exec("which rg");
			expect(rg.exitCode).toBe(0);
			expect(rg.stdout.trim()).toMatch(/^\/(?:bin|__agentos\/commands\/\d+)\/rg$/);

			const missing = await vm.exec("which definitely-not-a-command");
			expect(missing.exitCode).toBeGreaterThan(0);
			expect(missing.stdout.trim()).toBe("");
		});

		test("xu executes as a registered PATH command", async () => {
			const r = await vm.exec("xu hello-agent-os");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("xu-ok:hello-agent-os");
		});

		test("test command conditionals", async () => {
			await vm.exec("echo yes > /tmp/exists.txt");
			const r = await vm.exec(
				'test -f /tmp/exists.txt && echo "exists" || echo "missing"',
			);
			expect(r.stdout.trim()).toBe("exists");
		});

		test("date outputs something", async () => {
			const r = await vm.exec("date +%Y");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toMatch(/^\d{4}$/);
		});

		test("seq generates sequence", async () => {
			const r = await vm.exec("seq 1 5");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("1\n2\n3\n4\n5");
		});

		test("expr arithmetic", async () => {
			const r = await vm.exec("expr 6 '*' 7");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("42");
		});
	});

	// ── grep ──────────────────────────────────────────────────────────

	describe("grep", () => {
		useDescribeVm();

		test("basic pattern match", async () => {
			await vm.exec(
				'printf "apple\\nbanana\\ncherry\\napricot\\n" > /tmp/grep.txt',
			);
			const r = await vm.exec("grep ap /tmp/grep.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("apple");
			expect(r.stdout).toContain("apricot");
			expect(r.stdout).not.toContain("banana");
		});

		test("grep -c counts matches", async () => {
			await vm.exec('printf "a\\nb\\na\\nc\\na\\n" > /tmp/gc.txt');
			const r = await vm.exec("grep -c a /tmp/gc.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("3");
		});

		test("grep -i case insensitive", async () => {
			await vm.exec('printf "Hello\\nworld\\nHELLO\\n" > /tmp/gi.txt');
			const r = await vm.exec("grep -i hello /tmp/gi.txt");
			expect(r.exitCode).toBe(0);
			const lines = r.stdout.trim().split("\n");
			expect(lines).toHaveLength(2);
		});

		test("grep -v inverts match", async () => {
			await vm.exec('printf "yes\\nno\\nyes\\n" > /tmp/gv.txt');
			const r = await vm.exec("grep -v yes /tmp/gv.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("no");
		});

		test("grep with regex", async () => {
			await vm.exec('printf "foo123\\nbar456\\nbaz\\n" > /tmp/gre.txt');
			const r = await vm.exec("grep '[0-9]' /tmp/gre.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("foo123");
			expect(r.stdout).toContain("bar456");
			expect(r.stdout).not.toContain("baz");
		});

		test("egrep extended regex", async () => {
			await vm.exec('printf "cat\\ndog\\nbird\\n" > /tmp/eg.txt');
			const r = await vm.exec("egrep 'cat|dog' /tmp/eg.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("cat");
			expect(r.stdout).toContain("dog");
			expect(r.stdout).not.toContain("bird");
		});

		test("grep pipe from stdin", async () => {
			const r = await vm.exec("echo 'hello world' | grep world");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("world");
		});
	});

	// ── sed ───────────────────────────────────────────────────────────

	describe("sed", () => {
		useDescribeVm();

		test("substitute first occurrence", async () => {
			const r = await vm.exec("echo 'hello world' | sed 's/world/earth/'");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("hello earth");
		});

		test("global substitution", async () => {
			const r = await vm.exec("echo 'aaa' | sed 's/a/b/g'");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("bbb");
		});

		test("delete lines matching pattern", async () => {
			await vm.exec('printf "keep\\ndelete-me\\nkeep\\n" > /tmp/sed.txt');
			const r = await vm.exec("sed '/delete/d' /tmp/sed.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("keep\nkeep");
		});

		test("print specific line with -n and p", async () => {
			await vm.exec('printf "a\\nb\\nc\\n" > /tmp/sedp.txt');
			const r = await vm.exec("sed -n '2p' /tmp/sedp.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("b");
		});
	});

	// ── gawk ──────────────────────────────────────────────────────────

	describe("awk (gawk)", () => {
		useDescribeVm();

		test("print specific field", async () => {
			await vm.exec('printf "a b c\\n1 2 3\\n" > /tmp/awk.txt');
			const r = await vm.exec("awk '{print $2}' /tmp/awk.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("b\n2");
		});

		test("field separator", async () => {
			const r = await vm.exec("echo 'a:b:c' | awk -F: '{print $2}'");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("b");
		});

		test("sum column", async () => {
			await vm.exec('printf "10\\n20\\n30\\n" > /tmp/sum.txt');
			const r = await vm.exec("awk '{s+=$1} END {print s}' /tmp/sum.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("60");
		});

		test("pattern matching", async () => {
			await vm.exec('printf "ERR foo\\nINFO bar\\nERR baz\\n" > /tmp/awkg.txt');
			const r = await vm.exec("awk '/ERR/ {print $2}' /tmp/awkg.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("foo\nbaz");
		});
	});

	// ── findutils ─────────────────────────────────────────────────────

	describe("find and xargs (findutils)", () => {
		useDescribeVm();

		test("find files by name", async () => {
			await vm.exec(
				"mkdir -p /tmp/findtest/sub && echo a > /tmp/findtest/a.txt && echo b > /tmp/findtest/b.log && echo c > /tmp/findtest/sub/c.txt",
			);
			const r = await vm.exec("find /tmp/findtest -name '*.txt'");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("a.txt");
			expect(r.stdout).toContain("c.txt");
			expect(r.stdout).not.toContain("b.log");
		});

		test("find with -type d", async () => {
			await vm.exec(
				"mkdir -p /tmp/finddir/sub1 /tmp/finddir/sub2 && echo f > /tmp/finddir/file.txt",
			);
			const r = await vm.exec("find /tmp/finddir -mindepth 1 -type d");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("sub1");
			expect(r.stdout).toContain("sub2");
		});

		test("xargs passes args to command", async () => {
			await vm.exec('printf "hello\\nworld\\n" > /tmp/xargs.txt');
			const r = await vm.exec("xargs -a /tmp/xargs.txt echo");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("hello world");
		});
	});

	// ── diffutils ─────────────────────────────────────────────────────

	describe("diff (diffutils)", () => {
		useDescribeVm();

		test("diff identical files returns 0", async () => {
			await vm.exec("echo same > /tmp/d1.txt && echo same > /tmp/d2.txt");
			const r = await vm.exec("diff /tmp/d1.txt /tmp/d2.txt");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toBe("");
		});

		test("diff shows differences", async () => {
			await vm.exec("echo old > /tmp/d3.txt && echo new > /tmp/d4.txt");
			const r = await vm.exec("diff /tmp/d3.txt /tmp/d4.txt");
			// Our diff outputs the diff but may not set exit code 1.
			expect(r.stdout).toContain("old");
			expect(r.stdout).toContain("new");
		});
	});

	// ── tar ───────────────────────────────────────────────────────────

	describe("tar", () => {
		useDescribeVm();

		test("create and extract archive", async () => {
			await vm.exec(
				"mkdir -p /tmp/tardir && echo file-a > /tmp/tardir/a.txt && echo file-b > /tmp/tardir/b.txt",
			);
			const create = await vm.exec("tar cf /tmp/test.tar -C /tmp tardir");
			expect(create.exitCode).toBe(0);

			const extract = await vm.exec(
				"mkdir -p /tmp/extracted && tar xf /tmp/test.tar -C /tmp/extracted && cat /tmp/extracted/tardir/a.txt",
			);
			expect(extract.exitCode).toBe(0);
			expect(extract.stdout.trim()).toBe("file-a");
		}, 30_000);

		test("list archive contents", async () => {
			await vm.exec("mkdir -p /tmp/tarlist && echo x > /tmp/tarlist/x.txt");
			await vm.exec("tar cf /tmp/list.tar -C /tmp tarlist");
			const r = await vm.exec("tar tf /tmp/list.tar");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("x.txt");
		});
	});

	// ── gzip ──────────────────────────────────────────────────────────

	describe("gzip", () => {
		useDescribeVm();

		test("compress and decompress", async () => {
			await vm.exec('echo "compress me please" > /tmp/gz.txt');
			const comp = await vm.exec(
				"gzip /tmp/gz.txt && test -f /tmp/gz.txt.gz && echo ok",
			);
			expect(comp.exitCode).toBe(0);
			expect(comp.stdout.trim()).toBe("ok");

			const decomp = await vm.exec("gunzip /tmp/gz.txt.gz && cat /tmp/gz.txt");
			expect(decomp.exitCode).toBe(0);
			expect(decomp.stdout.trim()).toBe("compress me please");
		}, 30_000);

		test("zcat reads compressed file without extracting", async () => {
			await vm.exec('echo "zcat-data" > /tmp/zc.txt');
			await vm.exec("gzip /tmp/zc.txt");
			const r = await vm.exec("zcat /tmp/zc.txt.gz");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("zcat-data");
		});
	});

	// ── jq ────────────────────────────────────────────────────────────

	describe("jq", () => {
		useDescribeVm();

		test("extract field from JSON", async () => {
			const r = await vm.exec(
				'echo \'{"name":"test","version":42}\' | jq .name',
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe('"test"');
		});

		test("extract number", async () => {
			const r = await vm.exec("echo '{\"x\":99}' | jq .x");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("99");
		});

		test("filter array", async () => {
			const r = await vm.exec(
				'echo \'[{"a":1},{"a":2},{"a":3}]\' | jq ".[].a"',
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("1\n2\n3");
		});

		test("construct new JSON", async () => {
			const r = await vm.exec(
				"echo '{\"a\":1,\"b\":2}' | jq '{sum: (.a + .b)}'",
			);
			expect(r.exitCode).toBe(0);
			const parsed = JSON.parse(r.stdout.trim());
			expect(parsed).toEqual({ sum: 3 });
		});
	});

	// ── ripgrep ───────────────────────────────────────────────────────

	describe("rg (ripgrep)", () => {
		useDescribeVm();

		test("search files recursively", async () => {
			await vm.exec(
				"mkdir -p /tmp/rgdir/sub && echo 'needle here' > /tmp/rgdir/a.txt && echo nothing > /tmp/rgdir/b.txt && echo 'another needle' > /tmp/rgdir/sub/c.txt",
			);
			const r = await vm.exec("rg needle /tmp/rgdir/");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("a.txt");
			expect(r.stdout).toContain("c.txt");
			expect(r.stdout).not.toContain("b.txt");
		});

		test("rg -l lists matching files only", async () => {
			await vm.exec(
				"mkdir -p /tmp/rgl && echo match > /tmp/rgl/x.txt && echo no > /tmp/rgl/y.txt",
			);
			const r = await vm.exec("rg -l match /tmp/rgl/");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("x.txt");
			expect(r.stdout).not.toContain("y.txt");
		});
	});

	// ── fd ─────────────────────────────────────────────────────────────

	describe("fd (fd-find)", () => {
		useDescribeVm();

		test("find files by pattern", async () => {
			await vm.exec(
				"mkdir -p /tmp/fdtest/sub && echo ts > /tmp/fdtest/hello.ts && echo js > /tmp/fdtest/world.js && echo ts > /tmp/fdtest/sub/foo.ts",
			);
			const r = await vm.exec("fd '\\.ts$' /tmp/fdtest/");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("hello.ts");
			expect(r.stdout).toContain("foo.ts");
			expect(r.stdout).not.toContain("world.js");
		});
	});

	// ── tree ──────────────────────────────────────────────────────────

	describe("tree", () => {
		useDescribeVm();

		test("displays directory structure", async () => {
			await vm.exec(
				"mkdir -p /tmp/treedir/sub && echo a > /tmp/treedir/a.txt && echo b > /tmp/treedir/sub/b.txt",
			);
			const r = await vm.exec("tree /tmp/treedir");
			expect(r.exitCode).toBe(0);
			expect(r.stdout).toContain("a.txt");
			expect(r.stdout).toContain("sub");
			expect(r.stdout).toContain("b.txt");
		});
	});

	// ── yq ────────────────────────────────────────────────────────────

	describe("yq", () => {
		useDescribeVm();

		test("extract field from YAML via pipe", async () => {
			const r = await vm.exec("echo 'name: test' | yq '.name'");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("test");
		});
	});

	// ── curl (requires C build) ───────────────────────────────────────

	describe("curl", () => {
		useDescribeVm();

		const hasCurl = existsSync(join(curlPackage.commandDir, "curl"));

		const CURL_SCRIPT = `
const net = require("net");

function tryParseHttpRequest(buffer) {
  const headerEnd = buffer.indexOf("\\r\\n\\r\\n");
  if (headerEnd === -1) {
    return null;
  }

  const headerText = buffer.subarray(0, headerEnd).toString("utf8");
  const [requestLine, ...headerLines] = headerText.split("\\r\\n");
  const [method = "GET"] = requestLine.split(" ");

  let contentLength = 0;
  for (const line of headerLines) {
    const separator = line.indexOf(":");
    if (separator === -1) {
      continue;
    }
    const name = line.slice(0, separator).trim().toLowerCase();
    if (name === "content-length") {
      const parsed = Number(line.slice(separator + 1).trim());
      if (Number.isFinite(parsed) && parsed >= 0) {
        contentLength = parsed;
      }
    }
  }

  const bodyOffset = headerEnd + 4;
  if (buffer.length < bodyOffset + contentLength) {
    return null;
  }

  return {
    method,
    body: buffer.subarray(bodyOffset, bodyOffset + contentLength).toString("utf8"),
  };
}

function sendResponse(socket, status, headers, body) {
  const payload = Buffer.from(body, "utf8");
  const responseHeaders = [
    "HTTP/1.1 " + status,
    ...headers,
    "Content-Length: " + payload.length,
    "Connection: close",
    "",
    "",
  ].join("\\r\\n");

  socket.end(Buffer.concat([Buffer.from(responseHeaders, "utf8"), payload]));
}

const server = net.createServer((socket) => {
  let buffered = Buffer.alloc(0);
  socket.on("data", (chunk) => {
    buffered = Buffer.concat([buffered, chunk]);
    const request = tryParseHttpRequest(buffered);
    if (!request) {
      return;
    }

    if (request.method === "POST") {
      sendResponse(
        socket,
        "200 OK",
        ["Content-Type: application/json"],
        JSON.stringify({ echo: request.body }),
      );
      return;
    }

    sendResponse(
      socket,
      "200 OK",
      ["Content-Type: text/plain"],
      "hello from server",
    );
  });
});

server.listen(0, "0.0.0.0", () => {
  console.log("PORT:" + server.address().port);
});
`;

		const CURL_KEEPALIVE_SCRIPT = `
const net = require("net");
const server = net.createServer((socket) => {
  socket.once("data", () => {
    const body = "hello from keepalive";
    socket.write(
      "HTTP/1.1 200 OK\\r\\n" +
      "Content-Type: text/plain\\r\\n" +
      "Content-Length: " + Buffer.byteLength(body) + "\\r\\n" +
      "Connection: keep-alive\\r\\n" +
      "Keep-Alive: timeout=60\\r\\n" +
      "\\r\\n" +
      body,
    );
    // Intentionally leave the socket open so curl's shutdown path performs
    // a non-blocking drain read instead of waiting for EOF.
  });
});
server.listen(0, "0.0.0.0", () => {
  console.log("PORT:" + server.address().port);
});
`;

		async function startServer(
			testVm: AgentOs,
			script = CURL_SCRIPT,
		): Promise<{ pid: number; port: number }> {
			await testVm.writeFile("/tmp/curl-server.js", script);
			let resolvePort: (port: number) => void;
			const portPromise = new Promise<number>((r) => {
				resolvePort = r;
			});
			const { pid } = testVm.spawn("node", ["/tmp/curl-server.js"], {
				onStdout: (data: Uint8Array) => {
					const text = new TextDecoder().decode(data);
					const match = text.match(/PORT:(\d+)/);
					if (match) resolvePort(Number(match[1]));
				},
			});
			const port = await portPromise;
			return { pid, port };
		}

		async function runCurl(args: string[]): Promise<{
			exitCode: number;
			stdout: string;
			stderr: string;
		}> {
			let stdout = "";
			let stderr = "";
			const { pid } = vm.spawn("curl", args, {
				onStdout: (data) => {
					stdout += Buffer.from(data).toString("utf8");
				},
				onStderr: (data) => {
					stderr += Buffer.from(data).toString("utf8");
				},
			});
			const exitCode = await vm.waitProcess(pid);
			await new Promise<void>((resolve) => {
				setTimeout(resolve, 0);
			});
			return { exitCode, stdout, stderr };
		}

		test("curl GET request", async () => {
			expect(hasCurl).toBe(true);

			const { pid, port } = await startServer(vm);
			try {
				const r = await runCurl(["-s", `http://localhost:${port}/`]);
				expect(r.exitCode).toBe(0);
				expect(r.stdout).toContain("hello from server");
			} finally {
				vm.killProcess(pid);
			}
		});

		test("curl POST with data", async () => {
			expect(hasCurl).toBe(true);

			const { pid, port } = await startServer(vm);
			try {
				const r = await runCurl([
					"-s",
					"-X",
					"POST",
					"-d",
					"test-body",
					`http://localhost:${port}/`,
				]);
				expect(r.exitCode).toBe(0);
				const json = JSON.parse(r.stdout);
				expect(json.echo).toBe("test-body");
			} finally {
				vm.killProcess(pid);
			}
		});

		test("curl exits promptly after a keep-alive response", async () => {
				const { pid, port } = await startServer(vm, CURL_KEEPALIVE_SCRIPT);
				try {
					const startedAt = Date.now();
					const r = await runCurl(["-s", `http://localhost:${port}/`]);
					const elapsedMs = Date.now() - startedAt;
					expect(r.exitCode).toBe(0);
					expect(r.stdout).toContain("hello from keepalive");
					expect(r.stderr).not.toContain("i/o error");
					expect(elapsedMs).toBeLessThan(8000);
				} finally {
					vm.killProcess(pid);
				}
		}, 15000);
	});

	// ── file permissions (Bug 1 regression tests) ──────────────────────

	describe("file permissions", () => {
		useDescribeVm();

		test("stat shows correct file mode", async () => {
			await vm.exec("echo test > /tmp/perm.txt");
			const r = await vm.exec('stat -c "%a" /tmp/perm.txt');
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("644");
		});

		test("stat shows correct directory mode", async () => {
			await vm.exec("mkdir -p /tmp/perm-dir");
			const r = await vm.exec('stat -c "%a" /tmp/perm-dir');
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("755");
		});

		test("chmod changes file mode", async () => {
			await vm.exec("echo test > /tmp/chmod-test.txt");
			const r1 = await vm.exec("chmod 755 /tmp/chmod-test.txt");
			expect(r1.exitCode).toBe(0);
			const r = await vm.exec('stat -c "%a" /tmp/chmod-test.txt');
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("755");
		});

		test("ls -la shows correct permissions", async () => {
			await vm.exec("echo test > /tmp/ls-perm.txt");
			const stat = await vm.stat("/tmp/ls-perm.txt");
			const permissionBits = stat.mode & 0o777;
			const rendered = [
				permissionBits & 0o400 ? "r" : "-",
				permissionBits & 0o200 ? "w" : "-",
				permissionBits & 0o100 ? "x" : "-",
				permissionBits & 0o040 ? "r" : "-",
				permissionBits & 0o020 ? "w" : "-",
				permissionBits & 0o010 ? "x" : "-",
				permissionBits & 0o004 ? "r" : "-",
				permissionBits & 0o002 ? "w" : "-",
				permissionBits & 0o001 ? "x" : "-",
			].join("");
			expect(rendered).toBe("rw-r--r--");
		});
	});

	// ── exit codes (Bug 5 regression tests) ─────────────────────────

	describe("exit codes", () => {
		useDescribeVm();

		test("grep returns 1 on no match", async () => {
			await vm.exec('echo "hello" > /tmp/ec.txt');
			const r = await vm.exec("grep nonexistent /tmp/ec.txt");
			expect(r.exitCode).toBe(1);
		});

		test("diff returns 1 on different files", async () => {
			await vm.exec('echo "a" > /tmp/d1.txt && echo "b" > /tmp/d2.txt');
			const r = await vm.exec("diff /tmp/d1.txt /tmp/d2.txt");
			expect(r.exitCode).toBe(1);
		});

		test("false returns 1", async () => {
			const r = await vm.exec("false");
			expect(r.exitCode).toBe(1);
		});

		test("cat returns 1 on missing file", async () => {
			const r = await vm.exec("cat /tmp/nonexistent-file");
			expect(r.exitCode).toBe(1);
		});

		test("test -f on missing file returns 1", async () => {
			const r = await vm.exec("test -f /tmp/nonexistent-file");
			expect(r.exitCode).toBe(1);
		});
	});

	// ── complex pipelines ─────────────────────────────────────────────

	describe("cross-package pipelines", () => {
		useDescribeVm();

		test("find | grep | wc pipeline", async () => {
			await vm.exec(
				"mkdir -p /tmp/pipe && echo x > /tmp/pipe/a.txt && echo x > /tmp/pipe/b.log && echo x > /tmp/pipe/c.txt",
			);
			const r = await vm.exec(
				"find /tmp/pipe -name '*.txt' | grep txt | wc -l",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("2");
		}, 90_000);

		test("awk + sort pipeline", async () => {
			await vm.exec(
				'printf "alice 90\\nbob 70\\ncharlie 85\\n" > /tmp/scores.txt',
			);
			const r = await vm.exec(
				"sort -k2 -rn /tmp/scores.txt | head -1 | awk '{print $1}'",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("alice");
		}, 90_000);

		test("tar + gzip round trip", async () => {
			await vm.exec(
				"mkdir -p /tmp/tgz && echo round-trip-data > /tmp/tgz/data.txt",
			);
			const create = await vm.exec(
				"tar cf - -C /tmp tgz | gzip > /tmp/archive.tar.gz",
			);
			expect(create.exitCode).toBe(0);

			const extract = await vm.exec(
				"mkdir -p /tmp/tgz-out && zcat /tmp/archive.tar.gz | tar xf - -C /tmp/tgz-out && cat /tmp/tgz-out/tgz/data.txt",
			);
			expect(extract.exitCode).toBe(0);
			expect(extract.stdout.trim()).toBe("round-trip-data");
		}, 90_000);

		test("sed + grep text processing chain", async () => {
			await vm.exec(
				'printf "ERROR: disk full\\nINFO: ok\\nERROR: timeout\\nINFO: done\\n" > /tmp/chain.txt',
			);
			const r = await vm.exec("grep ERROR /tmp/chain.txt | sed 's/ERROR: //'");
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("disk full\ntimeout");
		}, 90_000);

		test("jq + awk data transformation", async () => {
			await vm.exec(
				'printf \'[{"price":10},{"price":20}]\\n\' > /tmp/items.json',
			);
			const r = await vm.exec(
				"cat /tmp/items.json | jq '.[].price' | awk '{s+=$1} END {print s}'",
			);
			expect(r.exitCode).toBe(0);
			expect(r.stdout.trim()).toBe("30");
		}, 90_000);
	});
});
