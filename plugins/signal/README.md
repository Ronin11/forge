# forge-signal

A bidirectional Signal bridge for Forge.

- **Outbound:** sends you a Signal message when a task needs input (a question),
  a target fails/goes unverified, or a proposal is filed. Toggle each in config.
- **Inbound:** turns your Signal messages into work.
  - Any plain message → files a task on `default_repo`.
  - `/task <text>` → the same, explicitly.
  - `/answer <task-id> <text>` → answers a waiting question (id from the
    notification, e.g. `3f69f96a`).
  - `/status` → a one-line queue summary. `/help` → the command list.

Only messages from the configured `recipient` are honored.

## One-time setup (out of band)

This plugin shells out to [`signal-cli`](https://github.com/AsamK/signal-cli);
install it and register or link an account first:

```sh
# Arch: `paru -S signal-cli`, or download a release. Then either register a new
# number (needs SMS/voice verification) …
signal-cli -a +15551234567 register
signal-cli -a +15551234567 verify 123-456
# … or link this as a second device to your phone's Signal:
signal-cli -a +15551234567 link -n forge   # scan the QR with Signal → Linked devices
```

Verify sending works before enabling the plugin:
`signal-cli -a +15551234567 send -m hi +15559876543`

## Config — `<FORGE_PLUGIN_DIR>/signal.toml`

```toml
[signal]
account      = "+15551234567"   # the number Forge sends FROM (registered above)
recipient    = "+15559876543"   # you — notified, and the only sender honored
default_repo = "equitizr"       # repo a bare message files a task on
# signal_cli  = "signal-cli"    # path/name; default "signal-cli"
# poll_seconds = 10             # inbound receive cadence
# questions = true  failures = true  proposals = true  throttling = true
# intake = true                 # set false to disable inbound commands
```

`account` and `recipient` are required; without them the plugin idles (it does
not crash-loop) until you fill them in and restart it.

## Install

`forge plugin install signal`, then edit `signal.toml` and
`forge plugin disable signal && forge plugin enable signal` (or restart the daemon).
