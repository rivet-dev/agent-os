# Networking & Previews

Proxy HTTP requests into agentOS VMs and create shareable preview URLs.

Proxy buffered HTTP requests into VM services with `httpRequest` and create time-limited, actor-only preview URLs (with configurable expiration, revocation, and CORS), all carried over one transport (the kernel socket table) that is loopback-only by default under a three-layer confinement model.

## Run an HTTP server in the VM

Guest code runs a normal Node HTTP server: it binds a loopback port inside the VM exactly like any Node process. Write the server file and spawn it.

## Fetch from a VM service

With the HTTP server running in the VM (above), send requests to it with `httpRequest`, including custom methods, headers, and body.

## Preview URLs

Preview URLs are port forwarding for VM services: a time-limited, public URL that proxies HTTP to a port inside the VM, for browser or external access (use `httpRequest` for server-to-server). Preview creation is intentionally actor-only through the native `createPreviewUrl` and `expirePreviewUrl` actions; tokens survive sleep/wake and CORS is enabled. See [Security](/docs/security-model) for details.

### Create a preview URL

Token lifetimes are configured under the `preview` key:

### Revoke a preview URL

Mint short-lived preview tokens so access expires automatically; the lifetime is capped by `preview.maxExpiresInSeconds`.

## Permissions

Network access is governed by the VM permission policy. By default the guest cannot reach the network; grant it, or allow only specific destinations:

```ts
const vm = agentOS({
  permissions: {
    network: {
      default: "deny",
      rules: [{ mode: "allow", operations: ["*"], patterns: ["api.example.com"] }],
    },
  },
});
```

See [Permissions](/docs/permissions) for the full configuration.