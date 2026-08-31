import QtQuick
import QtQuick.Controls
import Quickshell
import qs.Commons
import qs.Ui

// Forge in the bar: one icon whose color follows the daemon state, a count
// of running attempts, a badge with the human-queue total, and a panel with
// what is running, what needs you, the queue, both budget windows, and the
// last failure. All data comes from Status.qml; every section tolerates a
// missing or partial record.
Panel {
  id: root
  moduleName: "ronin.forge"

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  // The palette has no warning role, so throttled blends urgent toward the
  // foreground — related to the alarm color without shouting like it.
  readonly property color warning: Qt.rgba((urgent.r + foreground.r) / 2,
                                           (urgent.g + foreground.g) / 2,
                                           (urgent.b + foreground.b) / 2, 1)
  readonly property color track: Style.selectedFillFor(foreground, Color.accent)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  // md-hammer and md-power-plug-off from the shell's nerd font.
  readonly property string hammerGlyph: "󰣪"
  readonly property string offGlyph: "󰚦"

  readonly property var rec: status.record

  function num(v) {
    var n = Number(v)
    return isFinite(n) ? n : 0
  }

  readonly property var running: rec && Array.isArray(rec.running) ? rec.running : []
  readonly property var humanItems: rec && rec.human_queue && Array.isArray(rec.human_queue.items)
    ? rec.human_queue.items : []
  readonly property int humanCount: rec && rec.human_queue
    ? num(rec.human_queue.questions) + num(rec.human_queue.proposals) + num(rec.human_queue.verifications)
    : 0

  readonly property color stateColor: {
    if (status.state === "working") return Color.accent
    if (status.state === "attention") return urgent
    if (status.state === "throttled") return warning
    return dim // idle, and stale's "off" presentation
  }

  function openUrl(url) {
    if (!url) return
    Quickshell.execDetached(["omarchy", "launch", "browser", String(url)])
    root.close()
  }

  function answerItem(id) {
    if (!id) return
    Quickshell.execDetached(["omarchy-launch-floating-terminal-with-presentation",
                             "forge", "task", "answer", String(id)])
    root.close()
  }

  function approveItem(id) {
    if (!id) return
    Quickshell.execDetached(["forge", "proposal", "approve", String(id)])
  }

  function startDaemon() {
    Quickshell.execDetached(["forge", "daemon", "start"])
    root.close()
  }

  // Countdowns run against the record's own timestamp: resets_in_s was true
  // when the file was written, so the time since then comes off the clock.
  function resetsInMs(win) {
    if (!win || status.tsMs < 0) return -1
    return num(win.resets_in_s) * 1000 - Math.max(0, status.nowMs - status.tsMs)
  }

  function formatDuration(ms) {
    if (!(ms > 0)) return "now"
    var minutes = Math.floor(ms / 60000)
    var hours = Math.floor(minutes / 60)
    var days = Math.floor(hours / 24)
    if (days > 0) return days + "d " + (hours % 24) + "h"
    if (hours > 0) return hours + "h " + (minutes % 60) + "m"
    return Math.max(1, minutes) + "m"
  }

  function formatElapsed(seconds) {
    var s = Math.max(0, Math.round(num(seconds)))
    var m = Math.floor(s / 60)
    var h = Math.floor(m / 60)
    if (h > 0) return h + "h " + (m % 60) + "m"
    if (m > 0) return m + "m " + (s % 60) + "s"
    return s + "s"
  }

  function formatTokens(n) {
    if (n >= 1e9) return (n / 1e9).toFixed(1) + "B"
    if (n >= 1e6) return (n / 1e6).toFixed(1) + "M"
    if (n >= 1e3) return (n / 1e3).toFixed(1) + "K"
    return String(n)
  }

  function runningMeta(item) {
    if (!item) return ""
    var parts = []
    if (item.routine) parts.push(String(item.routine))
    if (item.repo) parts.push(String(item.repo))
    if (item.mode) parts.push(String(item.mode))
    var meta = parts.join(" · ")
    if (item.phase) meta += (meta === "" ? "" : " — ") + String(item.phase)
    return meta
  }

  function runningStats(item) {
    if (!item) return ""
    var parts = [formatElapsed(item.elapsed_s)]
    if (num(item.tokens) > 0) parts.push(formatTokens(num(item.tokens)))
    if (item.model) parts.push(String(item.model))
    return parts.join(" · ")
  }

  function stateLabel() {
    if (status.state === "stale") return "DAEMON DOWN"
    var label = status.state.toUpperCase()
    if (root.running.length > 0) label += " · " + root.running.length + " RUNNING"
    return label
  }

  Status {
    id: status
    settings: root.settings
  }

  // ------------------------------------------------------------ bar widget

  visible: true
  implicitWidth: barContent.implicitWidth
  implicitHeight: iconSlot.implicitHeight

  Row {
    id: barContent
    spacing: 0

    Item {
      id: iconSlot
      implicitWidth: button.implicitWidth
      implicitHeight: button.implicitHeight
      width: implicitWidth
      height: implicitHeight

      BarIconButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        text: root.stale ? root.offGlyph : root.hammerGlyph
        foreground: root.stateColor
        useActiveColor: false
        tooltipText: root.stale ? "Forge — daemon down" : "Forge"
        onPressed: root.toggle()
      }

      Rectangle {
        visible: root.humanCount > 0
        anchors.top: parent.top
        anchors.right: parent.right
        anchors.topMargin: Style.space(2)
        width: Math.max(height, badgeText.implicitWidth + Style.space(5))
        height: badgeText.implicitHeight + Style.space(2)
        radius: height / 2
        color: root.urgent

        Text {
          id: badgeText
          anchors.centerIn: parent
          text: root.humanCount > 9 ? "9+" : String(root.humanCount)
          color: Color.background
          font.family: root.fontFamily
          font.pixelSize: Math.max(8, Style.font.caption - 2)
          font.bold: true
        }
      }
    }

    Text {
      visible: root.running.length > 0
      anchors.verticalCenter: parent.verticalCenter
      text: String(root.running.length)
      rightPadding: Style.space(5)
      color: root.stateColor
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      font.bold: true
    }
  }

  // ----------------------------------------------------------------- panel

  KeyboardPanel {
    id: panel
    anchorItem: iconSlot
    owner: root
    bar: root.bar
    open: root.opened
    contentWidth: panel.fittedContentWidth(Style.space(340))
    contentHeight: panel.fittedContentHeight(column.implicitHeight, Style.space(560))

    Flickable {
      id: panelFlick
      anchors.fill: parent
      contentWidth: width
      contentHeight: column.implicitHeight
      clip: true
      boundsBehavior: Flickable.StopAtBounds
      flickableDirection: Flickable.VerticalFlick
      interactive: contentHeight > height
      ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

      Column {
        id: column
        width: panelFlick.width
        spacing: Style.space(12)

        PanelHero {
          width: parent.width
          title: "Forge"
          meta: root.stateLabel()
          foreground: root.foreground
          fontFamily: root.fontFamily

          iconComponent: Component {
            Text {
              text: root.stale ? root.offGlyph : root.hammerGlyph
              color: root.stateColor
              font.family: root.fontFamily
              font.pixelSize: Style.font.display
            }
          }
        }

        Text {
          visible: root.stale
          width: parent.width
          text: "The status file has gone stale — the daemon looks down."
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }

        // ---------- Running ----------
        PanelSeparator {
          visible: runningSection.visible
          foreground: root.foreground
        }

        Column {
          id: runningSection
          visible: root.running.length > 0
          width: parent.width
          spacing: Style.spacing.md

          PanelSectionHeader {
            text: "RUNNING"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Repeater {
            model: root.running

            Item {
              id: runRow
              required property var modelData
              width: runningSection.width
              implicitHeight: runColumn.implicitHeight + Style.spacing.lg

              Rectangle {
                anchors.fill: parent
                radius: Style.cornerRadius
                color: runHover.containsMouse
                  ? Style.hoverFillFor(root.foreground, Color.accent)
                  : Qt.rgba(root.foreground.r, root.foreground.g, root.foreground.b, 0.05)
              }

              Column {
                id: runColumn
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: Style.space(8)
                anchors.rightMargin: Style.space(8)
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.space(2)

                Item {
                  width: parent.width
                  implicitHeight: Math.max(runTitle.implicitHeight, runStats.implicitHeight)

                  Text {
                    id: runTitle
                    text: runRow.modelData && runRow.modelData.title ? String(runRow.modelData.title) : "(untitled)"
                    color: root.foreground
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.body
                    elide: Text.ElideRight
                    anchors.left: parent.left
                    anchors.right: runStats.left
                    anchors.rightMargin: Style.spacing.sm
                    anchors.verticalCenter: parent.verticalCenter
                  }

                  Text {
                    id: runStats
                    text: root.runningStats(runRow.modelData)
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                  }
                }

                Text {
                  width: parent.width
                  visible: text !== ""
                  text: root.runningMeta(runRow.modelData)
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  elide: Text.ElideRight
                }
              }

              MouseArea {
                id: runHover
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.openUrl(runRow.modelData ? runRow.modelData.url : "")
              }
            }
          }
        }

        // ---------- Needs you ----------
        PanelSeparator {
          visible: needsSection.visible
          foreground: root.foreground
        }

        Column {
          id: needsSection
          visible: root.humanItems.length > 0
          width: parent.width
          spacing: Style.spacing.md

          PanelSectionHeader {
            text: "NEEDS YOU"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Repeater {
            model: root.humanItems

            Column {
              id: needRow
              required property var modelData
              readonly property string kind: modelData && modelData.kind ? String(modelData.kind) : ""
              readonly property string itemId: modelData && modelData.id ? String(modelData.id) : ""
              readonly property string itemUrl: modelData && modelData.url ? String(modelData.url) : ""

              width: needsSection.width
              spacing: Style.spacing.xs

              Item {
                width: parent.width
                implicitHeight: Math.max(needTitle.implicitHeight, needAge.implicitHeight)

                Text {
                  id: needTitle
                  text: (needRow.kind !== "" ? needRow.kind + ": " : "")
                    + (needRow.modelData && needRow.modelData.title ? String(needRow.modelData.title) : "(untitled)")
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.body
                  elide: Text.ElideRight
                  anchors.left: parent.left
                  anchors.right: needAge.left
                  anchors.rightMargin: Style.spacing.sm
                  anchors.verticalCenter: parent.verticalCenter
                }

                Text {
                  id: needAge
                  text: needRow.modelData && root.num(needRow.modelData.age_s) > 0
                    ? root.formatDuration(root.num(needRow.modelData.age_s) * 1000) : ""
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  anchors.right: parent.right
                  anchors.verticalCenter: parent.verticalCenter
                }
              }

              Row {
                spacing: Style.spacing.md

                Button {
                  visible: needRow.kind === "question" && needRow.itemId !== ""
                  text: "Answer"
                  bordered: true
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  fontSize: Style.font.bodySmall
                  onClicked: root.answerItem(needRow.itemId)
                }

                Button {
                  visible: needRow.kind === "proposal" && needRow.itemId !== ""
                  text: "Approve"
                  bordered: true
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  fontSize: Style.font.bodySmall
                  onClicked: root.approveItem(needRow.itemId)
                }

                Button {
                  visible: needRow.itemUrl !== ""
                  text: "Open"
                  bordered: true
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  fontSize: Style.font.bodySmall
                  onClicked: root.openUrl(needRow.itemUrl)
                }
              }
            }
          }
        }

        // ---------- Queue ----------
        PanelSeparator {
          visible: queueRow.visible
          foreground: root.foreground
        }

        Text {
          id: queueRow
          visible: !!root.rec
          width: parent.width
          text: root.rec
            ? "Queued " + root.num(root.rec.queued)
              + " · Blocked " + root.num(root.rec.blocked)
              + " · Deferred " + root.num(root.rec.deferred)
            : ""
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
        }

        // ---------- Usage ----------
        PanelSeparator {
          visible: usageSection.visible
          foreground: root.foreground
        }

        Column {
          id: usageSection
          visible: !!root.rec && !!root.rec.usage
          width: parent.width
          spacing: Style.space(10)

          PanelSectionHeader {
            text: "USAGE"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          UsageRow {
            width: parent.width
            title: "Session (5h)"
            win: root.rec && root.rec.usage ? root.rec.usage.five_hour : null
          }

          UsageRow {
            width: parent.width
            title: "Weekly (7d)"
            win: root.rec && root.rec.usage ? root.rec.usage.seven_day : null
          }
        }

        // ---------- Last failure ----------
        PanelSeparator {
          visible: failureSection.visible
          foreground: root.foreground
        }

        Column {
          id: failureSection
          visible: !!root.rec && !!root.rec.last_failure
          width: parent.width
          spacing: Style.spacing.xs

          readonly property var failure: root.rec ? root.rec.last_failure : null

          PanelSectionHeader {
            text: "LAST FAILURE"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Text {
            width: parent.width
            text: {
              var f = failureSection.failure
              if (!f) return ""
              var parts = []
              if (f.routine) parts.push(String(f.routine))
              if (f.reason) parts.push(String(f.reason))
              return parts.length > 0 ? parts.join(" — ") : "(unknown)"
            }
            color: root.urgent
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            elide: Text.ElideRight

            MouseArea {
              anchors.fill: parent
              cursorShape: Qt.PointingHandCursor
              onClicked: root.openUrl(failureSection.failure ? failureSection.failure.url : "")
            }
          }
        }

        // ---------- Footer ----------
        PanelSeparator {
          foreground: root.foreground
        }

        Row {
          spacing: Style.spacing.md

          Button {
            visible: !!root.rec && !!root.rec.ui
            text: "Open Forge"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            onClicked: root.openUrl(root.rec ? root.rec.ui : "")
          }

          Button {
            visible: root.stale
            text: "Start daemon"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            onClicked: root.startDaemon()
          }
        }
      }
    }
  }

  // A budget window: label and percentage, meter with the target marker,
  // and the reset countdown computed against the record's timestamp.
  component UsageRow: Column {
    id: usageRow
    property string title: ""
    property var win: null

    readonly property real utilization: win ? Math.max(0, Math.min(1, root.num(win.utilization))) : -1
    readonly property real target: win ? Math.max(0, Math.min(1, root.num(win.target))) : 0
    readonly property bool overTarget: utilization >= 0 && target > 0 && utilization >= target

    visible: !!win
    spacing: Style.space(4)

    Item {
      width: parent.width
      implicitHeight: Math.max(usageLabel.implicitHeight, usageValue.implicitHeight)

      Text {
        id: usageLabel
        text: usageRow.title
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
      }

      Text {
        id: usageValue
        text: usageRow.utilization >= 0 ? Math.round(usageRow.utilization * 100) + "%" : "—"
        color: usageRow.overTarget ? root.urgent : root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
      }
    }

    Item {
      width: parent.width
      implicitHeight: Math.max(Style.space(4), Math.round(Style.spacing.controlHeight * 0.14))

      Rectangle {
        id: usageTrack
        anchors.fill: parent
        radius: height / 2
        color: root.track
      }

      Rectangle {
        anchors.left: usageTrack.left
        anchors.verticalCenter: usageTrack.verticalCenter
        height: usageTrack.height
        radius: usageTrack.radius
        width: usageTrack.width * Math.max(0, usageRow.utilization)
        color: usageRow.overTarget ? root.urgent : root.foreground

        Behavior on width {
          NumberAnimation { duration: 160; easing.type: Easing.OutCubic }
        }
      }

      Rectangle {
        visible: usageRow.target > 0
        x: usageTrack.width * usageRow.target - width / 2
        anchors.verticalCenter: usageTrack.verticalCenter
        width: Math.max(2, Style.space(2))
        height: usageTrack.height + Style.space(4)
        color: root.warning
      }
    }

    Text {
      width: parent.width
      text: {
        var remainingMs = root.resetsInMs(usageRow.win)
        return remainingMs > 0 ? "Resets in " + root.formatDuration(remainingMs) : ""
      }
      visible: text !== ""
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }
}
