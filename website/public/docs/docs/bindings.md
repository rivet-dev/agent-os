# Custom Bindings

Expose custom host functions to agents as CLI commands inside the VM.

Expose your host JavaScript functions (defined with Zod input schemas) to agents as auto-generated CLI commands installed at `/usr/local/bin/agentos-{name}` inside the VM, injected into the agent's [system prompt](/docs/system-prompt) and callable inside scripts for code-mode token savings.

## Getting started

Define a bindings group with Zod input schemas and pass it to `agentOS()`. Each binding becomes a CLI command inside the VM.

Each binding can set an explicit `timeout` (in milliseconds) for long-running work. Bindings run without a timeout unless one is set.

### Zod to CLI mapping

Zod schema fields are converted to CLI flags automatically. Field names are converted from camelCase to kebab-case.

| Zod type | CLI syntax | Example |
|---|---|---|
| `z.string()` | `--name value` | `--path /tmp/out.png` |
| `z.number()` | `--name 42` | `--limit 5` |
| `z.boolean()` | `--flag` / `--no-flag` | `--full-page` |
| `z.enum(["a","b"])` | `--name a` | `--format json` |
| `z.array(z.string())` | `--name a --name b` | `--tags foo --tags bar` |

Optional fields (via `.optional()`) become optional flags. Required fields are enforced at validation time. Use `.describe()` on Zod fields to generate useful `--help` output.

### What the agent sees

When bindings are registered, CLI shims are installed at `/usr/local/bin/agentos-{name}` inside the VM and the binding list is injected into the agent's [system prompt](/docs/system-prompt), so keep binding descriptions concise to save tokens.

The agent interacts with bindings as shell commands:

```bash
# List all available binding collections
agentos list-bindings

# List bindings in a specific group
agentos list-bindings weather

# Get help for a binding
agentos-weather forecast --help

# Call a binding with flags
agentos-weather forecast --city Paris --days 3

# Call a binding with inline JSON
agentos-weather forecast --json '{"city":"Paris","days":3}'

# Call a binding with JSON from a file
agentos-weather forecast --json-file /tmp/input.json
```

On success, the binding exits `0` and writes a JSON envelope to stdout:

```json
{"ok":true,"result":{"temperature":22,"condition":"sunny"}}
```

On failure (validation or execution error), the binding exits non-zero and writes the error message to stderr:

```text
Missing required flag: --city
```

## Bindings and MCP servers

AgentOS supports two ways to give agents access to external functionality: **bindings** and session-scoped **MCP servers**. Both work, but they have different tradeoffs.

|  | Bindings | MCP Servers |
|---|---|---|
| **How it works** | Call JavaScript functions on the host directly | Connect to a standard MCP server |
| **Authentication** | None required. Direct binding to the agent's OS. | Requires custom auth configuration per server |
| **Code mode** | Built-in. Bindings are exposed as CLI commands, so agents can call them inside scripts for up to 80% token reduction. | Requires extra work to make code mode work out of the box |
| **Latency** | Near-zero. Bound directly to the host process. | Extra network hop to reach the MCP server |
| **Setup** | Define bindings in your actor code with Zod schemas | Configure any standard MCP server |

Use bindings when you want to expose your own JavaScript functions to agents. Use MCP servers when you want to connect to existing third-party services. See [Sessions](/docs/sessions#open-a-session) for MCP server configuration.

## Security

Binding calls from the agent securely invoke your `execute()` functions on the host. Your functions run with full access to the host environment, so you can call databases, APIs, and services directly without proxying credentials into the VM. The agent never sees the credentials, it only sees the binding's input/output contract.

Bindings run on the host with full access to the host environment, so do not expose bindings that could compromise the host without appropriate safeguards.