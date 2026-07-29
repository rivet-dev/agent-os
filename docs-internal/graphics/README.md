# Launch / marketing graphics

Source for internally-generated agentOS marketing graphics (launch heroes,
social images, code-snippet cards). **Only the source lives here** — rendered
PNGs are written to `~/tmp/agentos-graphics/` and are intentionally *not*
committed.

## Generators

| Script | Output | Notes |
| --- | --- | --- |
| `render-launch.mjs` | `launch.png` (2048×1024) | "agentOS Apps" launch/social hero: wordmark lockup + V8 / SQLite / Rivet-Actors app tiles + PREVIEW pill. |
| `render-technical.mjs` | `technical.png` (2048×auto) | Titled header + syntax-highlighted steps from `snippets.json`. |

```bash
# writes to ~/tmp/agentos-graphics/ by default (pass a dir to override)
node docs-internal/graphics/render-launch.mjs
node docs-internal/graphics/render-technical.mjs
```

## How it stays reproducible

- **Fonts:** Manrope + JetBrains Mono are read live from `website/public/fonts/`.
- **`assets/`** (vendored, committed):
  - `v8.svg` — official V8 logo (`v8.dev/_img/v8.svg`).
  - `sqlite.svg` — SQLite logo, color (vectorlogo.zone icon).
  - `actors.svg` — Rivet Actors mark (from the `rivet` repo), recolored to ink at render time.
  - `NimbusSans-Bold.otf` — URW Helvetica clone. The agentOS wordmark's "OS"
    is a live `<text>` in Helvetica; the site renders it in the viewer's system
    Helvetica, so we embed this clone to render it faithfully off a Mac.
- **playwright** is resolved from the repo's `node_modules` and drives system
  Chromium (`/usr/bin/chromium`).
- **shiki** (`render-technical.mjs` only) is a dev-only highlighter, **not** a
  repo dependency. Resolve order: repo `node_modules` → `SHIKI_DIR=/path/to/node_modules/shiki`.
  If absent: `pnpm add -D shiki`.

## Design decisions (so future edits stay on-brand)

- Background is the site's paper surface `#EFEFEF`; text is ink `#1B1916`
  (`tailwind.config` / `global.css`).
- "Apps" is **Manrope Semibold**, tracking `-0.015em` — the landing page's
  `HERO_H1_CLASS` heading treatment — sized as a supporting label, not competing
  with the wordmark. It's optically centered on the "OS" badge at render time.
- Logo tiles copy the landing page `FLOATING_AGENTS` treatment: rounded-square,
  white→`#f1f1f3` gradient, `ring-ink/10`, layered shadow, individual rotation,
  V8 dominant in the middle.
- Twitter shows 2:1 uncropped, so the canvas is 2048×1024.
