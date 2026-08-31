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

## Documentation

- [DESIGN.md](docs/DESIGN.md) — Architecture and system design
- [CONSTITUTION.md](docs/CONSTITUTION.md) — Fixed principles
- [MODES.md](docs/MODES.md) — Agent mode specifications
- [PLUGINS.md](docs/PLUGINS.md) — Plugin system
- [STYLE.md](docs/STYLE.md) — Code standards
- [VERIFICATION.md](docs/VERIFICATION.md) — Verification levels
- [SMOKE.md](docs/SMOKE.md) — Smoke tests
