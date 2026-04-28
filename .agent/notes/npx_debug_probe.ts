import { createIntegrationKernel } from '/home/nathan/a5/registry/tests/kernel/helpers.ts';

(async () => {
  const { kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
  try {
    const result = await kernel.exec('npx -y semver 1.2.3', {
      cwd: '/',
      timeout: 20000,
      env: {
        CODEX_DEBUG_NPM_CLI: '1',
        CODEX_DEBUG_HTTP_POLYFILL: '1',
      },
    });
    console.log(JSON.stringify(result));
  } finally {
    await dispose();
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
