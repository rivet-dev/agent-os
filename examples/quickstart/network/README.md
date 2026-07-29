---
title: "Network"
description: "Start an HTTP server inside the VM and call it with vm.network.httpRequest()."
category: "Quickstart"
order: 6
---

# Network

Run a real HTTP server inside the VM and call it from your host code. Reach for this when guest code needs to expose a port or you want to drive an in-VM service over HTTP.

## How it works

Create a VM with `network` and `childProcess` permissions, then `filesystem.writeFile` a small Node server script into the VM. `vm.process.spawn` launches it, and the server prints its bound port on stdout, which an `onProcessOutput` subscription parses out. With the port in hand, `vm.network.httpRequest({ port, path })` routes a buffered request to that in-VM server over localhost and returns the serializable `HttpResponse` DTO. Cleanup waits briefly on the process and disposes the VM.

> Preview URLs (`agent.createPreviewUrl`) live only in the RivetKit actor wrapper, not the direct VM API — see `examples/networking/`.

## Run it

```sh
npm install
npx tsx index.ts
# Logs the server's port, then "Response: { status: 'ok', method: 'GET', url: '/api/test' }"
```

## Source

View the source on GitHub: https://github.com/rivet-dev/agent-os/tree/main/examples/quickstart/network
