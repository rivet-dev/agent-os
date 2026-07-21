# Models & Credentials

Choose agent models and pass provider credentials to sessions securely.

- Choose the model through your **agent adapter**.
- Pass provider credentials from trusted server code.
- Credentials are injected at session creation and can be isolated per tenant.
- The VM does **not** inherit the host `process.env` — keys must be passed
  explicitly.

## Passing API keys

Pass LLM provider keys via the `env` option on `openSession`.

## Per-tenant credentials

- Key `getOrCreate` on the tenant id → isolated VM per tenant.
- Look up the tenant's key on the server, inject via session `env`.
- Keys stay on the server and never reach the client, and one tenant's key
  never reaches another.

Declare the agent software on the server:

Resolve each tenant's key and pass it at session creation:

## Models

- Model selection belongs to the configured agent adapter.
- AgentOS forwards the session environment and preserves the agent's native
  model behavior — no second model-selection layer.
- See your [agent](/docs/agents/pi) page for supported models and providers.