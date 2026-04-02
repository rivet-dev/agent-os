# Browser Playground Example

This example provides a small in-browser playground with:

- Monaco for editing code
- Agent OS browser runtime for TypeScript execution
- the sandbox-agent inspector dark theme

Run it from the repo:

```bash
pnpm -C packages/playground dev
```

Then open:

```text
http://localhost:4173/
```

Notes:

- `pnpm run setup-vendor` symlinks Monaco and TypeScript from `node_modules` into `vendor/` (runs automatically before `dev` and `build`).
- The dev server sets COOP/COEP headers required for SharedArrayBuffer and serves all assets from the local filesystem.
