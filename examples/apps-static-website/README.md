# agentOS Apps: Static Website

A directory with `index.html` and no `package.json` is served directly. CSS,
JavaScript, SVG, and other byte assets are included in the immutable release.
A package with a build script is treated as a built static site when it emits
`dist/index.html`.

Run the checked workspace example with Node.js 22 or newer:

```sh
pnpm --dir examples/apps-static-website start
# In another terminal:
curl -X POST http://localhost:3000/deploy/static-website
```

RivetKit starts its local Engine automatically. Against an existing Rivet
deployment, use the standard Rivet connection variables instead. Open
`http://localhost:3000/apps/static-website/`.
