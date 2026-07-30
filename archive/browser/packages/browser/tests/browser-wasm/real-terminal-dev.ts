// Vite dev entry for the manual real-brush-shell test. Pulls xterm's stylesheet
// through Vite (the shared entry must stay CSS-free for the esbuild gate build),
// runs the real entry (which builds the terminal, boots the real `sh` wasm on a
// kernel PTY, and exposes window.__realTerminal), then auto-boots so you can type
// immediately. Real brush + real coreutils, all inside the agentOS VM.
import "@xterm/xterm/css/xterm.css";
import "./real-terminal.entry.ts";

void window.__realTerminal?.start();
