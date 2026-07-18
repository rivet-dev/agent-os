#!/usr/bin/env node

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, realpathSync, statSync } from 'node:fs';
import { basename, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const defaultCommandsDir = resolve(
  root,
  'toolchain/target/wasm32-wasip1/release/commands',
);
const defaultManifestPath = resolve(
  root,
  'crates/execution/assets/agentos-wasm-abi.json',
);
const expectedManifestSchemaVersion = 2;
const allowedImportStatuses = new Set(['canonical', 'compatibility']);

const args = process.argv.slice(2);
const printObserved = args.includes('--print-observed');
const printContract = args.includes('--print-contract');
const jsonOutput = args.includes('--json');

function option(name, fallback) {
  const index = args.indexOf(name);
  if (index === -1) return fallback;
  if (index + 1 >= args.length) throw new Error(`${name} requires a value`);
  return resolve(process.cwd(), args[index + 1]);
}

const commandsDir = option('--commands', defaultCommandsDir);
const manifestPath = option('--manifest', defaultManifestPath);

function signatureFromImportLine(line, command) {
  const header = line.match(/^\s*\(import\s+"([^"]+)"\s+"([^"]+)"\s+(.+)\)\s*$/);
  if (!header) return null;
  const [, module, name, declaration] = header;
  if (!declaration.startsWith('(func ')) {
    throw new Error(
      `${command}: non-function import ${module}.${name} is outside the AgentOS ABI`,
    );
  }

  const params = [...declaration.matchAll(/\(param(?:\s+\$[^\s)]+)?\s+([^)]+)\)/g)]
    .flatMap((match) => match[1].trim().split(/\s+/));
  const results = [...declaration.matchAll(/\(result\s+([^)]+)\)/g)]
    .flatMap((match) => match[1].trim().split(/\s+/));
  for (const type of [...params, ...results]) {
    if (!['i32', 'i64', 'f32', 'f64', 'v128', 'funcref', 'externref'].includes(type)) {
      throw new Error(`${command}: unsupported import value type ${type}`);
    }
  }
  return { module, name, params, results };
}

function inspectCommand(path, command) {
  const bytes = readFileSync(path);
  if (bytes.length < 8 || bytes.subarray(0, 4).toString('hex') !== '0061736d') {
    throw new Error(`${command}: expected a WebAssembly binary`);
  }
  const wat = execFileSync('wasm-dis', [path, '-o', '-'], {
    encoding: 'utf8',
    maxBuffer: 512 * 1024 * 1024,
  });
  const imports = wat
    .split('\n')
    .filter((line) => /^\s*\(import\s/.test(line))
    .map((line) => signatureFromImportLine(line, command));
  if (imports.some((entry) => entry === null)) {
    throw new Error(`${command}: failed to parse one or more WebAssembly imports`);
  }
  imports.sort((a, b) =>
    `${a.module}\0${a.name}`.localeCompare(`${b.module}\0${b.name}`),
  );
  return {
    command,
    target: basename(realpathSync(path)),
    sha256: createHash('sha256').update(bytes).digest('hex'),
    imports,
  };
}

function importKey(entry) {
  return `${entry.module}.${entry.name}`;
}

function signatureText(entry) {
  return `(${entry.params.join(',')}) -> (${entry.results.join(',')})`;
}

function collectCommands() {
  let entries;
  try {
    entries = readdirSync(commandsDir, { withFileTypes: true });
  } catch (error) {
    throw new Error(
      `canonical command directory is unavailable (${relative(root, commandsDir)}); run just tools-rebuild first: ${error.message}`,
    );
  }
  const commandNames = entries
    .filter((entry) => entry.isFile() || entry.isSymbolicLink())
    .map((entry) => entry.name)
    .sort();
  if (commandNames.length === 0) {
    throw new Error(`no commands found in ${relative(root, commandsDir)}`);
  }
  return commandNames.map((command) => {
    const path = resolve(commandsDir, command);
    if (!statSync(path).isFile()) {
      throw new Error(`${command}: command target is not a file`);
    }
    return inspectCommand(path, command);
  });
}

function collectObserved(commands) {
  const observed = new Map();
  for (const command of commands) {
    for (const entry of command.imports) {
      const key = importKey(entry);
      const prior = observed.get(key);
      if (prior && signatureText(prior) !== signatureText(entry)) {
        throw new Error(
          `${key} has conflicting signatures: ${signatureText(prior)} vs ${signatureText(entry)} in ${command.command}`,
        );
      }
      const record = prior ?? { ...entry, commands: [] };
      record.commands.push(command.command);
      observed.set(key, record);
    }
  }
  return [...observed.values()].sort((a, b) =>
    importKey(a).localeCompare(importKey(b)),
  );
}

function loadManifest() {
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  if (
    manifest.schemaVersion !== expectedManifestSchemaVersion ||
    !Array.isArray(manifest.imports)
  ) {
    throw new Error(
      `ABI manifest must have schemaVersion ${expectedManifestSchemaVersion} and an imports array`,
    );
  }
  const declared = new Map();
  for (const entry of manifest.imports) {
    if (
      typeof entry.module !== 'string' ||
      typeof entry.name !== 'string' ||
      !Array.isArray(entry.params) ||
      !Array.isArray(entry.results) ||
      !allowedImportStatuses.has(entry.status)
    ) {
      throw new Error(
        'every ABI import requires a known status, module, name, params, and results',
      );
    }
    const key = importKey(entry);
    if (declared.has(key)) throw new Error(`duplicate ABI declaration ${key}`);
    declared.set(key, entry);
  }
  const aliases = new Map(Object.entries(manifest.moduleAliases ?? {}));
  for (const [alias, canonical] of aliases) {
    if (alias === canonical) throw new Error(`ABI module alias ${alias} is self-referential`);
  }
  return { manifest, declared, aliases };
}

function verifyObserved(observed, declared, aliases) {
  const failures = [];
  for (const entry of observed) {
    const key = importKey(entry);
    const canonicalModule = aliases.get(entry.module) ?? entry.module;
    const contract = declared.get(`${canonicalModule}.${entry.name}`);
    if (!contract) {
      failures.push(`${key}: undeclared import (${signatureText(entry)})`);
      continue;
    }
    if (signatureText(contract) !== signatureText(entry)) {
      failures.push(
        `${key}: expected ${signatureText(contract)}, observed ${signatureText(entry)}`,
      );
    }
  }
  if (failures.length > 0) {
    throw new Error(`WASM import audit failed:\n- ${failures.join('\n- ')}`);
  }
}

try {
  const commands = collectCommands();
  const observed = collectObserved(commands);
  if (printContract) {
    const contract = observed.map(({ commands: _commands, ...entry }) => entry);
    process.stdout.write(`${JSON.stringify(contract)}\n`);
    process.exit(0);
  }
  if (printObserved) {
    process.stdout.write(`${JSON.stringify(observed, null, 2)}\n`);
    process.exit(0);
  }

  const { manifest, declared, aliases } = loadManifest();
  verifyObserved(observed, declared, aliases);
  const distinctTargets = new Set(commands.map((entry) => entry.target)).size;
  const evidence = {
    schemaVersion: manifest.schemaVersion,
    abiVersion: manifest.abiVersion,
    commandEntries: commands.length,
    distinctModules: distinctTargets,
    observedImports: observed.length,
    commands,
  };
  if (jsonOutput) {
    process.stdout.write(`${JSON.stringify(evidence, null, 2)}\n`);
  } else {
    process.stdout.write(
      `WASM import audit passed: ${commands.length} commands, ${distinctTargets} modules, ${observed.length} imports\n`,
    );
  }
} catch (error) {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
}
