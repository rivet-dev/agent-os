import { createIntegrationKernel } from '/home/nathan/a5/registry/tests/kernel/helpers.ts';

(async () => {
  const { kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
  try {
    await kernel.writeFile('/probe.js', [
      "const { createRequire } = require('module');",
      "const npmRequire = createRequire('/__agentos/node-runtime/npx/lib/npm.js');",
      "try {",
      "  console.log('RES1', require.resolve('@npmcli/agent'));",
      "} catch (e) { console.error('ERR1', e && e.message); }",
      "try {",
      "  console.log('RES2', require.resolve('@npmcli/agent', { paths: ['/__agentos/node-runtime/npx'] }));",
      "} catch (e) { console.error('ERR2', e && e.message); }",
      "try {",
      "  console.log('RES3', require.resolve('@npmcli/agent', { paths: ['/__agentos/node-runtime/npx/lib'] }));",
      "} catch (e) { console.error('ERR3', e && e.message); }",
      "try {",
      "  console.log('RES4', require.resolve('make-fetch-happen', { paths: ['/__agentos/node-runtime/npx/lib'] }));",
      "} catch (e) { console.error('ERR4', e && e.message); }",
      "try {",
      "  console.log('RES5', npmRequire.resolve('@npmcli/agent'));",
      "} catch (e) { console.error('ERR5', e && e.message); }",
      "try {",
      "  console.log('RES6', npmRequire.resolve('make-fetch-happen'));",
      "} catch (e) { console.error('ERR6', e && e.message); }",
    ].join('\n'));
    const result = await kernel.exec('node /probe.js', { cwd: '/', timeout: 5000 });
    console.log(JSON.stringify(result));
  } finally {
    await dispose();
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
