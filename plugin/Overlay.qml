import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons
import qs.Ui

// Visual only. The daemon owns the keyboard, so this window takes no keys
// and no clicks.
Item {
  id: root

  property var chars: []

  function onLine(line) {
    var text = String(line || "").trim()
    if (!text) return
    var msg
    try {
      msg = JSON.parse(text)
    } catch (e) {
      return
    }
    if (msg.op === "show" && Array.isArray(msg.chars)) root.chars = msg.chars
    else if (msg.op === "hide") root.chars = []
  }

  function reconnect() {
    bridge.connected = false
    bridge.connected = true
  }

  Socket {
    id: bridge
    path: Quickshell.env("XDG_RUNTIME_DIR") + "/macmykeys.sock"
    connected: true
    parser: SplitParser {
      splitMarker: "\n"
      onRead: function(line) { root.onLine(line) }
    }
    onConnectionStateChanged: {
      if (!bridge.connected) root.chars = []
    }
    onError: function(error) {
      bridge.connected = false
    }
  }

  Timer {
    interval: 500
    running: !bridge.connected
    repeat: true
    onTriggered: root.reconnect()
  }

  readonly property int cell: Style.space(48)
  readonly property int pad: Style.space(16)
  readonly property bool open: root.chars.length > 0

  PanelWindow {
    id: panel
    visible: root.open
    anchors { top: true; bottom: true; left: true; right: true }
    color: "transparent"
    WlrLayershell.namespace: "macmykeys"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.None
    exclusionMode: ExclusionMode.Ignore
    mask: Region {}

    BorderSurface {
      id: card
      readonly property int cols: Math.max(root.chars.length, 1)
      width: card.borderLeft + root.pad + cols * root.cell + card.borderRight
      height: card.borderTop + root.pad + Style.font.display + Style.space(8) + Style.font.title + root.pad + card.borderBottom
      anchors.horizontalCenter: parent.horizontalCenter
      anchors.bottom: parent.bottom
      anchors.bottomMargin: Style.space(67)
      color: Util.alpha(Color.background, 0.97)
      borderSpec: Border.surfaceSpec("popups", "border", Color.popups.border, Math.max(1, Style.space(2)))
      radius: Style.cornerRadius
      opacity: root.open ? 1 : 0

      Row {
        anchors.fill: parent
        anchors.topMargin: card.borderTop + root.pad
        anchors.rightMargin: card.borderRight
        anchors.bottomMargin: card.borderBottom + root.pad
        anchors.leftMargin: card.borderLeft
        spacing: 0

        Repeater {
          model: root.chars
          delegate: Item {
            required property string modelData
            required property int index
            width: root.cell
            height: parent.height

            Text {
              anchors.horizontalCenter: parent.horizontalCenter
              anchors.top: parent.top
              text: modelData
              textFormat: Text.PlainText
              font.family: Style.font.family
              font.pixelSize: Style.font.display
              color: Color.popups.text
            }
            Text {
              anchors.horizontalCenter: parent.horizontalCenter
              anchors.bottom: parent.bottom
              text: index + 1
              textFormat: Text.PlainText
              font.family: Style.font.family
              font.pixelSize: Style.font.title
              color: Color.accent
            }
          }
        }
      }
    }
  }
}
