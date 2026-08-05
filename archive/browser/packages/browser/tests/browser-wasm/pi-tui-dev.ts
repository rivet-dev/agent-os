// Vite dev entry for manual pi-TUI testing. Pulls xterm's stylesheet through
// Vite (the esbuild bundle used by the Playwright gate gets its CSS a different
// way, so the shared entry must not import CSS), then runs the real entry, which
// builds the terminal, wires the PTY, and exposes window.__piTui.
//
// Unlike the gate, the dev page ALWAYS auto-boots pi so you can type immediately:
//   - default              -> real Chrome window.LanguageModel (honest error if absent)
//   - ?mockModel=1          -> labeled placeholder model (no real LLM)
import "@xterm/xterm/css/xterm.css";
import "./pi-tui.entry.ts";

void window.__piTui?.start();
