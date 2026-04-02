/**
 * Integration tests for pngcrush WASM command.
 *
 * Verifies pngcrush can optimize a PNG file, produces valid output,
 * and shows version/help info using the AgentOs API with real WASM binaries.
 */

import { existsSync } from "node:fs";
import { deflateSync } from "node:zlib";
import { describe, it, expect, afterEach, beforeEach } from "vitest";
import { AgentOs } from "@rivet-dev/agent-os-core";
import coreutils from "@rivet-dev/agent-os-coreutils";
import pngcrush from "../../software/pngcrush/dist/index.js";

const hasCoreutils = existsSync(coreutils.commandDir);
const hasPngcrush = existsSync(pngcrush.commandDir);

function skipReason(): string | false {
  if (!hasCoreutils) return "coreutils WASM binaries not available";
  if (!hasPngcrush) return "pngcrush WASM binary not available";
  return false;
}

/**
 * Generate a minimal valid 1x1 red PNG using Node's zlib for correct compression.
 */
function createMinimalPng(): Uint8Array {
  const buf: number[] = [];

  // PNG signature
  buf.push(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a);

  // IHDR chunk: 1x1, 8-bit RGB
  const ihdrData = [
    0x00, 0x00, 0x00, 0x01, // width = 1
    0x00, 0x00, 0x00, 0x01, // height = 1
    0x08,                   // bit depth = 8
    0x02,                   // color type = RGB
    0x00,                   // compression = deflate
    0x00,                   // filter = adaptive
    0x00,                   // interlace = none
  ];
  writeChunk(buf, "IHDR", ihdrData);

  // IDAT chunk: zlib-compressed scanline (filter byte 0 + RGB pixel)
  const rawScanline = Buffer.from([0x00, 0xff, 0x00, 0x00]); // filter=none, R=255, G=0, B=0
  const compressed = deflateSync(rawScanline);
  writeChunk(buf, "IDAT", [...compressed]);

  // IEND chunk
  writeChunk(buf, "IEND", []);

  return new Uint8Array(buf);
}

function writeChunk(buf: number[], type: string, data: number[]) {
  const len = data.length;
  buf.push((len >> 24) & 0xff, (len >> 16) & 0xff, (len >> 8) & 0xff, len & 0xff);
  for (let i = 0; i < 4; i++) buf.push(type.charCodeAt(i));
  buf.push(...data);
  const crcInput = new Uint8Array(4 + data.length);
  for (let i = 0; i < 4; i++) crcInput[i] = type.charCodeAt(i);
  crcInput.set(data, 4);
  const crc = crc32(crcInput);
  buf.push((crc >> 24) & 0xff, (crc >> 16) & 0xff, (crc >> 8) & 0xff, crc & 0xff);
}

function crc32(data: Uint8Array): number {
  let crc = 0xffffffff;
  for (let i = 0; i < data.length; i++) {
    crc ^= data[i];
    for (let j = 0; j < 8; j++) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

describe.skipIf(skipReason())("pngcrush command", () => {
  let vm: AgentOs;

  beforeEach(async () => {
    vm = await AgentOs.create({ software: [coreutils, pngcrush] });
  });

  afterEach(async () => {
    await vm.dispose();
  });

  it("prints version with -version flag", async () => {
    const result = await vm.exec("pngcrush -version");
    expect(result.exitCode).toBe(0);
    const output = result.stdout + result.stderr;
    expect(output).toContain("pngcrush 1.8");
  });

  it("optimizes a PNG file and produces valid output", async () => {
    const inputPng = createMinimalPng();
    await vm.writeFile("/tmp/input.png", inputPng);

    const result = await vm.exec("pngcrush -m 1 /tmp/input.png /tmp/output.png");
    expect(result.exitCode).toBe(0);

    // Verify output file was created and is a valid PNG
    expect(await vm.exists("/tmp/output.png")).toBe(true);
    const outputData = await vm.readFile("/tmp/output.png");
    expect(outputData.length).toBeGreaterThan(0);
    // PNG signature
    expect(outputData[0]).toBe(0x89);
    expect(outputData[1]).toBe(0x50); // P
    expect(outputData[2]).toBe(0x4e); // N
    expect(outputData[3]).toBe(0x47); // G
  });

  it("shows help with -h flag", async () => {
    const result = await vm.exec("pngcrush -h");
    const output = result.stdout + result.stderr;
    expect(output.toLowerCase()).toContain("usage");
  });
});
