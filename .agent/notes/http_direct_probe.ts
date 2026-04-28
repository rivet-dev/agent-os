import { createIntegrationKernel } from '/home/nathan/a5/registry/tests/kernel/helpers.ts';

(async () => {
  const { kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
  try {
    const script = "const https=require('https'); https.get('https://registry.npmjs.org/', (res)=>{ console.log('STATUS', res.statusCode); res.resume(); res.on('end', ()=>process.exit(0)); }).on('error', (e)=>{ console.error('ERR', e && e.code, e && e.message); process.exit(1); });";
    const result = await kernel.exec(`node -e ${JSON.stringify(script)}`, { cwd: '/', timeout: 10000 });
    console.log(JSON.stringify(result));
  } finally {
    await dispose();
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
