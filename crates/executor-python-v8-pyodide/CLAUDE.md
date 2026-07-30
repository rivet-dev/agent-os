# Python V8/Pyodide executor

This crate owns Pyodide context setup, Python execution, and Python-specific
guest adaptation over `agentos-executor-v8-runtime`.

- Do not duplicate kernel filesystem, process, signal, network, or TTY state.
- Reusable V8 mechanics belong in `agentos-executor-v8-runtime`.
- Cross-executor contracts belong in `agentos-executor-contract`.
