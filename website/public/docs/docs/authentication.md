# Authentication

Authenticate connections to agentOS actors using Rivet Actor connection params and hooks.

agentOS uses the same authentication system as [Rivet Actors](/docs/actors/authentication): clients send credentials as connection params, and you validate them server-side.

- Clients pass credentials in `params` when they connect.
- Validate them on the server in `onBeforeConnect` (throw to reject the connection), or extract user data into connection state with `createConnState` (read it in actions via `c.conn.state`).
- You can declare the credential shape with `agentOS<ConnParams>(...)` to document what you accept, but the client's `params` is `unknown` and is not checked against it. The real check is your hook, not the types.
- AgentOS uses ordinary Rivet actor connection hooks, so authentication runs before the connection reaches an action.

## Example

The server declares the credential shape and validates it in `onBeforeConnect` (throw to reject); the client passes credentials as `params`.

See [Actor Authentication](/docs/actors/authentication) for JWT validation, role-based access control, external auth providers, and token caching.
