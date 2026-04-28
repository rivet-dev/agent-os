import { createIntegrationKernel } from '/home/nathan/a5/registry/tests/kernel/helpers.ts';

(async () => {
  const { kernel, dispose } = await createIntegrationKernel({ runtimes: ['wasmvm', 'node'] });
  try {
    const script = [
      "const https=require('https');",
      "const { getAgent } = require('/__agentos/node-runtime/npx/node_modules/@npmcli/agent/lib/index.js');",
      "const agent = getAgent('https://registry.npmjs.org/', { timeout: 3000 });",
      "const req = https.get('https://registry.npmjs.org/', { agent }, (res) => {",
      "  console.log('STATUS', res.statusCode);",
      "  res.resume();",
      "  res.on('end', () => process.exit(0));",
      "});",
      "req.on('socket', (socket) => {",
      "  console.error('SOCKET', JSON.stringify({ connecting: socket.connecting, secureConnecting: socket.secureConnecting, authorized: socket.authorized, encrypted: socket.encrypted }));",
      "  socket.on('connect', () => console.error('SOCKET_CONNECT'));",
      "  socket.on('secureConnect', () => console.error('SOCKET_SECURE_CONNECT'));",
      "  socket.on('timeout', () => console.error('SOCKET_TIMEOUT'));",
      "  socket.on('close', () => console.error('SOCKET_CLOSE'));",
      "  socket.on('error', (e) => console.error('SOCKET_ERROR', e && e.code, e && e.message));",
      "});",
      "req.on('response', () => console.error('REQ_RESPONSE'));",
      "req.on('timeout', () => console.error('REQ_TIMEOUT'));",
      "req.on('close', () => console.error('REQ_CLOSE'));",
      "req.on('error', (e) => { console.error('REQ_ERROR', e && e.code, e && e.message); process.exit(1); });",
    ].join('\n');
    await kernel.writeFile('/probe.js', script);
    const result = await kernel.exec('node /probe.js', { cwd: '/', timeout: 10000 });
    console.log(JSON.stringify(result));
  } finally {
    await dispose();
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
