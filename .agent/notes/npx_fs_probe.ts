import { mkdtemp, rm } from 'node:fs/promises';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { existsSync } from 'node:fs';
import {
  COMMANDS_DIR,
  createKernel,
  createNodeRuntime,
  createWasmVmRuntime,
  NodeFileSystem,
} from '/home/nathan/a5/registry/tests/kernel/helpers.ts';

(async () => {
  const tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-npx-fs-probe-'));
  console.log('TEMP', tempDir);
  const vfs = new NodeFileSystem({ root: tempDir });
  const kernel = createKernel({ filesystem: vfs, cwd: '/' });
  await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
  await kernel.mount(createNodeRuntime());

  try {
    const result = await kernel.exec('npx -y semver 1.2.3', { cwd: '/', timeout: 20000 });
    console.log('RESULT', JSON.stringify(result));
    console.log('HAS_NODE_MODULES', existsSync(path.join(tempDir, 'node_modules')));
    console.log('HAS_HOME_NPM', existsSync(path.join(tempDir, 'home', 'user', '.npm')));
  } finally {
    await kernel.dispose();
    // leave temp dir for inspection on failure
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
