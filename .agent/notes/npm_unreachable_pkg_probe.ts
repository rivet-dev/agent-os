import { createIntegrationKernel } from '/home/nathan/a5/registry/tests/kernel/helpers.ts';

(async () => {
  const { kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
  try {
    await kernel.writeFile('/package.json', JSON.stringify({
      name: 'test-npm-no-network',
      private: true,
      dependencies: { 'left-pad': '1.3.0' },
    }));
    const result = await kernel.exec('npm install --registry=http://localhost:1', {
      cwd: '/',
      timeout: 15000,
    });
    console.log(JSON.stringify(result));
  } finally {
    await dispose();
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
