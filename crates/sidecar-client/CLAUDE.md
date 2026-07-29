See `../CLAUDE.md` for crate-wide runtime and testing rules.

## Local Patterns

- Keep this crate Agent OS-agnostic: no `agentos-protocol`, `agentos-client`, `agentos-sidecar`, ACP, agents, sessions, or binding semantics.
- The generic transport resolves `AGENTOS_SIDECAR_BIN` / `agentos-native-sidecar`; product wrappers such as Agent OS must resolve their own wrapper binary and pass it explicitly.
- Expose raw agentos wire types and transport primitives only; ergonomic product facades belong in product-specific client crates.
