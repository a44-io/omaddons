import QtQuick
import QtQuick.Controls
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "views"
import "ui"
import "Model.js" as Model
import "Build.js" as Build

// The panel frame (F4): the hero with Check now, Undo last and Open AddOns, the target
// filter, the view switcher, the content slot the views load into (F5, F6, F9), the
// footer (progress, the last result with its Undo and Fix, the error cards), the
// confirm dialog every destructive action goes through, and the key reference (`?`).
// Keyboard first: one dispatcher looks every key up in Model.KEYMAP (design doc §3)
// and maps its action to a service call or the current view. It decides nothing:
// labels come from Model.js over the CLI's own output, every action is a service call,
// and every CLI string renders as plain text (PlainLabel, or qs.Ui's own labels).
Item {
  id: root

  // The shared service, the bar's colors and font, and whether the panel is open.
  property var svc: null
  property color foreground: Commons.Color.foreground
  property color urgent: Commons.Color.urgent
  property string fontFamily: Style.font.family
  property bool opened: false

  signal closeRequested()
  // Tab and Shift-Tab: the neighbouring bar panel (Omarchy's, never intercepted).
  signal tabRequested(int direction)

  readonly property color dim: Qt.darker(foreground, 1.55)
  // What the panel needs to show everything: the column, or the dialog, chooser or key
  // reference over it.
  readonly property real contentImplicitHeight: confirming || chooser.opened || helpOpen
    ? Math.max(column.implicitHeight, Style.space(confirming ? 220 : helpOpen ? 560 : 360))
    : column.implicitHeight

  // ---- view state, in memory only (M3-D3)
  property string view: "updates"
  // "" is All; otherwise a target id. View-bound requests carry it as their selection.
  property string filter: ""
  property int cursor: 0
  property bool cursorActive: false
  property double nowMs: Date.now()
  // A refusal from the service ("omaddons is not ready", a template it would not
  // build), until the next action.
  property string notice: ""
  // Why the last key did nothing here ("Nothing to pin here"), until the next key.
  property string keyNote: ""
  // The key reference (`?`) is open over everything but a dialog.
  property bool helpOpen: false

  // The views by name: F5 adds updates and installed, F6 issues and search, F9 backups.
  // A view without one leaves its slot empty; the switcher and the key map are final.
  // A view may offer `frame` (set on load), `rowCount`, `rowItem(i)`, `typing`,
  // `focusSearch()` and `handleKey(action, cursor)`, which gets KEYMAP's action names.
  property var viewComponents: ({ "updates": updatesView, "installed": installedView, "issues": issuesView,
    "backups": backupsView, "search": searchView })
  // Frame-wide view actions a later task registers (backup, dismiss, …): action →
  // function(frame).
  property var keyHandlers: ({
    "dismiss": function(frame) { return frame.dismissErrors() },
    "backup": function(frame) { return frame.backUp() }
  })

  readonly property var current: contentLoader.item
  readonly property int rowCount: current && typeof current.rowCount === "number" ? current.rowCount : 0
  // A text field of the view has the keyboard: the key catcher lets it type.
  readonly property bool typing: !!current && current.typing === true

  readonly property string phase: svc ? String(svc.phase) : "starting"
  // Another omaddons build (M3-D42): read-only, only Retry and Reinstall act.
  readonly property bool readOnly: phase === "incompatible"
  readonly property bool canAct: !!svc && svc.canAct === true
  readonly property bool busy: !!svc && !!svc.action
  readonly property var summary: svc ? svc.summary : null
  readonly property var targets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property var selectedTarget: Model.targetById(targets, filter)
  readonly property var lastChange: svc ? svc.lastChange : null
  readonly property var undo: Model.undoAction(lastChange)
  readonly property var openArgv: Model.addonsDirArgv(selectedTarget)
  readonly property var lastResult: svc ? svc.lastResult : null
  readonly property var resultUndo: Model.resultUndo(lastResult)
  readonly property var lastError: svc ? svc.lastError : null
  readonly property var confirm: svc ? svc.confirm : null
  readonly property bool confirming: !!confirm
  // Without a compatible answer there are no views to act in: the card's buttons take
  // the cursor (j/k) and Enter.
  readonly property bool cardMode: phase === "missing" || phase === "unreachable" || phase === "incompatible"
  readonly property var cardActions: phase === "incompatible" ? ["retry", "reinstall"] : (cardMode ? ["retry"] : [])
  // Where a key is looked up in KEYMAP: the card's rows, or the current view's.
  readonly property string keyContext: cardMode ? "card" : view
  // Where the keyboard goes: the dialog's own keys while it is open, then the chooser's,
  // then the key reference's.
  readonly property Item focusItem: confirming ? confirmKeys
    : (chooser.opened ? chooser.keyItem : (helpOpen ? help.keyItem : keyCatcher))
  // The frame's own shortcuts (Ctrl+Z, PgUp, PgDn) fire only where the catcher would
  // take keys: never under a dialog, the reference, or a field that is typing.
  readonly property bool shortcutsOn: opened && !typing && !confirming && !chooser.opened && !helpOpen

  function alpha(c, a) { return Qt.rgba(c.r, c.g, c.b, a) }

  // ------------------------------------------------------------- state changes

  onOpenedChanged: {
    helpOpen = false
    if (!opened) return
    nowMs = Date.now()
    cursor = 0
    cursorActive = false
    notice = ""
    keyNote = ""
    flick.contentY = 0
    if (!takeRequestedView() && svc) svc.readView(view)
  }

  onConfirmingChanged: {
    // A new preview starts on No: Enter alone never confirms a destructive change.
    dialog.selectedIndex = 0
    // A dialog is the top layer: the reference gives way to it.
    if (confirming) helpOpen = false
    Qt.callLater(root.focusNow)
  }
  onConfirmChanged: dialog.selectedIndex = 0
  onTypingChanged: if (!typing) Qt.callLater(root.focusNow)
  onHelpOpenChanged: Qt.callLater(root.focusNow)

  function focusNow() {
    if (root.opened && !root.typing) root.focusItem.forceActiveFocus()
  }

  // The view the service was asked to show (middle click, `show <view>` over IPC).
  function takeRequestedView() {
    if (!svc || Model.VIEWS.indexOf(String(svc.requestedView)) < 0) return false
    var requested = String(svc.requestedView)
    svc.requestedView = ""
    showView(requested)
    return true
  }

  Connections {
    target: root.svc
    ignoreUnknownSignals: true
    function onRequestedViewChanged() { if (root.opened) root.takeRequestedView() }
  }

  // "checked N min ago" stays true while the panel sits open.
  Timer {
    interval: 30000
    repeat: true
    running: root.opened
    onTriggered: root.nowMs = Date.now()
  }

  function showView(name) {
    view = name
    keyNote = ""
    cursor = 0
    flick.contentY = 0
    if (svc) svc.readView(name)
  }

  // ------------------------------------------------------------- the key map

  // One function per PanelKeyCatcher signal. The catcher moves the cursor (j/k) and
  // steps views (h/l) itself; Enter, Space, x and every typed key go through act, which
  // looks them up in Model.KEYMAP.
  function move(dx, dy) {
    keyNote = ""
    if (dx !== 0) {
      showView(Model.stepView(view, dx))
      return
    }
    moveCursor(cursor + dy)
  }

  // The cursor to row `to` (the card's button in a problem card), kept in range.
  function moveCursor(to) {
    var count = cardMode ? cardActions.length : rowCount
    cursorActive = true
    cursor = count > 0 ? Math.max(0, Math.min(count - 1, to)) : 0
    ensureCursorVisible()
  }

  // How many rows a page key moves: as many of the cursor's row as the content shows.
  function pageRows() {
    var item = current && typeof current.rowItem === "function" ? current.rowItem(cursor) : null
    var rowHeight = item && item.height > 0 ? item.height : Style.space(32)
    return Math.max(1, Math.floor(flick.height / rowHeight) - 1)
  }

  // Enter and Space: the card's button, or the row's primary action (the view's).
  function activate() {
    return act("Enter")
  }

  function deleteKey() {
    return act("x")
  }

  function textKey(text) {
    return act(text)
  }

  // The key `name` (Model.keyName's) where the panel stands: its KEYMAP action run, its
  // note shown when it acts only in other views, or nothing.
  function act(name) {
    var key = Model.panelKey(name, keyContext)
    keyNote = ""
    if (!key) return false
    if (key.action === null) {
      keyNote = String(key.note || "")
      return true
    }
    var action = key.action
    if (action === "view") {
      showView(key.view)
      return true
    }
    if (action === "help") {
      helpOpen = true
      return true
    }
    if (action === "press") return runCard(cardActions[Math.min(cursor, cardActions.length - 1)])
    if (action === "client") {
      filter = Model.stepFilter(targets, filter, key.step)
      cursor = 0
      return true
    }
    if (action === "search") {
      showView("search")
      if (current && typeof current.focusSearch === "function") current.focusSearch()
      return true
    }
    if (action === "edge") {
      moveCursor(key.step < 0 ? 0 : rowCount - 1)
      return true
    }
    if (action === "page") {
      moveCursor(cursor + key.step * pageRows())
      return true
    }
    // Every other action waits for a compatible answer (M3-D42).
    if (cardMode || !canAct) return false
    if (action === "update_all") return updateAll()
    if (action === "check") return checkNow()
    if (action === "undo") return undoLast()
    if (action === "open") return openAddOns()
    return routeKey(action)
  }

  // An action the current view or a later task implements; an unregistered one does
  // nothing (no key exists before its action does).
  function routeKey(name) {
    if (current && typeof current.handleKey === "function" && current.handleKey(name, cursor) === true) return true
    var handler = keyHandlers[name]
    if (typeof handler !== "function") return false
    handler(root)
    return true
  }

  // The dialog's keys (it has the keyboard while open), as KEYMAP's sheet rows say. A
  // held key that started before the preview appeared reaches it only as auto-repeat,
  // which never confirms: Yes needs a fresh press (M3-D46).
  function confirmKey(key, text, autoRepeat, modifiers) {
    if (!confirming) return false
    var action = Model.dialogKey(Model.keyName(key, text, modifiers), autoRepeat)
    if (action === "cancel") confirmNo()
    else if (action === "switch") dialog.selectedIndex = dialog.selectedIndex === 0 ? 1 : 0
    else if (action === "confirm") confirmYes()
    else if (action === "press") {
      if (dialog.selectedIndex === 1) confirmYes()
      else confirmNo()
    }
    // Every other key is the dialog's too: nothing behind it acts.
    return true
  }

  // ------------------------------------------------------------- actions

  function report(refusal) {
    notice = refusal ? String(refusal) : ""
    return refusal === ""
  }

  function updateAll() {
    if (busy) return false
    var request = Model.updateRequest(filter, Model.updateRows(svc.lists, filter, targets))
    return report(svc.run(request.template, request.slots, request.label, request.shown))
  }

  function checkNow() {
    svc.check()
    return true
  }

  function undoLast() {
    if (busy || !undo.argv) return false
    return report(svc.runCliArgv(undo.argv, undo.label))
  }

  function openAddOns() {
    if (!openArgv) return false
    Quickshell.execDetached(Model.addonsDirArgv(selectedTarget))
    return true
  }

  function runCli(command, label) {
    if (busy || !command || !command.argv) return false
    return report(svc.runCliArgv(command.argv, label))
  }

  // `d` (and the Issues view's Dismiss): the filtered client's new errors, or each
  // client's, bound to the errors read the panel shows.
  function dismissErrors() {
    if (busy || !svc) return false
    return report(svc.dismissErrors(Model.dismissPlan(svc.errors, filter)))
  }

  // `b` (and the Backups view's button): the filtered client, or every client.
  function backUp() {
    if (busy || !svc) return false
    var request = Model.backupRequest(filter)
    return report(svc.run(request.template, request.slots, request.label))
  }

  // `r` on an addon row, or Enter on a backup: the restore chooser
  // (`{target, addon}` or `{target, backup}`).
  function openChooser(spec) {
    if (!canAct || busy || !spec || !spec.target) return false
    chooser.open(spec)
    return true
  }

  function chooserClosed() {
    Qt.callLater(root.focusNow)
  }

  function runCard(action) {
    if (!svc) return false
    if (action === "retry") svc.refresh()
    else if (action === "reinstall") svc.reinstallWidget()
    else return false
    return true
  }

  function confirmYes() {
    if (svc) report(svc.confirmYes())
  }

  function confirmNo() {
    if (svc) svc.confirmNo()
  }

  // Keeps the cursor's row (or card button) inside the scrolled area: the key catcher
  // consumes j/k, so the Flickable never scrolls by itself (F0).
  function ensureCursorVisible() {
    var item = null
    if (cardMode) item = cardButtons.itemAt(cursor)
    else if (current && typeof current.rowItem === "function") item = current.rowItem(cursor)
    if (!item) return
    var y = item.mapToItem(column, 0, 0).y
    var margin = Style.space(8)
    if (y < flick.contentY + margin)
      flick.contentY = Math.max(0, y - margin)
    else if (y + item.height > flick.contentY + flick.height - margin)
      flick.contentY = Math.min(Math.max(0, flick.contentHeight - flick.height), y + item.height - flick.height + margin)
  }

  // The service's (or a view's) text field gives the keyboard back.
  function leaveField() {
    keyCatcher.forceActiveFocus()
  }

  // ------------------------------------------------------------- the panel

  PanelKeyCatcher {
    id: keyCatcher
    objectName: "keyCatcher"
    anchors.fill: parent
    blocked: root.typing || root.confirming || chooser.opened

    onMoveRequested: function(dx, dy) { root.move(dx, dy) }
    // Enter emits returnRequested then activateRequested (F0): only the latter acts.
    onActivateRequested: root.activate()
    onDeleteRequested: root.deleteKey()
    onCloseRequested: root.closeRequested()
    onTabRequested: function(direction) { root.tabRequested(direction) }
    onTextKey: function(text) { root.textKey(text) }

    Flickable {
      id: flick
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
        width: flick.width
        spacing: Style.space(12)

        // ---------- Hero: the summary, Check now, Undo last, Open AddOns ----------
        PanelHero {
          id: hero
          objectName: "hero"
          width: parent.width
          title: "omaddons"
          meta: Model.heroMeta(root.summary)
          // Read-only keeps the last numbers, marked stale.
          detail: root.readOnly ? "stale" : Model.heroDetail(root.summary, root.targets, root.nowMs)
          metaOpacity: root.readOnly ? 0.5 : 1.0
          foreground: root.foreground
          fontFamily: root.fontFamily

          iconComponent: Component {
            Text {
              textFormat: Text.PlainText
              // U+F0297 nf-md-gamepad_variant, as on the bar.
              text: String.fromCodePoint(0xF0297)
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.display
            }
          }

          trailingControl: Component {
            Row {
              spacing: Style.spacing.sm

              PanelActionButton {
                objectName: "checkButton"
                // U+F04E6 nf-md-sync.
                iconText: String.fromCodePoint(0xF04E6)
                tooltipText: Model.withKey("Check now", "check")
                enabled: root.canAct
                opacity: enabled ? 1.0 : 0.4
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.checkNow()
              }

              PanelActionButton {
                objectName: "undoButton"
                // U+F054C nf-md-undo.
                iconText: String.fromCodePoint(0xF054C)
                tooltipText: Model.withKey(root.undo.label, "undo")
                enabled: root.canAct && !root.busy && root.undo.argv !== null
                opacity: enabled ? 1.0 : 0.4
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.undoLast()
              }

              PanelActionButton {
                objectName: "openButton"
                // U+F0770 nf-md-folder_open.
                iconText: String.fromCodePoint(0xF0770)
                tooltipText: Model.withKey("Open AddOns", "open")
                enabled: root.canAct && root.openArgv !== null
                opacity: enabled ? 1.0 : 0.4
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.openAddOns()
              }
            }
          }
        }

        // ---------- Cards: no answer to work with ----------
        Text {
          visible: root.phase === "starting"
          width: parent.width
          textFormat: Text.PlainText
          text: "Asking omaddons…"
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
        }

        BorderSurface {
          id: phaseCard
          objectName: "phaseCard"
          visible: root.cardMode
          width: parent.width
          implicitHeight: cardColumn.implicitHeight + Style.spacing.xl * 2
          color: root.alpha(root.urgent, 0.10)
          borderSpec: Border.flat(root.alpha(root.urgent, 0.35), 1)
          radius: Style.cornerRadius

          Column {
            id: cardColumn
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            anchors.leftMargin: Style.space(12)
            anchors.rightMargin: Style.space(12)
            spacing: Style.spacing.md

            PlainLabel {
              objectName: "cardTitle"
              width: parent.width
              wrapMode: Text.WordWrap
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
              text: root.phase === "missing" ? "omaddons not found at " + (root.svc ? root.svc.binary : "")
                : root.phase === "unreachable" ? "omaddons gave no answer the widget can read"
                : root.phase === "incompatible" && root.svc ? root.svc.incompatibleText : ""
            }

            // The binary's own words when it answered with something else.
            PlainLabel {
              objectName: "cardDetail"
              visible: root.phase === "unreachable" && text !== ""
              width: parent.width
              wrapMode: Text.WrapAnywhere
              maximumLineCount: 8
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: root.svc ? String(root.svc.unreachableText || "") : ""
            }

            // A missing binary: the two ways back, as text (they need a path or a
            // terminal only the user has).
            Repeater {
              objectName: "missingCommands"
              model: root.phase === "missing"
                ? ["omarchy bar set " + Build.PLUGIN_ID + " binary <path>", "omaddons omarchy install"]
                : []

              PlainLabel {
                required property string modelData
                width: cardColumn.width
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: modelData
              }
            }

            Row {
              spacing: Style.spacing.md

              Repeater {
                id: cardButtons
                model: root.cardActions

                Button {
                  required property string modelData
                  required property int index
                  objectName: "card_" + modelData
                  text: modelData === "reinstall" ? "Reinstall widget" : "Retry"
                  hasCursor: root.cursorActive && index === root.cursor
                  bordered: true
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  fontSize: Style.font.bodySmall
                  onClicked: root.runCard(modelData)
                }
              }
            }
          }
        }

        // ---------- First run and the widget's own issues ----------
        Column {
          objectName: "noTargets"
          // Only an answer that lists no client; a failed status shows its error instead.
          visible: root.phase === "ok" && root.targets.length === 0 && root.summary !== null
          width: parent.width
          spacing: Style.spacing.sm

          Text {
            width: parent.width
            textFormat: Text.PlainText
            text: "No World of Warcraft clients found"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          // It needs a path only the user has: text, no button (M3-D2).
          Text {
            width: parent.width
            textFormat: Text.PlainText
            text: "Add one from a terminal: omaddons targets add <path>"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
        }

        BorderSurface {
          objectName: "widgetOutdated"
          visible: root.phase === "ok" && !!root.summary && root.summary.widgetOutdated === true
          width: parent.width
          implicitHeight: outdatedRow.implicitHeight + Style.spacing.lg * 2
          color: "transparent"
          borderSpec: Border.flat(root.alpha(root.foreground, 0.25), 1)
          radius: Style.cornerRadius

          Row {
            id: outdatedRow
            anchors.verticalCenter: parent.verticalCenter
            anchors.left: parent.left
            anchors.leftMargin: Style.space(12)
            spacing: Style.spacing.lg

            Text {
              anchors.verticalCenter: parent.verticalCenter
              textFormat: Text.PlainText
              text: "The widget is from another build"
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
            }

            Button {
              objectName: "reinstallOutdated"
              text: "Reinstall"
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              onClicked: root.runCard("reinstall")
            }
          }
        }

        // ---------- Target filter: All, then each client (t) ----------
        Flow {
          id: filterRow
          objectName: "filterRow"
          visible: root.targets.length > 0
          width: parent.width
          spacing: Style.spacing.md

          Button {
            objectName: "filter_all"
            text: "All"
            selected: root.filter === ""
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            onClicked: root.filter = ""
          }

          Repeater {
            model: root.targets

            Button {
              required property var modelData
              objectName: "filter_" + modelData.id
              // A running client carries a dot (U+25CF).
              text: modelData.id + (modelData.running ? " " + String.fromCodePoint(0x25CF) : "")
              selected: root.filter === modelData.id
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              onClicked: root.filter = modelData.id
            }
          }

          Text {
            objectName: "runningUnknown"
            visible: root.targets.some(function(t) { return t.runningUnknown === true })
            textFormat: Text.PlainText
            text: "running: unknown"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
        }

        // ---------- View switcher (h/l, 1–5) ----------
        Row {
          id: switcher
          objectName: "switcher"
          width: parent.width
          spacing: Style.spacing.sm
          readonly property real cellWidth: (width - spacing * (Model.VIEWS.length - 1)) / Model.VIEWS.length

          Repeater {
            model: Model.VIEWS

            Button {
              required property string modelData
              required property int index
              objectName: "view_" + modelData
              width: switcher.cellWidth
              text: (index + 1) + " " + Model.VIEW_TITLES[modelData]
              selected: root.view === modelData
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.bodySmall
              onClicked: root.showView(modelData)
            }
          }
        }

        // ---------- The current view ----------
        Loader {
          id: contentLoader
          objectName: "content"
          width: parent.width
          sourceComponent: root.viewComponents[root.view] || null
          // Read-only: the rows stay visible, inert.
          enabled: !root.readOnly
          onLoaded: if (item && "frame" in item) item.frame = root
        }

        // ---------- Footer: progress, the last result, the last failure ----------
        Column {
          id: footer
          objectName: "footer"
          width: parent.width
          spacing: Style.spacing.sm

          Rectangle {
            objectName: "progressTrack"
            visible: root.busy
            width: parent.width
            height: Style.space(4)
            radius: height / 2
            color: root.alpha(root.foreground, 0.15)

            Rectangle {
              readonly property real fraction: Model.progressFraction(root.svc ? root.svc.action : null)
              width: parent.width * (fraction < 0 ? 0.15 : fraction)
              height: parent.height
              radius: parent.radius
              color: Commons.Color.accent
            }
          }

          PlainLabel {
            objectName: "progressLine"
            visible: root.busy
            width: parent.width
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: root.busy ? String(root.svc.action.label || "") + (root.svc.action.message ? ": " + root.svc.action.message : "") : ""
          }

          // Why the change buttons are off.
          Text {
            objectName: "busyNote"
            visible: root.busy
            width: parent.width
            textFormat: Text.PlainText
            text: "other changes wait until this one finishes"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }

          PlainLabel {
            objectName: "queued"
            visible: text !== ""
            width: parent.width
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: root.svc && root.svc.queuedAction ? "next: " + root.svc.queuedAction : ""
          }

          Row {
            objectName: "result"
            visible: !!root.lastResult && !root.busy
            width: parent.width
            spacing: Style.spacing.md

            PlainLabel {
              objectName: "resultLine"
              anchors.verticalCenter: parent.verticalCenter
              width: Math.min(implicitWidth, parent.width - (resultUndoButton.visible ? resultUndoButton.width + parent.spacing : 0))
              color: root.lastResult && !root.lastResult.ok ? root.urgent : root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: root.lastResult ? String(root.lastResult.line || "") : ""
            }

            Button {
              id: resultUndoButton
              objectName: "resultUndo"
              visible: root.resultUndo !== null
              enabled: root.canAct && !root.busy
              opacity: enabled ? 1.0 : 0.4
              text: "Undo"
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.caption
              onClicked: root.runCli(root.resultUndo, "Undo")
            }
          }

          // A partial result: each failed item's fix the classifier accepts.
          Flow {
            visible: !!root.lastResult && !root.busy
            width: parent.width
            spacing: Style.spacing.sm

            Repeater {
              model: root.lastResult && Array.isArray(root.lastResult.fixes) ? root.lastResult.fixes : []

              Button {
                required property var modelData
                required property int index
                objectName: "itemFix_" + index
                enabled: root.canAct && !root.busy
                opacity: enabled ? 1.0 : 0.4
                text: "Fix: " + modelData.command
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                fontSize: Style.font.caption
                onClicked: root.runCli(modelData, "Fix")
              }
            }
          }

          BorderSurface {
            objectName: "errorCard"
            // Like the result row: an earlier failure is not shown while another runs.
            visible: !!root.lastError && !root.cardMode && !root.busy
            width: parent.width
            implicitHeight: errorColumn.implicitHeight + Style.spacing.lg * 2
            color: root.alpha(root.urgent, 0.10)
            borderSpec: Border.flat(root.alpha(root.urgent, 0.35), 1)
            radius: Style.cornerRadius

            Column {
              id: errorColumn
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              anchors.leftMargin: Style.space(12)
              anchors.rightMargin: Style.space(12)
              spacing: Style.spacing.sm

              PlainLabel {
                objectName: "errorMessage"
                width: parent.width
                wrapMode: Text.WordWrap
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: root.lastError ? String(root.lastError.message || "") : ""
              }

              // A fix the classifier accepts is a button; the first is the Fix, the
              // rest its alternatives.
              Flow {
                width: parent.width
                spacing: Style.spacing.sm

                Repeater {
                  model: root.lastError && Array.isArray(root.lastError.fixes) ? root.lastError.fixes : []

                  Button {
                    required property var modelData
                    required property int index
                    objectName: "fix_" + index
                    enabled: root.canAct && !root.busy
                    opacity: enabled ? 1.0 : 0.4
                    text: index === 0 ? "Fix: " + modelData.command : modelData.command
                    bordered: index === 0
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    fontSize: Style.font.caption
                    onClicked: root.runCli(modelData, index === 0 ? "Fix" : "Fix (alternative)")
                  }
                }
              }

              // A prose fix, or one the classifier refused: text.
              PlainLabel {
                objectName: "fixText"
                visible: text !== ""
                width: parent.width
                wrapMode: Text.WordWrap
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: root.lastError && root.lastError.fixText ? String(root.lastError.fixText) : ""
              }

              // A restore whose folders changed owner since the backup: one button per
              // folder, each the same restore's preview for that folder.
              Flow {
                width: parent.width
                spacing: Style.spacing.sm

                Repeater {
                  model: root.lastError && Array.isArray(root.lastError.choices) ? root.lastError.choices : []

                  Button {
                    required property var modelData
                    required property int index
                    objectName: "choice_" + index
                    enabled: root.canAct && !root.busy
                    opacity: enabled ? 1.0 : 0.4
                    text: "Restore " + modelData.label
                    bordered: true
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    fontSize: Style.font.caption
                    onClicked: root.report(root.svc.restoreFolder(modelData.addon))
                  }
                }
              }

              // A restore that cannot exchange here: the backup's own plain folder.
              PlainLabel {
                objectName: "backupFolder"
                visible: text !== ""
                width: parent.width
                wrapMode: Text.WrapAnywhere
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: root.lastError && root.lastError.folder ? "The backup's own folder: " + root.lastError.folder : ""
              }
            }
          }

          PlainLabel {
            objectName: "notice"
            visible: text !== ""
            width: parent.width
            wrapMode: Text.WordWrap
            color: root.urgent
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: root.notice
          }

          // A key that does nothing here says so (design doc §3.3), until the next key.
          PlainLabel {
            objectName: "keyNote"
            visible: text !== ""
            width: parent.width
            wrapMode: Text.WordWrap
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: root.keyNote
          }

          // Never a guessed run (M3-D37).
          Text {
            objectName: "lastChangeUnknown"
            visible: !!root.lastChange && root.lastChange.unknown === true
            width: parent.width
            textFormat: Text.PlainText
            text: "last change: unknown"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
        }
      }
    }
  }

  // ---------- The confirm dialog: the CLI's own dry run (M3-D9, M3-D46) ----------
  ConfirmDialog {
    id: dialog
    objectName: "confirmDialog"
    anchors.fill: parent
    z: 10
    opened: root.confirming
    message: root.confirm
      ? (root.confirm.stale ? String(root.confirm.stale) + "\n\n" : "") + String(root.confirm.text || "")
      : ""
    cancelText: "No"
    confirmText: "Yes"
    foreground: root.foreground
    fontFamily: root.fontFamily
    onConfirmed: root.confirmYes()
    onCanceled: root.confirmNo()
  }

  Component {
    id: updatesView
    UpdatesView {}
  }

  Component {
    id: installedView
    InstalledView {}
  }

  Component {
    id: issuesView
    IssuesView {}
  }

  Component {
    id: searchView
    SearchView {}
  }

  Component {
    id: backupsView
    BackupsView {}
  }

  RestoreChooser {
    id: chooser
    objectName: "chooser"
    anchors.fill: parent
    z: 8
    frame: root
  }

  // ---------- The key reference (`?`): KEYMAP's rows where the panel stands ----------
  // Over the views and the problem cards, under a dialog. While it is open every key is
  // its own: `?` and Esc close it, j/k scroll it, and nothing beneath it acts.
  Sheet {
    id: help
    objectName: "help"
    anchors.fill: parent
    z: 9
    reference: true
    opened: root.helpOpen
    title: "Keys · " + (root.cardMode ? "Problem card" : String(Model.VIEW_TITLES[root.view] || ""))
    foreground: root.foreground
    fontFamily: root.fontFamily
    onCanceled: root.helpOpen = false

    Repeater {
      objectName: "helpSections"
      model: root.helpOpen ? Model.helpSections(root.keyContext) : []

      Column {
        required property var modelData
        width: help.bodyWidth
        spacing: Style.spacing.xs
        bottomPadding: Style.spacing.md

        SectionHeader {
          width: parent.width
          title: modelData.title
          foreground: root.foreground
          fontFamily: root.fontFamily
        }

        Repeater {
          model: modelData.rows

          Row {
            required property var modelData
            width: help.bodyWidth
            spacing: Style.spacing.md

            PlainLabel {
              id: hintLabel
              objectName: "helpKey"
              width: Style.space(84)
              elide: Text.ElideNone
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              text: modelData.hint
            }

            PlainLabel {
              objectName: "helpLabel"
              width: parent.width - hintLabel.width - parent.spacing
              wrapMode: Text.WordWrap
              color: help.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: modelData.label
            }
          }
        }
      }
    }
  }

  // Takes the keyboard while the dialog is open: it sees auto-repeat, which the key
  // catcher's signals do not carry.
  Item {
    id: confirmKeys
    objectName: "confirmKeys"
    Keys.onPressed: function(event) {
      event.accepted = root.confirmKey(event.key, event.text, event.isAutoRepeat, event.modifiers)
    }
  }

  // The keys that are no single character (design doc §3: Ctrl+Z, PgUp, PgDn) are
  // window shortcuts: they fire on the ShortcutOverride pass, before the key catcher
  // sees the key, and go through the same KEYMAP lookup as every other key.
  Item {
    Shortcut {
      sequence: "Ctrl+Z"
      enabled: root.shortcutsOn
      onActivated: root.act("Ctrl+Z")
    }
    Shortcut {
      sequence: "PgUp"
      enabled: root.shortcutsOn
      onActivated: root.act("PgUp")
    }
    Shortcut {
      sequence: "PgDown"
      enabled: root.shortcutsOn
      onActivated: root.act("PgDn")
    }
  }
}
