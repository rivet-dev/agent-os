import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const generatedPath = path.resolve(
	scriptDir,
	"..",
	"..",
	"runtime-browser",
	"src",
	"generated",
	"node-polyfills.ts",
);

async function loadGeneratedPolyfill(name) {
	const source = await readFile(generatedPath, "utf8");
	const prefix = `export const BROWSER_${name}_POLYFILL_CODE = `;
	const line = source.split("\n").find((candidate) => candidate.startsWith(prefix));
	assert.ok(line, `missing generated ${name} polyfill`);
	const code = JSON.parse(line.slice(prefix.length, -1));
	const module = { exports: {} };
	new Function("module", "exports", code)(module, module.exports);
	return module.exports;
}

test("generated browser Node polyfills use upstream behavior", async () => {
	const [
		assertModule,
		events,
		querystring,
		stream,
		stringDecoder,
		url,
	] = await Promise.all([
		loadGeneratedPolyfill("ASSERT"),
		loadGeneratedPolyfill("EVENTS"),
		loadGeneratedPolyfill("QUERYSTRING"),
		loadGeneratedPolyfill("STREAM"),
		loadGeneratedPolyfill("STRING_DECODER"),
		loadGeneratedPolyfill("URL"),
	]);

	assertModule.deepStrictEqual(
		{ nested: new Set(["alpha"]) },
		{ nested: new Set(["alpha"]) },
	);

	const emitter = new events();
	const seen = [];
	emitter.once("value", (value) => seen.push(value));
	emitter.emit("value", "first");
	emitter.emit("value", "second");
	assert.deepEqual(seen, ["first"]);

	assert.deepEqual({ ...querystring.parse("a=hello+world&a=again") }, {
		a: ["hello world", "again"],
	});

	const passThrough = new stream.PassThrough();
	const chunks = [];
	passThrough.on("data", (chunk) => chunks.push(chunk.toString()));
	passThrough.end("streamed");
	assert.deepEqual(chunks, ["streamed"]);
	assert.equal(typeof stream.promises.pipeline, "function");
	assert.ok(stream.Readable.toWeb(new stream.PassThrough()) instanceof ReadableStream);

	const decoder = new stringDecoder.StringDecoder("utf8");
	const encoded = Buffer.from("Grüße");
	assert.equal(
		decoder.write(encoded.subarray(0, 4)) + decoder.end(encoded.subarray(4)),
		"Grüße",
	);

	for (const input of [
		"data:image/png;base64,SGVsbG8=",
		"mailto:user@example.com",
	]) {
		assert.equal(new url.URL(input).href, input);
	}
	assert.equal(url.resolve("https://example.com/a/", "../b"), "https://example.com/b");
	assert.equal(url.parse("https://example.com/a?b=c").pathname, "/a");
	assert.equal(url.fileURLToPath("file:///tmp/a%20b"), "/tmp/a b");
	assert.equal(url.pathToFileURL("/tmp/a b").href, "file:///tmp/a%20b");
	assert.equal(url.domainToASCII("mañana.com"), "xn--maana-pta.com");
});
