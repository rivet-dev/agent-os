// Nightly: requires a non-core registry command.
/**
 * Integration tests for the real OpenSSH ssh client (10.4p1, linked with a
 * hermetic OpenSSL libcrypto build for standard software algorithm coverage).
 *
 * Mirrors the git HTTPS suite's loopback-server harness: an in-test SSH
 * server (the pure-JS `ssh2` package) listens on a host loopback port that
 * the kernel exempts, and the WASM ssh client connects out through the
 * kernel's host_net path. Covers batch/key-based exec, host-key
 * verification (known_hosts, StrictHostKeyChecking=accept-new), publickey
 * auth failure, and git-over-ssh (clone + push against host
 * git-upload-pack/git-receive-pack).
 */

import { describe, it, expect, afterEach, beforeAll, afterAll, vi } from 'vitest';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawn, spawnSync } from 'node:child_process';
import { Server as SshServer, utils as sshUtils } from 'ssh2';
import type { Connection } from 'ssh2';
import { createWasmVmRuntime } from '@rivet-dev/agentos-test-harness';
import {
  allowAll,
  C_BUILD_DIR,
  COMMANDS_DIR,
  createInMemoryFileSystem,
  createKernel,
  describeIf,
  hasWasmBinaries,
  hasCWasmBinaries,
} from '@rivet-dev/agentos-test-harness';
import type { Kernel } from '@rivet-dev/agentos-test-harness';

vi.setConfig({ testTimeout: 60_000, hookTimeout: 60_000 });

const hasSsh = hasWasmBinaries && existsSync(resolve(COMMANDS_DIR, 'ssh'));
const hasGit = hasWasmBinaries && existsSync(resolve(COMMANDS_DIR, 'git'));
const hasHostGit = spawnSync('git', ['--version'], { stdio: 'ignore' }).status === 0;
const hasProxyHelper = hasCWasmBinaries('ssh_proxy_helper');
const hasSkHelperContract = hasCWasmBinaries('ssh_sk_helper_contract');
const sshCommandDirs = [C_BUILD_DIR, COMMANDS_DIR].filter((dir) => existsSync(dir));

const SSH_USER = 'agentos';

type GeneratedSshKeyPair = ReturnType<typeof sshUtils.generateKeyPairSync>;

/**
 * ssh2@1.17.0 occasionally emits an Ed25519 OpenSSH private key that its own
 * parser rejects. Keep ephemeral test setup deterministic without weakening
 * the key type exercised by the real OpenSSH client.
 */
function generateEd25519KeyPair(): GeneratedSshKeyPair {
  const maxAttempts = 8;
  let lastError: Error | undefined;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const pair = sshUtils.generateKeyPairSync('ed25519');
    const parsed = sshUtils.parseKey(pair.private);
    if (!(parsed instanceof Error)) return pair;
    lastError = parsed;
  }
  throw new Error(
    `ssh2 generated ${maxAttempts} invalid Ed25519 keypairs`,
    { cause: lastError },
  );
}

interface TestKeys {
  hostKey: GeneratedSshKeyPair;
  clientKey: GeneratedSshKeyPair;
  /** A second client keypair the server does NOT authorize. */
  wrongClientKey: GeneratedSshKeyPair;
  /** A second host key used to simulate a changed/unknown server identity. */
  otherHostKey: GeneratedSshKeyPair;
}

function generateKeys(): TestKeys {
  return {
    hostKey: generateEd25519KeyPair(),
    clientKey: generateEd25519KeyPair(),
    wrongClientKey: generateEd25519KeyPair(),
    otherHostKey: generateEd25519KeyPair(),
  };
}

/** Standard ssh2 publickey-auth handler restricted to one authorized key. */
function installAuthHandler(client: Connection, authorizedPublicKey: string) {
  const allowed = sshUtils.parseKey(authorizedPublicKey);
  if (allowed instanceof Error) throw allowed;
  client.on('authentication', (ctx) => {
    if (ctx.method !== 'publickey') {
      return ctx.reject(['publickey']);
    }
    const matches =
      ctx.key.algo === allowed.type &&
      ctx.key.data.equals(allowed.getPublicSSH());
    if (!matches) {
      return ctx.reject(['publickey']);
    }
    if (ctx.signature && ctx.blob) {
      if (allowed.verify(ctx.blob, ctx.signature, ctx.hashAlgo) === true) {
        return ctx.accept();
      }
      return ctx.reject(['publickey']);
    }
    // pk-check phase (no signature yet): tell the client the key is OK.
    return ctx.accept();
  });
}

/** exec handler: `echo hello`-style canned command execution. */
function installEchoExecHandler(client: Connection) {
  client.on('ready', () => {
    client.on('session', (acceptSession) => {
      const session = acceptSession();
      session.on('exec', (acceptExec, _reject, info) => {
        const stream = acceptExec();
        if (info.command === 'echo hello') {
          stream.write('hello\n');
          stream.exit(0);
        } else {
          stream.stderr.write(`unknown test command: ${info.command}\n`);
          stream.exit(127);
        }
        stream.end();
      });
    });
  });
}

/**
 * exec handler bridging `git-upload-pack '/x.git'` / `git-receive-pack ...`
 * to the host git against a bare repo root — an in-test stand-in for a real
 * SSH git host (what git-shell does on a server).
 */
function installGitExecHandler(client: Connection, repoRoot: string) {
  client.on('ready', () => {
    client.on('session', (acceptSession) => {
      const session = acceptSession();
      session.on('exec', (acceptExec, reject, info) => {
        const match = /^(git-upload-pack|git-receive-pack|git-upload-archive) '(.*)'$/.exec(
          info.command,
        );
        if (!match) {
          const stream = acceptExec();
          stream.stderr.write(`unsupported command: ${info.command}\n`);
          stream.exit(128);
          stream.end();
          return;
        }
        const [, service, requestedPath] = match;
        const repoPath = join(repoRoot, requestedPath.replace(/^\/+/, ''));
        const stream = acceptExec();
        const child = spawn('git', [service.replace(/^git-/, ''), repoPath]);
        stream.pipe(child.stdin);
        child.stdout.pipe(stream, { end: false });
        child.stderr.pipe(stream.stderr, { end: false });
        child.on('close', (code) => {
          stream.exit(code ?? 1);
          stream.end();
        });
        child.on('error', () => {
          stream.exit(127);
          stream.end();
        });
      });
    });
  });
}

async function listen(server: SshServer): Promise<number> {
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  return (server.address() as import('node:net').AddressInfo).port;
}

async function createSshKernel(
  loopbackExemptPorts: number[],
  wasmActiveCpuTimeLimitMs?: number,
) {
  const vfs = createInMemoryFileSystem();
  await (vfs as any).chmod('/', 0o1777);
  await vfs.mkdir('/tmp', { recursive: true });
  await (vfs as any).chmod('/tmp', 0o1777);
  const kernel = createKernel({
    filesystem: vfs,
    permissions: allowAll,
    loopbackExemptPorts,
    ...(wasmActiveCpuTimeLimitMs === undefined
      ? {}
      : { limits: { wasm: { activeCpuTimeLimitMs: wasmActiveCpuTimeLimitMs } } }),
    syncFilesystemOnDispose: false,
  });
  await kernel.mount(createWasmVmRuntime({ commandDirs: sshCommandDirs }));
  return { kernel, vfs, dispose: () => kernel.dispose() };
}

async function run(
  kernel: Kernel,
  cmd: string,
): Promise<{ stdout: string; stderr: string; exitCode: number }> {
  const r = await kernel.exec(cmd);
  if (r.exitCode !== 0) {
    throw new Error(
      `Command failed (exit ${r.exitCode}): ${cmd}\nstdout: ${r.stdout}\nstderr: ${r.stderr}`,
    );
  }
  return r;
}

/**
 * Resolve the guest user's home directory (ssh resolves `~` through
 * getpwuid(getuid())->pw_dir, which the runtime keeps aligned with $HOME).
 */
async function guestHome(kernel: Kernel): Promise<string> {
  const r = await run(kernel, "sh -c 'echo $HOME'");
  const home = r.stdout.trim();
  expect(home).toMatch(/^\//);
  return home;
}

/** Seed ~/.ssh with an identity and (optionally) a known_hosts line. */
async function seedSshDir(
  kernel: Kernel,
  vfs: any,
  home: string,
  privateKey: string,
  knownHostsLine?: string,
): Promise<string> {
  const sshDir = `${home}/.ssh`;
  await vfs.mkdir(sshDir, { recursive: true });
  await vfs.chmod(sshDir, 0o700);
  await kernel.writeFile(`${sshDir}/id_ed25519`, `${privateKey}\n`);
  await vfs.chmod(`${sshDir}/id_ed25519`, 0o600);
  if (knownHostsLine !== undefined) {
    await kernel.writeFile(`${sshDir}/known_hosts`, `${knownHostsLine}\n`);
    await vfs.chmod(`${sshDir}/known_hosts`, 0o600);
  }
  return sshDir;
}

function knownHostsEntry(port: number, hostPublicKey: string): string {
  // `[host]:port` hashing syntax from sshd(8) AUTHORIZED_KEYS/known_hosts
  // format; non-default ports always use the bracketed form.
  return `[127.0.0.1]:${port} ${hostPublicKey}`;
}

// TODO(P6): requires the ssh WASM artifact, intentionally excluded from the
// fast software-build gate (same as git).
describeIf(hasSsh, 'ssh command', () => {
  let kernel: Kernel;
  let vfs: any;
  let dispose: (() => Promise<void>) | undefined;

  afterEach(async () => {
    await dispose?.();
    dispose = undefined;
  });

  it('ssh -V reports the real OpenSSH and OpenSSL versions', async () => {
    ({ kernel, vfs, dispose } = await createSshKernel([]));
    const r = await kernel.exec('ssh -V');
    expect(r.exitCode).toBe(0);
    const banner = `${r.stdout}${r.stderr}`;
    expect(banner).toMatch(/OpenSSH_10\.4/);
    expect(banner).toMatch(/OpenSSL 3\.5\.7/);
  });

  it.each([
    ['key', ['ssh-ed25519', 'ssh-rsa', 'ecdsa-sha2-nistp256', 'ecdsa-sha2-nistp384', 'ecdsa-sha2-nistp521', 'sk-ssh-ed25519@openssh.com', 'sk-ecdsa-sha2-nistp256@openssh.com']],
    ['key-sig', ['ssh-ed25519', 'ssh-rsa', 'rsa-sha2-256', 'rsa-sha2-512', 'ecdsa-sha2-nistp256', 'ecdsa-sha2-nistp384', 'ecdsa-sha2-nistp521', 'sk-ssh-ed25519@openssh.com', 'sk-ecdsa-sha2-nistp256@openssh.com']],
    ['cipher', ['chacha20-poly1305@openssh.com', 'aes128-gcm@openssh.com', 'aes256-gcm@openssh.com', 'aes128-cbc', 'aes256-cbc', '3des-cbc']],
  ])('ssh -Q %s includes the standard software crypto families', async (query, expected) => {
    ({ kernel, vfs, dispose } = await createSshKernel([]));
    const r = await kernel.exec(`ssh -Q ${query}`);
    expect(r.exitCode, `stdout:\n${r.stdout}\nstderr:\n${r.stderr}`).toBe(0);
    const algorithms = new Set(r.stdout.trim().split(/\s+/));
    for (const algorithm of expected) {
      expect(algorithms.has(algorithm), `missing ${query} algorithm ${algorithm}`).toBe(true);
    }
  });

  it('Match exec uses the spawned command exit status like native OpenSSH', async () => {
    ({ kernel, vfs, dispose } = await createSshKernel([]));
    await kernel.writeFile(
      '/tmp/ssh-match.conf',
      [
        'Match originalhost parity exec "true"',
        '  HostName matched.invalid',
        'Host parity',
        '  HostName unmatched.invalid',
        '',
      ].join('\n'),
    );
    const r = await kernel.exec('ssh -G -F /tmp/ssh-match.conf parity');
    expect(r.exitCode, r.stderr).toBe(0);
    expect(r.stdout).toMatch(/^hostname matched\.invalid$/m);
  });

  it('ships the client helpers and reaches their real wire-protocol parsers', async () => {
    ({ kernel, vfs, dispose } = await createSshKernel([]));
    await vfs.mkdir('/etc/ssh', { recursive: true });
    const hostKey = generateEd25519KeyPair();
    await kernel.writeFile('/etc/ssh/ssh_host_ed25519_key', `${hostKey.private}\n`);
    await vfs.chmod('/etc/ssh/ssh_host_ed25519_key', 0o600);
    await kernel.writeFile('/etc/ssh/ssh_config', 'EnableSSHKeysign yes\n');

    const keysign = await kernel.exec('ssh-keysign');
    expect(keysign.exitCode).not.toBe(127);
    expect(keysign.stderr).toMatch(/ssh_msg_recv failed|incomplete message/i);
    expect(keysign.stderr).not.toMatch(/not found|ENOENT/i);

    const sk = await kernel.exec('ssh-sk-helper');
    expect(sk.exitCode).not.toBe(127);
    // Like native OpenSSH, ssh-sk-helper sends its pre-protocol failure to
    // syslog by default and may therefore have empty stderr on EOF.
    expect(sk.stderr).not.toMatch(/not found|ENOENT/i);
  });

  it.runIf(hasSkHelperContract)('exercises the security-key helper framed protocol', async () => {
    ({ kernel, vfs, dispose } = await createSshKernel([]));
    const framed = await kernel.exec('ssh_sk_helper_contract');
    expect(framed.exitCode, framed.stderr).toBe(0);
    expect(framed.stdout).toBe('ssh_sk_helper_framed_provider_error=yes\n');
  });

  it('uses ssh-keysign for a signed hostbased authentication request', async () => {
    const serverHostKey = generateEd25519KeyPair();
    const clientHostKey = generateEd25519KeyPair();
    const parsedClientHostKey = sshUtils.parseKey(clientHostKey.public);
    if (parsedClientHostKey instanceof Error) throw parsedClientHostKey;
    let sawSignedHostbased = false;
    const server = new SshServer({ hostKeys: [serverHostKey.private] }, (client) => {
      client.on('authentication', (context) => {
        const ctx = context as any;
        if (ctx.method !== 'hostbased') return ctx.reject(['hostbased']);
        sawSignedHostbased = Boolean(
          ctx.signature &&
          ctx.blob &&
          parsedClientHostKey.verify(ctx.blob, ctx.signature, ctx.hashAlgo),
        );
        return sawSignedHostbased ? ctx.accept() : ctx.reject(['hostbased']);
      });
      installEchoExecHandler(client);
    });
    const port = await listen(server);
    try {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        clientHostKey.private,
        knownHostsEntry(port, serverHostKey.public),
      );
      await vfs.mkdir('/etc/ssh', { recursive: true });
      await kernel.writeFile('/etc/ssh/ssh_host_ed25519_key', `${clientHostKey.private}\n`);
      await vfs.chmod('/etc/ssh/ssh_host_ed25519_key', 0o600);
      await kernel.writeFile('/etc/ssh/ssh_host_ed25519_key.pub', `${clientHostKey.public}\n`);
      await kernel.writeFile('/etc/ssh/ssh_config', 'Host *\n  EnableSSHKeysign yes\n');

      const result = await kernel.exec(
        `ssh -T -o BatchMode=yes -o HostbasedAuthentication=yes ` +
        `-o PreferredAuthentications=hostbased -o EnableSSHKeysign=yes ` +
        `-o HostbasedKeyTypes=ssh-ed25519 -p ${port} ${SSH_USER}@127.0.0.1 echo hello`,
      );
      expect(result.exitCode, result.stderr).toBe(0);
      expect(result.stdout).toBe('hello\n');
      expect(sawSignedHostbased).toBe(true);
    } finally {
      await new Promise<void>((resolveClose) => server.close(() => resolveClose()));
    }
  });

  describe('against an in-test ssh2 server', () => {
    let keys: TestKeys;
    let server: SshServer;
    let port: number;
    let sessionRequests: number;

    beforeAll(async () => {
      keys = generateKeys();
      sessionRequests = 0;
      server = new SshServer({ hostKeys: [keys.hostKey.private] }, (client) => {
        installAuthHandler(client, keys.clientKey.public);
        client.on('ready', () => {
          client.on('session', () => {
            sessionRequests++;
          });
        });
        installEchoExecHandler(client);
      });
      port = await listen(server);
    });

    afterAll(async () => {
      if (server) await new Promise<void>((r) => server.close(() => r()));
    });

    const sshCmd = (extra: string) =>
      `ssh -T -o BatchMode=yes ${extra} -p ${port} ${SSH_USER}@127.0.0.1 echo hello`;

    it('runs a remote command with ed25519 publickey auth and known_hosts', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const r = await kernel.exec(sshCmd(''));
      expect(r.stderr).not.toMatch(/setsockopt/i);
      expect(r.stdout).toBe('hello\n');
      expect(r.exitCode, `stdout:\n${r.stdout}\nstderr:\n${r.stderr}`).toBe(0);
    });

    it('runs LocalCommand through the same POSIX spawn path', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const r = await kernel.exec(
        sshCmd("-o PermitLocalCommand=yes -o LocalCommand='echo local-command'"),
      );
      expect(r.exitCode, r.stderr).toBe(0);
      expect(r.stdout).toContain('local-command');
      expect(r.stdout).toContain('hello\n');
    });

    it('uses KnownHostsCommand output for host-key verification', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(kernel, vfs, home, keys.clientKey.private);
      await kernel.writeFile(
        '/tmp/known-hosts-command',
        `#!/bin/sh\necho '${knownHostsEntry(port, keys.hostKey.public)}'\n`,
      );
      await vfs.chmod('/tmp/known-hosts-command', 0o700);

      const r = await kernel.exec(
        sshCmd('-o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null -o KnownHostsCommand=/tmp/known-hosts-command'),
      );
      expect(r.exitCode, r.stderr).toBe(0);
      expect(r.stdout).toBe('hello\n');
    });

    it('fails explicitly when ssh -f would need a live authenticated snapshot', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const r = await kernel.exec(
        `ssh -f -T -o BatchMode=yes -p ${port} ${SSH_USER}@127.0.0.1 echo hello`,
      );
      expect(r.exitCode).not.toBe(0);
      expect(r.stderr).toMatch(/cannot snapshot|live authenticated process state/i);
    });

    it('rejects multiplexed ssh -f before asking the master to open a session', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const controlPath = '/tmp/agentos-ssh-control';
      const target = `-p ${port} ${SSH_USER}@127.0.0.1`;
      let masterResult: Awaited<ReturnType<Kernel['exec']>> | undefined;
      const master = kernel.exec(
        `ssh -M -N -n -T -o BatchMode=yes -o ControlMaster=yes ` +
        `-o ControlPersist=no -S ${controlPath} ${target}`,
      );
      void master.then((result) => {
        masterResult = result;
      });

      try {
        const deadline = Date.now() + 10_000;
        for (;;) {
          try {
            await vfs.stat(controlPath);
            break;
          } catch {
            if (masterResult !== undefined) {
              throw new Error(
                `SSH control master exited early (${masterResult.exitCode}): ${masterResult.stderr}`,
              );
            }
            if (Date.now() >= deadline) {
              throw new Error('timed out waiting for the SSH control socket');
            }
            await new Promise((resolveWait) => setTimeout(resolveWait, 10));
          }
        }

        const before = sessionRequests;
        const r = await kernel.exec(
          `ssh -f -T -o BatchMode=yes -S ${controlPath} ${target} echo hello`,
        );
        expect(r.exitCode).not.toBe(0);
        expect(r.stderr).toMatch(/cannot snapshot|live authenticated process state/i);
        expect(sessionRequests).toBe(before);

        const check = await kernel.exec(`ssh -S ${controlPath} -O check ${target}`);
        expect(check.exitCode, check.stderr).toBe(0);
      } finally {
        await kernel.exec(`ssh -S ${controlPath} -O exit ${target}`);
        await master;
      }
    });

    it.runIf(hasProxyHelper)('ProxyCommand preserves full-duplex SSH transport through a spawned helper', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const r = await kernel.exec(
        sshCmd("-o ProxyCommand='ssh_proxy_helper stdio %h %p'"),
      );
      expect(r.exitCode, `stdout:\n${r.stdout}\nstderr:\n${r.stderr}`).toBe(0);
      expect(r.stdout).toBe('hello\n');
    });

    it.runIf(hasProxyHelper)('ProxyUseFdpass transfers a connected TCP socket from the spawned helper', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const r = await kernel.exec(
        sshCmd("-o ProxyUseFdpass=yes -o ProxyCommand='ssh_proxy_helper fdpass %h %p'"),
      );
      expect(r.exitCode, `stdout:\n${r.stdout}\nstderr:\n${r.stderr}`).toBe(0);
      expect(r.stdout).toBe('hello\n');
    });

    it('propagates the remote exit status', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const r = await kernel.exec(
        `ssh -T -o BatchMode=yes -p ${port} ${SSH_USER}@127.0.0.1 false-command`,
      );
      expect(r.exitCode).toBe(127);
      expect(r.stderr).toContain('unknown test command');
    });

    it('fails publickey auth with an unauthorized client key', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.wrongClientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const r = await kernel.exec(sshCmd(''));
      expect(r.exitCode).not.toBe(0);
      expect(r.stderr).toMatch(/Permission denied \(publickey\)/i);
      expect(r.stdout).not.toContain('hello');
    });

    it('fails host key verification when known_hosts pins a different key', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.otherHostKey.public),
      );

      const r = await kernel.exec(sshCmd(''));
      expect(r.exitCode).not.toBe(0);
      expect(r.stderr).toMatch(
        /REMOTE HOST IDENTIFICATION HAS CHANGED|Host key verification failed/i,
      );
      expect(r.stdout).not.toContain('hello');
    });

    it('fails closed in BatchMode when the host key is unknown', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      await seedSshDir(kernel, vfs, home, keys.clientKey.private);

      const r = await kernel.exec(sshCmd(''));
      expect(r.exitCode).not.toBe(0);
      expect(r.stderr).toMatch(/Host key verification failed/i);
    });

    it('StrictHostKeyChecking=accept-new succeeds and records the host key', async () => {
      ({ kernel, vfs, dispose } = await createSshKernel([port]));
      const home = await guestHome(kernel);
      const sshDir = await seedSshDir(kernel, vfs, home, keys.clientKey.private);

      const r = await kernel.exec(sshCmd('-o StrictHostKeyChecking=accept-new'));
      expect(r.stdout).toBe('hello\n');
      expect(r.exitCode).toBe(0);
      expect(r.stderr).toMatch(/Permanently added/i);

      const knownHosts = new TextDecoder().decode(
        await kernel.readFile(`${sshDir}/known_hosts`),
      );
      const hostKeyBlob = keys.hostKey.public.split(/\s+/)[1];
      expect(knownHosts).toContain(`[127.0.0.1]:${port}`);
      expect(knownHosts).toContain(hostKeyBlob);
    });
  });

  // git-over-ssh: the WASM git execs the WASM ssh from PATH (git connect.c),
  // which tunnels git-upload-pack / git-receive-pack to the host-side bare
  // repo behind the ssh2 server.
  //
  // This also regresses mixed polling in the runtime: ssh polls a dup'd stdin
  // pipe alongside its host-net socket while Git waits for the remote helper.
  describeIf(hasGit && hasHostGit, 'git-over-ssh clone/push', () => {
    let keys: TestKeys;
    let server: SshServer;
    let port: number;
    let repoRoot: string;

    const gitConfig = [
      '-c safe.directory=*',
      '-c init.defaultBranch=main',
      '-c user.name=agentos',
      '-c user.email=agentos@example.invalid',
    ].join(' ');
    const git = (args: string) => `git ${gitConfig} ${args}`;

    function runHostGit(args: string[], cwd?: string) {
      const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
      if (result.status !== 0) {
        throw new Error(
          `host git failed: git ${args.join(' ')}\nstdout: ${result.stdout}\nstderr: ${result.stderr}`,
        );
      }
    }

    beforeAll(async () => {
      keys = generateKeys();
      repoRoot = mkdtempSync(join(tmpdir(), 'agentos-git-ssh-'));
      const worktree = join(repoRoot, 'worktree');
      const origin = join(repoRoot, 'origin.git');

      runHostGit(['-c', 'init.defaultBranch=main', 'init', worktree]);
      writeFileSync(join(worktree, 'README.md'), 'remote ssh clone\n');
      runHostGit(['-C', worktree, 'add', 'README.md']);
      runHostGit([
        '-C', worktree,
        '-c', 'user.name=agentos', '-c', 'user.email=agentos@example.invalid',
        'commit', '-m', 'seed',
      ]);
      runHostGit(['clone', '--bare', worktree, origin]);

      server = new SshServer({ hostKeys: [keys.hostKey.private] }, (client) => {
        installAuthHandler(client, keys.clientKey.public);
        installGitExecHandler(client, repoRoot);
      });
      port = await listen(server);
    });

    afterAll(async () => {
      if (server) await new Promise<void>((r) => server.close(() => r()));
      rmSync(repoRoot, { recursive: true, force: true });
    });

    it('clones and pushes over ssh://', async () => {
      // This is a functional transport/concurrency test, not the active-CPU
      // safeguard test. A cold Wasmtime Git + OpenSSH process tree can exceed
      // the 30s default; limit behavior is covered by wasmtime_safety.rs.
      ({ kernel, vfs, dispose } = await createSshKernel([port], 120_000));
      const home = await guestHome(kernel);
      await seedSshDir(
        kernel,
        vfs,
        home,
        keys.clientKey.private,
        knownHostsEntry(port, keys.hostKey.public),
      );

      const url = `ssh://${SSH_USER}@127.0.0.1:${port}/origin.git`;

      const cloned = await kernel.exec(git(`clone ${url} /tmp/clone`));
      expect(cloned.exitCode, cloned.stderr).toBe(0);
      const readme = new TextDecoder().decode(
        await kernel.readFile('/tmp/clone/README.md'),
      );
      expect(readme).toBe('remote ssh clone\n');
      const head = new TextDecoder().decode(
        await kernel.readFile('/tmp/clone/.git/HEAD'),
      );
      expect(head.trim()).toBe('ref: refs/heads/main');

      // Push a new commit back over the same transport.
      await kernel.writeFile('/tmp/clone/pushed.txt', 'pushed over ssh\n');
      await run(kernel, git('-C /tmp/clone add pushed.txt'));
      await run(kernel, git("-C /tmp/clone commit -m 'push over ssh'"));
      const pushed = await kernel.exec(
        git('-C /tmp/clone push origin HEAD:refs/heads/ssh-push'),
      );
      expect(pushed.exitCode, pushed.stderr).toBe(0);

      // Verify the ref really landed in the host-side bare repo.
      const originRef = spawnSync(
        'git',
        ['-C', join(repoRoot, 'origin.git'), 'rev-parse', '--verify', 'refs/heads/ssh-push'],
        { encoding: 'utf8' },
      );
      expect(originRef.status).toBe(0);
      expect(originRef.stdout.trim()).toMatch(/^[0-9a-f]{40,64}$/);
    }, 300_000);
  });
});
