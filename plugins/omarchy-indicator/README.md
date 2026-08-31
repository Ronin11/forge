# omarchy-indicator

Forge in the Omarchy bar. Pure QML — no daemon process of its own; it
watches the status file that the `status-file` plugin maintains at
`~/.local/state/forge/status.json`.

`forge plugin install omarchy-indicator` copies `manifest.json`, the
`*.qml` files, and this README into `~/.config/omarchy/plugins/ronin.forge/`
and puts the widget on the bar's right section, immediately before
`omarchy.agents`. `forge plugin uninstall omarchy-indicator` reverses both.
The shell hot-reloads; no restart is needed.

Files:

- `plugin.toml` — the Forge-side manifest (`forge plugin list` discovery);
  not installed into Omarchy.
- `manifest.json` — the Omarchy plugin manifest: id `ronin.forge`, a
  `bar-widget` whose entry point is `Panel.qml`, with a settings schema for
  `statusFile` and `staleAfterSec`.
- `Status.qml` — one `FileView` on the status file: defensive JSON parsing,
  a 1 s clock, and the derived `record` / `stale` / `state`.
- `Panel.qml` — the bar widget (state-colored hammer icon, running count,
  human-queue badge) and the click-open panel: Running, Needs you, Queue,
  Usage meters with target markers and reset countdowns, Last failure, and
  a footer with "Open Forge" and — when the daemon is down — "Start daemon".
