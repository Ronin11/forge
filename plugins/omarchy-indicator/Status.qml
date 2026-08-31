import QtQuick
import Quickshell
import Quickshell.Io

// The Forge status file, read straight off disk. The daemon's status-file
// plugin rewrites it atomically (tmp + rename) on every relevant event and
// on a heartbeat; this component only watches, parses defensively, and
// derives staleness. Consumers read `record`, `stale`, and `state`.
Item {
  id: root
  visible: false

  // The widget's inline shell.json settings, handed down by the panel.
  property var settings: ({})

  readonly property string home: Quickshell.env("HOME") || ""

  function setting(name, fallback) {
    var value = settings ? settings[name] : undefined
    return value === undefined || value === null ? fallback : value
  }

  function expandPath(path) {
    var value = String(path || "").trim()
    if (value === "") return ""
    if (value === "~") return home
    if (value.indexOf("~/") === 0) return home + value.substring(1)
    if (value.indexOf("$HOME/") === 0) return home + value.substring(5)
    if (value.charAt(0) !== "/") return home + "/" + value
    return value
  }

  readonly property string statusFile: expandPath(String(setting("statusFile", "~/.local/state/forge/status.json")))
  readonly property int staleAfterSec: Math.max(5, Math.round(Number(setting("staleAfterSec", 30)) || 30))

  // The parsed status record, or null when the file is absent or invalid.
  property var record: null

  // Countdowns and staleness read this instead of Date.now() so both keep
  // moving while the panel sits open and the file sits still.
  property double nowMs: Date.now()

  Timer {
    interval: 1000
    running: true
    repeat: true
    onTriggered: root.nowMs = Date.now()
  }

  FileView {
    path: root.statusFile
    watchChanges: true
    printErrors: false
    onFileChanged: reload()
    onLoaded: root.parse(text())
    onLoadFailed: root.record = null
  }

  function parse(content) {
    try {
      var parsed = JSON.parse(String(content || ""))
      root.record = parsed && typeof parsed === "object" ? parsed : null
    } catch (e) {
      root.record = null
    }
  }

  // Milliseconds of the record's own timestamp, or -1 when unusable.
  readonly property double tsMs: {
    if (!record || !record.ts) return -1
    var ms = new Date(String(record.ts)).getTime()
    return isFinite(ms) ? ms : -1
  }

  // A record older than staleAfterSec means the daemon is down: the file
  // simply stops updating, so age is the only signal.
  readonly property bool stale: !record || tsMs < 0 || (nowMs - tsMs) > staleAfterSec * 1000

  readonly property string state: stale ? "stale" : String(record.state || "idle")
}
