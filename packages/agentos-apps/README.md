# Dynamic Apps moved

Install `@rivet-dev/dynamic-apps` and import from that package instead:

```sh
npm remove @rivet-dev/agentos-apps
npm add @rivet-dev/dynamic-apps
```

This compatibility package throws an actionable moved-package error from both
its root and `./advanced` entry points.
