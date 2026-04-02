/**
 * Integration tests for git command.
 *
 * Verifies init, add, commit, branch, checkout (with DWIM), plus local and
 * smart-HTTP remote clone via kernel.exec() with real WASM binaries.
 */

import { describe, it, expect, afterEach, beforeAll, afterAll } from 'vitest';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer, type Server as HttpServer } from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import { createWasmVmRuntime } from '@rivet-dev/agent-os-posix';
import { createKernel, createInMemoryFileSystem, allowAll } from '@secure-exec/core';
import { createNodeHostNetworkAdapter } from '@secure-exec/nodejs';
import { COMMANDS_DIR, hasWasmBinaries } from '../helpers.js';
import type { Kernel } from '@secure-exec/core';

/** Check git binary exists in addition to base WASM binaries */
const hasGit = hasWasmBinaries && existsSync(resolve(COMMANDS_DIR, 'git'));
const hasHostGit = spawnSync('git', ['--version'], { stdio: 'ignore' }).status === 0;

/** Create a kernel with a world-writable in-memory filesystem */
async function createGitKernel() {
  const vfs = createInMemoryFileSystem();
  // Make root and /tmp writable by all users (WASM processes run as non-root)
  await (vfs as any).chmod('/', 0o1777);
  await vfs.mkdir('/tmp', { recursive: true });
  await (vfs as any).chmod('/tmp', 0o1777);
  const kernel = createKernel({ filesystem: vfs });
  await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
  return { kernel, vfs, dispose: () => kernel.dispose() };
}

async function createGitKernelWithNet() {
  const vfs = createInMemoryFileSystem();
  await (vfs as any).chmod('/', 0o1777);
  await vfs.mkdir('/tmp', { recursive: true });
  await (vfs as any).chmod('/tmp', 0o1777);
  const kernel = createKernel({
    filesystem: vfs,
    permissions: allowAll,
    hostNetworkAdapter: createNodeHostNetworkAdapter(),
  });
  await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
  return { kernel, vfs, dispose: () => kernel.dispose() };
}

function runHostGit(args: string[], cwd?: string) {
  const result = spawnSync('git', args, {
    cwd,
    encoding: 'utf8',
  });
  if (result.status !== 0) {
    throw new Error(
      `host git failed: git ${args.join(' ')}\nstdout: ${result.stdout}\nstderr: ${result.stderr}`,
    );
  }
}

/** Helper: run command and assert success */
async function run(kernel: Kernel, cmd: string): Promise<{ stdout: string; stderr: string; exitCode: number }> {
  const r = await kernel.exec(cmd);
  if (r.exitCode !== 0) {
    throw new Error(`Command failed (exit ${r.exitCode}): ${cmd}\nstdout: ${r.stdout}\nstderr: ${r.stderr}`);
  }
  return r;
}

describe.skipIf(!hasGit)('git command', () => {
  let kernel: Kernel;
  let vfs: any;
  let dispose: () => Promise<void>;

  afterEach(async () => {
    await dispose?.();
  });

  it('init creates .git directory structure', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    const result = await run(kernel, 'git init /repo');
    expect(result.stdout).toContain('Initialized empty Git repository');

    expect(await vfs.exists('/repo/.git/HEAD')).toBe(true);
    expect(await vfs.exists('/repo/.git/objects')).toBe(true);
    expect(await vfs.exists('/repo/.git/refs/heads')).toBe(true);

    const head = new TextDecoder().decode(await vfs.readFile('/repo/.git/HEAD'));
    expect(head.trim()).toBe('ref: refs/heads/main');
  });

  it('add + commit creates objects and updates ref', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /repo');
    await kernel.writeFile('/repo/hello.txt', 'hello world\n');
    await run(kernel, 'git -C /repo add hello.txt');
    await run(kernel, "git -C /repo commit -m 'first commit'");

    expect(await vfs.exists('/repo/.git/refs/heads/main')).toBe(true);
  });

  it('branch lists branches with current marked', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /repo');
    await kernel.writeFile('/repo/file.txt', 'content\n');
    await run(kernel, 'git -C /repo add file.txt');
    await run(kernel, "git -C /repo commit -m 'init'");

    const result = await run(kernel, 'git -C /repo branch');
    expect(result.stdout.trim()).toBe('* main');
  });

  it('checkout -b creates a new branch', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /repo');
    await kernel.writeFile('/repo/file.txt', 'content\n');
    await run(kernel, 'git -C /repo add file.txt');
    await run(kernel, "git -C /repo commit -m 'init'");

    await run(kernel, 'git -C /repo checkout -b feature');

    const result = await run(kernel, 'git -C /repo branch');
    const lines = result.stdout.trim().split('\n').map((l: string) => l.trim());
    expect(lines).toContain('* feature');
    expect(lines).toContain('main');
  });

  it('full quickstart scenario: init, commit, branch, clone, checkout', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    // Create origin repo
    await run(kernel, 'git init /tmp/origin');
    await kernel.writeFile('/tmp/origin/README.md', '# demo repo\n');
    await run(kernel, 'git -C /tmp/origin add README.md');
    await run(kernel, "git -C /tmp/origin commit -m 'initial commit'");

    // Check default branch
    let r = await run(kernel, 'git -C /tmp/origin branch');
    expect(r.stdout.trim()).toBe('* main');

    // Create feature branch with a new file
    await run(kernel, 'git -C /tmp/origin checkout -b feature');
    await kernel.writeFile('/tmp/origin/feature.txt', 'checked out from feature\n');
    await run(kernel, 'git -C /tmp/origin add feature.txt');
    await run(kernel, "git -C /tmp/origin commit -m 'add feature file'");

    // Switch back to main
    await run(kernel, 'git -C /tmp/origin checkout main');

    // Clone
    await run(kernel, 'git clone /tmp/origin /tmp/clone');

    // Clone should only show main branch initially
    r = await run(kernel, 'git -C /tmp/clone branch');
    expect(r.stdout.trim()).toBe('* main');

    // Checkout feature (DWIM from remote tracking)
    await run(kernel, 'git -C /tmp/clone checkout feature');

    // Now both branches should be listed
    r = await run(kernel, 'git -C /tmp/clone branch');
    const branches = r.stdout.trim().split('\n').map((l: string) => l.trim());
    expect(branches).toContain('* feature');
    expect(branches).toContain('main');

    // Verify feature file exists in clone
    const featureContent = new TextDecoder().decode(await vfs.readFile('/tmp/clone/feature.txt'));
    expect(featureContent).toBe('checked out from feature\n');

    // Verify README exists too
    const readmeContent = new TextDecoder().decode(await vfs.readFile('/tmp/clone/README.md'));
    expect(readmeContent).toBe('# demo repo\n');
  });

  it('clone without an explicit destination uses the source basename', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin');
    await kernel.writeFile('/tmp/origin/README.md', 'default destination\n');
    await run(kernel, 'git -C /tmp/origin add README.md');
    await run(kernel, "git -C /tmp/origin commit -m 'seed'");

    await run(kernel, 'mkdir -p /work');
    await run(kernel, 'git -C /work clone /tmp/origin');

    expect(await vfs.exists('/work/origin/.git/HEAD')).toBe(true);
    const readmeContent = new TextDecoder().decode(await vfs.readFile('/work/origin/README.md'));
    expect(readmeContent).toBe('default destination\n');
  });

  it('clone without an explicit destination strips a trailing .git suffix', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin.git');
    await kernel.writeFile('/tmp/origin.git/README.md', 'suffix destination\n');
    await run(kernel, 'git -C /tmp/origin.git add README.md');
    await run(kernel, "git -C /tmp/origin.git commit -m 'seed'");

    await run(kernel, 'mkdir -p /work');
    await run(kernel, 'git -C /work clone /tmp/origin.git');

    expect(await vfs.exists('/work/origin/.git/HEAD')).toBe(true);
    const readmeContent = new TextDecoder().decode(await vfs.readFile('/work/origin/README.md'));
    expect(readmeContent).toBe('suffix destination\n');
  });

  it('clone into an existing empty destination directory succeeds', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin');
    await kernel.writeFile('/tmp/origin/README.md', 'empty destination\n');
    await run(kernel, 'git -C /tmp/origin add README.md');
    await run(kernel, "git -C /tmp/origin commit -m 'seed'");

    await run(kernel, 'mkdir -p /tmp/clone');
    await run(kernel, 'git clone /tmp/origin /tmp/clone');

    expect(await vfs.exists('/tmp/clone/.git/HEAD')).toBe(true);
    const readmeContent = new TextDecoder().decode(await vfs.readFile('/tmp/clone/README.md'));
    expect(readmeContent).toBe('empty destination\n');
  });

  it('clone rejects a non-empty destination directory', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin');
    await kernel.writeFile('/tmp/origin/README.md', 'origin\n');
    await run(kernel, 'git -C /tmp/origin add README.md');
    await run(kernel, "git -C /tmp/origin commit -m 'seed'");

    await run(kernel, 'mkdir -p /tmp/clone');
    await kernel.writeFile('/tmp/clone/existing.txt', 'keep me\n');

    const result = await kernel.exec('git clone /tmp/origin /tmp/clone');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toMatch(/already exists|not an empty directory|destination/i);

    const existing = new TextDecoder().decode(await vfs.readFile('/tmp/clone/existing.txt'));
    expect(existing).toBe('keep me\n');
    expect(await vfs.exists('/tmp/clone/.git')).toBe(false);
  });

  it('clone of a missing repository fails without leaving a partial destination', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    const result = await kernel.exec('git clone /tmp/missing /tmp/clone');
    expect(result.exitCode).not.toBe(0);
    expect(result.stderr).toMatch(/not a git repository|missing|no such file|fatal/i);
    expect(await vfs.exists('/tmp/clone')).toBe(false);
  });

  it('clone of an empty repository succeeds and leaves an empty worktree', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin');
    await run(kernel, 'git clone /tmp/origin /tmp/clone');

    const head = new TextDecoder().decode(await vfs.readFile('/tmp/clone/.git/HEAD'));
    expect(head.trim()).toBe('ref: refs/heads/main');
    expect(await vfs.exists('/tmp/clone/.git/config')).toBe(true);
    expect(await vfs.exists('/tmp/clone/.git/refs/heads/main')).toBe(false);
    expect(await vfs.exists('/tmp/clone/README.md')).toBe(false);
  });

  it('clone preserves nested directory trees', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin');
    await run(kernel, 'mkdir -p /tmp/origin/src/nested');
    await kernel.writeFile('/tmp/origin/src/nested/file.txt', 'nested payload\n');
    await kernel.writeFile('/tmp/origin/src/root.txt', 'root payload\n');
    await run(kernel, 'git -C /tmp/origin add src/nested/file.txt src/root.txt');
    await run(kernel, "git -C /tmp/origin commit -m 'nested tree'");

    await run(kernel, 'git clone /tmp/origin /tmp/clone');

    const nested = new TextDecoder().decode(await vfs.readFile('/tmp/clone/src/nested/file.txt'));
    const root = new TextDecoder().decode(await vfs.readFile('/tmp/clone/src/root.txt'));
    expect(nested).toBe('nested payload\n');
    expect(root).toBe('root payload\n');
  });

  it('clone honors the source default branch when HEAD is not main', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin');
    await kernel.writeFile('/tmp/origin/README.md', 'main branch\n');
    await run(kernel, 'git -C /tmp/origin add README.md');
    await run(kernel, "git -C /tmp/origin commit -m 'main'");

    await run(kernel, 'git -C /tmp/origin checkout -b trunk');
    await kernel.writeFile('/tmp/origin/trunk.txt', 'trunk branch\n');
    await run(kernel, 'git -C /tmp/origin add trunk.txt');
    await run(kernel, "git -C /tmp/origin commit -m 'trunk'");

    await run(kernel, 'git clone /tmp/origin /tmp/clone');

    const head = new TextDecoder().decode(await vfs.readFile('/tmp/clone/.git/HEAD'));
    expect(head.trim()).toBe('ref: refs/heads/trunk');
    expect(await vfs.exists('/tmp/clone/.git/refs/heads/trunk')).toBe(true);
    const trunk = new TextDecoder().decode(await vfs.readFile('/tmp/clone/trunk.txt'));
    expect(trunk).toBe('trunk branch\n');
  });

  it('clone copies nested branch refs and checkout DWIM works for branch names with slashes', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'git init /tmp/origin');
    await kernel.writeFile('/tmp/origin/README.md', '# demo repo\n');
    await run(kernel, 'git -C /tmp/origin add README.md');
    await run(kernel, "git -C /tmp/origin commit -m 'initial commit'");

    await run(kernel, 'git -C /tmp/origin checkout -b feature/deep');
    await kernel.writeFile('/tmp/origin/feature.txt', 'nested branch payload\n');
    await run(kernel, 'git -C /tmp/origin add feature.txt');
    await run(kernel, "git -C /tmp/origin commit -m 'nested branch'");
    await run(kernel, 'git -C /tmp/origin checkout main');

    await run(kernel, 'git clone /tmp/origin /tmp/clone');

    expect(await vfs.exists('/tmp/clone/.git/refs/remotes/origin/feature/deep')).toBe(true);

    await run(kernel, 'git -C /tmp/clone checkout feature/deep');
    const featureContent = new TextDecoder().decode(await vfs.readFile('/tmp/clone/feature.txt'));
    expect(featureContent).toBe('nested branch payload\n');
    const head = new TextDecoder().decode(await vfs.readFile('/tmp/clone/.git/HEAD'));
    expect(head.trim()).toBe('ref: refs/heads/feature/deep');
  });

  it('clone works with relative source and destination paths', async () => {
    ({ kernel, vfs, dispose } = await createGitKernel());

    await run(kernel, 'mkdir -p /tmp/work');
    await run(kernel, 'git init /tmp/work/origin');
    await kernel.writeFile('/tmp/work/origin/README.md', 'relative clone\n');
    await run(kernel, 'git -C /tmp/work/origin add README.md');
    await run(kernel, "git -C /tmp/work/origin commit -m 'seed'");

    await run(kernel, 'git -C /tmp/work clone ./origin ./clone');

    expect(await vfs.exists('/tmp/work/clone/.git/HEAD')).toBe(true);
    const readmeContent = new TextDecoder().decode(await vfs.readFile('/tmp/work/clone/README.md'));
    expect(readmeContent).toBe('relative clone\n');
  });

  describe.skipIf(!hasHostGit)('remote clone over smart HTTP', () => {
    let repoRoot: string;
    let httpServer: HttpServer;
    let httpPort: number;

    beforeAll(async () => {
      repoRoot = mkdtempSync(join(tmpdir(), 'agentos-git-http-'));
      const worktree = join(repoRoot, 'worktree');
      const origin = join(repoRoot, 'origin.git');

      runHostGit(['-c', 'init.defaultBranch=main', 'init', worktree]);
      writeFileSync(join(worktree, 'README.md'), 'remote smart clone\n');
      runHostGit(['-C', worktree, 'add', 'README.md']);
      runHostGit([
        '-C', worktree,
        '-c', 'user.name=Agent OS',
        '-c', 'user.email=agent@example.com',
        'commit',
        '-m',
        'seed',
      ]);

      runHostGit(['-C', worktree, 'checkout', '-b', 'feature/deep']);
      writeFileSync(join(worktree, 'feature.txt'), 'remote branch payload\n');
      runHostGit(['-C', worktree, 'add', 'feature.txt']);
      runHostGit([
        '-C', worktree,
        '-c', 'user.name=Agent OS',
        '-c', 'user.email=agent@example.com',
        'commit',
        '-m',
        'feature branch',
      ]);

      runHostGit(['-C', worktree, 'checkout', 'main']);
      runHostGit(['clone', '--bare', worktree, origin]);
      runHostGit(['-C', origin, 'repack', '-a', '-d', '-f', '--depth=50', '--window=50']);

      httpServer = createServer((req, res) => {
        const url = new URL(req.url ?? '/', 'http://127.0.0.1');
        const bodyChunks: Buffer[] = [];

        req.on('data', (chunk) => {
          bodyChunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
        });

        req.on('end', () => {
          const requestBody = Buffer.concat(bodyChunks);
          const gitProtocol = req.headers['git-protocol'];
          const env = {
            ...process.env,
            GIT_HTTP_EXPORT_ALL: '1',
            GIT_PROJECT_ROOT: repoRoot,
            PATH_INFO: url.pathname,
            QUERY_STRING: url.search.startsWith('?') ? url.search.slice(1) : url.search,
            REQUEST_METHOD: req.method ?? 'GET',
            CONTENT_TYPE: String(req.headers['content-type'] ?? ''),
            CONTENT_LENGTH: String(requestBody.length),
            REMOTE_ADDR: '127.0.0.1',
            GIT_PROTOCOL: typeof gitProtocol === 'string' ? gitProtocol : '',
            HTTP_GIT_PROTOCOL: typeof gitProtocol === 'string' ? gitProtocol : '',
          };

          const child = spawn('git', ['http-backend'], { env });
          const stdout: Buffer[] = [];
          const stderr: Buffer[] = [];

          child.stdout.on('data', (chunk) => {
            stdout.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
          });
          child.stderr.on('data', (chunk) => {
            stderr.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
          });
          child.on('error', (error) => {
            res.writeHead(500, { 'Content-Type': 'text/plain' });
            res.end(String(error));
          });
          child.on('close', (code) => {
            const output = Buffer.concat(stdout);
            const headerSep = output.indexOf(Buffer.from('\r\n\r\n'));
            const altSep = output.indexOf(Buffer.from('\n\n'));
            const sepIndex = headerSep >= 0 ? headerSep : altSep;
            const sepLen = headerSep >= 0 ? 4 : altSep >= 0 ? 2 : 0;

            if (code !== 0 && sepIndex === -1) {
              res.writeHead(500, { 'Content-Type': 'text/plain' });
              res.end(Buffer.concat(stderr));
              return;
            }

            if (sepIndex === -1) {
              res.writeHead(500, { 'Content-Type': 'text/plain' });
              res.end(output);
              return;
            }

            const headerText = output.subarray(0, sepIndex).toString('utf8');
            const responseBody = output.subarray(sepIndex + sepLen);
            let status = 200;
            const headers: Record<string, string> = {};

            for (const line of headerText.split(/\r?\n/)) {
              if (!line) continue;
              const colon = line.indexOf(':');
              if (colon === -1) continue;
              const name = line.slice(0, colon);
              const value = line.slice(colon + 1).trim();
              if (name.toLowerCase() === 'status') {
                status = Number.parseInt(value, 10) || 200;
              } else {
                headers[name] = value;
              }
            }

            res.writeHead(status, headers);
            res.end(responseBody);
          });

          child.stdin.end(requestBody);
        });
      });

      await new Promise<void>((resolveListen) => {
        httpServer.listen(0, '127.0.0.1', resolveListen);
      });
      httpPort = (httpServer.address() as import('node:net').AddressInfo).port;
    });

    afterAll(async () => {
      await new Promise<void>((resolveClose) => httpServer.close(() => resolveClose()));
      rmSync(repoRoot, { recursive: true, force: true });
    });

    it('clone fetches refs and worktree contents from a smart HTTP remote', async () => {
      ({ kernel, vfs, dispose } = await createGitKernelWithNet());

      await run(kernel, `git clone http://127.0.0.1:${httpPort}/origin.git /tmp/clone`);

      const head = new TextDecoder().decode(await vfs.readFile('/tmp/clone/.git/HEAD'));
      expect(head.trim()).toBe('ref: refs/heads/main');

      const readme = new TextDecoder().decode(await vfs.readFile('/tmp/clone/README.md'));
      expect(readme).toBe('remote smart clone\n');
      expect(await vfs.exists('/tmp/clone/.git/refs/remotes/origin/feature/deep')).toBe(true);

      await run(kernel, 'git -C /tmp/clone checkout feature/deep');
      const feature = new TextDecoder().decode(await vfs.readFile('/tmp/clone/feature.txt'));
      expect(feature).toBe('remote branch payload\n');
    });
  });
});
