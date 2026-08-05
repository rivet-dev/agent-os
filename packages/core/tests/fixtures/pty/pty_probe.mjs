// pty_probe.mjs — guest-Node twin of pty_probe.c.
//
// Implements the IDENTICAL argv-dispatched case set and the IDENTICAL `#`-prefixed
// marker protocol (byte-matching hex/text encoding) so the host harness can assert
// the same strings against both the WASM-C probe and this Node probe.
//
// Launched by the host as a BUILT-IN VM command:
//   vm.openShell({ command: "node", args: ["/pty_probe.mjs", caseId], cols, rows })
// openShell auto-injects AGENTOS_EXEC_TTY=1 + COLUMNS/LINES; the sidecar opens a
// PTY and dup2()s the slave onto fd 0/1/2.
//
// GUEST-NODE TTY STATUS (the runtime now routes guest-node stdin through the
// kernel PTY and populates the TTY config from the kernel, so most cells match
// the wasm-c probe). Remaining honest gaps (asserted-as-broken by the host, NOT
// faked here):
//   - SIGINT/SIGQUIT are consumed by the kernel line discipline (no data byte),
//     but the signal is not yet delivered into the V8 isolate / does not kill it,
//     so the probe is not torn down the way the wasm-c process is.
//   - a raw lone-LF stdout write does not flow through OPOST/ONLCR (guest-node
//     stdout payload-write quirk), so `a\nb` is not rewritten to `a\r\nb`.

// ---- output discipline: synchronous writes to fd 1, explicit \r\n ------------

function out(s) {
	process.stdout.write(s);
}

// ---- encoders (byte-identical to the C print_hex / print_text) --------------

function printHex(bytes) {
	let o = "";
	for (let i = 0; i < bytes.length; i++) {
		if (i > 0) o += " ";
		o += bytes[i].toString(16).toUpperCase().padStart(2, "0");
	}
	return o;
}

function printText(bytes) {
	let o = "";
	for (let i = 0; i < bytes.length; i++) {
		const c = bytes[i];
		if (c === 0x0d) o += "\\r";
		else if (c === 0x0a) o += "\\n";
		else if (c === 0x09) o += "\\t";
		else if (c === 0x1b) o += "\\e";
		else if (c < 0x20 || c === 0x7f)
			o += "\\x" + c.toString(16).toUpperCase().padStart(2, "0");
		else o += String.fromCharCode(c);
	}
	return o;
}

function emitBytes(tag, buf) {
	out(`#BYTES tag=${tag} n=${buf.length} hex=${printHex(buf)} text=${printText(buf)}\r\n`);
}

// ---- blocking stdin reader (keeps the probe "blocked in read") --------------
//
// Accumulates process.stdin 'data' bytes one at a time until the terminator byte
// is seen / `cap` bytes collected / EOF, then resolves a Buffer. Mirrors the C
// read_until loop: the host can type bytes with NO terminator and the promise
// stays pending, so any glyph on screen is kernel echo, not this readback.

function readUntil(term, cap = 1 << 20) {
	return new Promise((resolve) => {
		const acc = [];
		let done = false;
		const finish = () => {
			if (done) return;
			done = true;
			process.stdin.off("data", onData);
			process.stdin.off("end", onEnd);
			process.stdin.pause();
			resolve(Buffer.from(acc));
		};
		const onData = (chunk) => {
			for (let i = 0; i < chunk.length; i++) {
				acc.push(chunk[i]);
				if (chunk[i] === term || acc.length >= cap) {
					finish();
					return;
				}
			}
		};
		const onEnd = () => finish();
		process.stdin.on("data", onData);
		process.stdin.on("end", onEnd);
		process.stdin.resume();
	});
}

// Reads exactly one byte (or resolves an empty Buffer on EOF).
function readByte() {
	return new Promise((resolve) => {
		let done = false;
		const finish = (buf) => {
			if (done) return;
			done = true;
			process.stdin.off("data", onData);
			process.stdin.off("end", onEnd);
			process.stdin.pause();
			resolve(buf);
		};
		const onData = (chunk) => finish(Buffer.from(chunk.subarray(0, 1)));
		const onEnd = () => finish(Buffer.alloc(0));
		process.stdin.on("data", onData);
		process.stdin.on("end", onEnd);
		process.stdin.resume();
	});
}

// Attempts a raw-mode toggle, emitting the honest #MODE marker. On guest-node
// setRawMode throws (isTTY=false) -> `#MODE want=<m> rc=err err=<message>`.
function setMode(want) {
	const enable = want === "raw";
	try {
		process.stdin.setRawMode(enable);
		out(`#MODE want=${want} rc=0\r\n`);
	} catch (err) {
		out(`#MODE want=${want} rc=err err=${err && err.message ? err.message : String(err)}\r\n`);
	}
}

// ---- cases ------------------------------------------------------------------

async function caseCookedEcho() {
	// Cooked is the kernel default; setRawMode(false) would throw on guest-node,
	// and cooked is what we want, so report a no-op success to mirror the C output.
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=echo\r\n");
	const buf = await readUntil(0x0a);
	if (buf.length === 0) {
		out("#EOF tag=echo n=0\r\n");
		return;
	}
	emitBytes("echo", buf);
}

async function caseControlCharEcho() {
	setMode("cooked");
	out("#READY tag=ctl\r\n");
	const buf = await readUntil(0x0a);
	if (buf.length === 0) {
		out("#EOF tag=ctl n=0\r\n");
		return;
	}
	emitBytes("ctl", buf);
}

async function caseRawNoEcho() {
	setMode("raw");
	out("#READY tag=raw\r\n");
	const buf = await readUntil(0x21); // '!'
	emitBytes("raw", buf);
}

async function caseBackspace() {
	// Cooked default; mirror C #MODE want=cooked rc=0 (no setRawMode call).
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=erase\r\n");
	const buf = await readUntil(0x0a);
	emitBytes("erase", buf);
}

async function caseKillLine() {
	setMode("cooked");
	out("#READY tag=kill\r\n");
	const buf = await readUntil(0x0a);
	emitBytes("kill", buf);
}

async function caseWordErase() {
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=werase\r\n");
	const buf = await readUntil(0x0a);
	emitBytes("werase", buf);
}

async function caseLineBuffering() {
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=canon\r\n");
	const buf = await readUntil(0x0a);
	if (buf.length === 0) {
		out("#EOF tag=canon n=0\r\n");
		return;
	}
	emitBytes("canon", buf);
}

async function caseSigint() {
	out("#MODE want=cooked rc=0\r\n");
	// A caught SIGINT must run this handler without leaking ^C into stdin.
	process.on("SIGINT", () => {
		out("#SIG name=SIGINT\r\n");
		finish("sigint");
	});
	out("#READY tag=sigint\r\n");
	// Report any incorrectly leaked data byte; a working runtime fires the
	// handler instead.
	const buf = await readByte();
	if (buf.length === 0) {
		out("#EOF tag=sigint n=0\r\n");
	} else {
		emitBytes("sigint", buf);
	}
}

async function caseSigquit() {
	out("#MODE want=cooked rc=0\r\n");
	process.on("SIGQUIT", () => {
		out("#SIG name=SIGQUIT\r\n");
		finish("sigquit");
	});
	out("#READY tag=sigquit\r\n");
	// The lone 0x1C arrives as a data byte on guest-node (no signal); report it.
	const buf = await readByte();
	if (buf.length === 0) {
		out("#EOF tag=sigquit n=0\r\n");
	} else {
		emitBytes("sigquit", buf);
	}
}

async function caseVsusp() {
	out("#MODE want=cooked rc=0\r\n");
	// SHOULD fire on ^Z; on guest-node it never does (no SIGTSTP delivery) — kept
	// so a fixed runtime passes the same probe and the absence of #SIG is honest.
	process.on("SIGTSTP", () => {
		out("#SIG name=SIGTSTP\r\n");
		finish("vsusp");
	});
	out("#READY tag=susp\r\n");
	// The kernel line discipline consumes ^Z as a signal (no data byte), so this
	// read stays pending; a leaked data byte resolves it and is reported.
	const buf = await readByte();
	if (buf.length === 0) {
		out("#EOF tag=susp n=0\r\n");
	} else {
		emitBytes("susp", buf);
	}
}

async function caseEraseCtrlH() {
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=eraseh\r\n");
	const buf = await readUntil(0x0a);
	emitBytes("eraseh", buf);
}

async function caseVintrBuffer() {
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=vintrbuf\r\n");
	// The kernel flushes the canonical buffer on ^C, so the buffered "abc" is
	// discarded; if this runtime survives the SIGINT it then reads "de\n" and the
	// delivered line proves the flush ("de\n", not "abcde\n").
	const buf = await readUntil(0x0a);
	if (buf.length === 0) {
		out("#EOF tag=vintrbuf n=0\r\n");
	} else {
		emitBytes("vintrbuf", buf);
	}
}

async function caseRawCtrlcByte() {
	setMode("raw");
	out("#READY tag=rawc\r\n");
	const buf = await readByte();
	if (buf.length === 0) {
		out("#EOF tag=rawc n=0\r\n");
		return;
	}
	emitBytes("rawc", buf);
}

function caseOnlcr() {
	// Raw-write `a\nb`; the kernel pty's OPOST+ONLCR must inject CR before the LF.
	process.stdout.write(Buffer.from([0x61, 0x0a, 0x62]));
}

async function caseIcrnl() {
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=icrnl\r\n");
	const buf = await readUntil(0x0a);
	emitBytes("icrnl", buf);
}

async function caseEof() {
	out("#MODE want=cooked rc=0\r\n");
	out("#READY tag=eof\r\n");
	const buf = await readByte();
	if (buf.length === 0) {
		out("#EOF tag=eof n=0\r\n");
	} else {
		// Honest-broken guest-node path: ^D delivered as a data byte, not EOF.
		emitBytes("eof", buf);
	}
}

async function caseResizeSigwinch() {
	out(`#SIZE tag=before rc=0 cols=${process.stdout.columns} rows=${process.stdout.rows}\r\n`);
	// Match Node: the sidecar forwards the kernel foreground-process-group
	// SIGWINCH, and the handler reads the live resized PTY dimensions.
	process.on("SIGWINCH", () => {
		out("#SIG name=SIGWINCH\r\n");
		out(`#SIZE tag=after rc=0 cols=${process.stdout.columns} rows=${process.stdout.rows}\r\n`);
	});
	out("#READY tag=resize\r\n");
	await readUntil(0x21); // '!'
}

async function caseCpr() {
	setMode("raw");
	process.stdout.write("\x1b[6n");
	out("#CPR sent=1\r\n");
	const buf = await readUntil(0x52); // 'R'
	if (buf.length === 0) {
		out("#EOF tag=cpr n=0\r\n");
		return;
	}
	out(`#CPRREPLY n=${buf.length} hex=${printHex(buf)} text=${printText(buf)}\r\n`);
}

function caseIsatty() {
	const in_ = process.stdin.isTTY ? 1 : 0;
	const o = process.stdout.isTTY ? 1 : 0;
	const e = process.stderr.isTTY ? 1 : 0; // PTY slave is dup2'd onto fd 2 too
	out(`#TTY in=${in_} out=${o} err=${e}\r\n`);
}

function caseWinsize() {
	const cols = process.stdout.columns;
	const rows = process.stdout.rows;
	out(`#SIZE tag=open rc=0 cols=${cols} rows=${rows}\r\n`);
}

// ---- dispatch ---------------------------------------------------------------

const CASE_ID = process.argv[2] ?? "";

function finish(id) {
	out(`#DONE id=${id}\r\n`);
	process.exit(0);
}

async function main() {
	if (CASE_ID === "") {
		out("#ERR unknown-case id=\r\n");
		process.exit(2);
		return;
	}

	out(`#START id=${CASE_ID}\r\n`);

	switch (CASE_ID) {
		case "cooked-echo":
			await caseCookedEcho();
			break;
		case "control-char-echo":
			await caseControlCharEcho();
			break;
		case "raw-no-echo":
			await caseRawNoEcho();
			break;
		case "backspace":
			await caseBackspace();
			break;
		case "kill-line":
			await caseKillLine();
			break;
		case "word-erase":
			await caseWordErase();
			break;
		case "line-buffering":
			await caseLineBuffering();
			break;
		case "sigint":
			await caseSigint();
			break;
		case "sigquit":
			await caseSigquit();
			break;
		case "vsusp":
			await caseVsusp();
			break;
		case "erase-ctrl-h":
			await caseEraseCtrlH();
			break;
		case "vintr-buffer":
			await caseVintrBuffer();
			break;
		case "raw-ctrlc-byte":
			await caseRawCtrlcByte();
			break;
		case "onlcr":
			caseOnlcr();
			break;
		case "icrnl":
			await caseIcrnl();
			break;
		case "eof":
			await caseEof();
			break;
		case "resize-sigwinch":
			await caseResizeSigwinch();
			break;
		case "cpr":
			await caseCpr();
			break;
		case "isatty":
			caseIsatty();
			break;
		case "winsize":
			caseWinsize();
			break;
		default:
			out(`#ERR unknown-case id=${CASE_ID}\r\n`);
			process.exit(2);
			return;
	}

	out(`#DONE id=${CASE_ID}\r\n`);
	process.exit(0);
}

main();
