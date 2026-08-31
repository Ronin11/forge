# Forge

Forge is a local-first system that runs coding agents against local Git repositories. It measures everything agents do and uses those measurements to self-improve via proposals — refining its own prompts, tools, documentation, and processes.

One Go binary, one SQLite database, one machine, one developer.

## Building

```bash
just build
```

## Running

```bash
forge daemon
```

## Plugins

Integrations — bars, notifiers, GitHub intake, extra MCP tools — live outside the
core as separate programs that speak Forge's wire contract over its Unix socket.
Repo-embedded plugins install with `forge plugin install <name>`.

Your own customizations stay in your own directories: list them under
`plugin_dirs` in `~/.forge/config.toml` and the daemon discovers, starts, and
lists them with no change to a Forge checkout.

```toml
# ~/.forge/config.toml
plugin_dirs = ["~/.config/forge/plugins"]
```

See [PLUGINS.md](docs/PLUGINS.md) for the wire contract (the public plugin API)
and the out-of-tree workflow. Plugins consume the contract and never import
Forge's Go packages.

## Documentation

- [DESIGN.md](docs/DESIGN.md) — Architecture and system design
- [CONSTITUTION.md](docs/CONSTITUTION.md) — Fixed principles
- [MODES.md](docs/MODES.md) — Agent mode specifications
- [PLUGINS.md](docs/PLUGINS.md) — Plugin system
- [STYLE.md](docs/STYLE.md) — Code standards
- [VERIFICATION.md](docs/VERIFICATION.md) — Verification levels
- [SMOKE.md](docs/SMOKE.md) — Smoke tests
