# Agent OS WasmVM git compatibility

The `git` command in Agent OS is a clean-room, Apache-2.0 reimplementation of a
small git subset for WasmVM guests.

## Supported commands

- `git init`
- `git add`
- `git commit -m ...`
- `git branch`
- `git checkout`
- `git clone <local-path>`
- `git clone http://...`
- `git clone https://...`

Remote clone support is limited to unauthenticated smart-HTTP fetches. The
implementation can read from `http://` and `https://` remotes, but it does not
support credentials, push, or SSH transport.

## Unsupported commands and transports

The guest command exits with a typed `GitSubcommandUnsupported` error for:

- `git push`
- `git fetch`
- `git pull`
- `git remote`
- SSH-style clone URLs such as `git@host:owner/repo.git` and `ssh://...`
- `git://...` clone URLs
- Authenticated HTTP(S) remotes such as `https://user@host/repo.git`
- Submodules, hooks, GPG signing, and other advanced porcelain workflows

If you need behavior outside this subset, keep using host git or extend the
clean-room implementation deliberately.
