import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { COMMANDS_DIR, createKernel, NodeFileSystem, createWasmVmRuntime, createNodeRuntime } from '../helpers.ts';

const tempDir = await mkdtemp(path.join(tmpdir(), 'kernel-npm-install-repro-'));
console.log('tempDir', tempDir);
try {
  await writeFile(path.join(tempDir, 'package.json'), JSON.stringify({name:'test-npm-install', private:true, dependencies:{'left-pad':'1.3.0'}}));
  const vfs = new NodeFileSystem({ root: tempDir });
  const kernel = createKernel({ filesystem: vfs, cwd: '/' });
  await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));
  await kernel.mount(createNodeRuntime());
  try {
    const installResult = await kernel.exec('npm install', { cwd: '/' });
    console.log('exitCode', installResult.exitCode);
    console.log('stdout >>>\n' + installResult.stdout + '\n<<<');
    console.log('stderr >>>\n' + installResult.stderr + '\n<<<');
  } finally {
    await kernel.dispose();
  }
} finally {
  await rm(tempDir, { recursive: true, force: true });
}
