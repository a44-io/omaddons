import QtQuick
import QtTest
import qs.Commons as Commons
import "../../ui"
import "../../Model.js" as Model

// The shared components of omarchy/ui/ (design doc §5): each renders what it is handed,
// fires its signals, elides as §5.3 says at 400 wide, and decides nothing. The Sheet's
// fresh-press guard, and VirtualList's delegate reuse and cursor following.
TestCase {
  id: testCase
  name: "Components"
  when: windowShown
  visible: true
  width: 600
  height: 720

  property var made: []

  function make(component, props) {
    var item = component.createObject(testCase, props || {})
    verify(item !== null)
    made = made.concat([item])
    return item
  }

  function cleanup() {
    for (var i = 0; i < made.length; i++) made[i].destroy()
    made = []
  }

  function child(item, name) {
    var found = findChild(item, name)
    verify(found !== null, "no " + name)
    return found
  }

  Component { id: rowComponent; AddonRow { width: 540 } }
  Component { id: chipComponent; Chip {} }
  Component { id: headerComponent; SectionHeader { width: 540 } }
  Component { id: activityComponent; ActivityBar { width: 540 } }
  Component { id: emptyComponent; EmptyState { width: 540 } }
  Component { id: cardComponent; NoticeCard { width: 540 } }
  Component { id: progressComponent; ProgressBar { width: 400 } }
  Component { id: badgeComponent; Badge {} }
  Component { id: hintComponent; KeyHint {} }
  Component { id: separatorComponent; Separator {} }
  Component { id: viewStateComponent; ViewState { width: 540 } }
  Component { id: alertComponent; AlertLine {} }

  Component {
    id: sheetComponent
    Sheet {
      width: 540
      height: 600
      title: "Remove DBM from Retail?"
      confirmText: "Remove"
      Text { textFormat: Text.PlainText; text: "DBM 12.1.15 (1 folder)" }
    }
  }

  // A sheet of choices, each line as the choosers build theirs (1n2p.15): hover through
  // the sheet's gate, one click chooses.
  Component {
    id: choosingComponent
    Sheet {
      id: choosing
      width: 540
      height: 600
      title: "Restore which?"
      choosing: true
      choiceCount: 3
      Repeater {
        model: 3
        Rectangle {
          required property int index
          objectName: "choiceLine_" + index
          width: 400
          height: 40
          color: "transparent"
          MouseArea {
            id: lineArea
            anchors.fill: parent
            hoverEnabled: true
            onPositionChanged: function(mouse) { choosing.hoverChoice(index, lineArea, mouse) }
            onClicked: {
              choosing.choice = index
              choosing.choose(index)
            }
          }
        }
      }
    }
  }

  Component { id: planComponent; PlanSheet { width: 540; height: 600 } }

  // A PlanSheet handed `sheet` as the service hands it, by a JS assignment (createObject's
  // initial properties would turn its arrays into lists).
  function makePlan(sheet, props) {
    var item = make(planComponent, props)
    item.plan = sheet
    return item
  }

  // A plan sheet as Model builds one: `count` updates over Retail and Classic Era, an
  // urgent effect after a normal one in the list it was handed, and `extra` laid over.
  function plan(count, extra) {
    var items = []
    for (var i = 0; i < count; i++) {
      var client = i < count / 2 ? "Retail" : "Classic Era"
      var first = i === 0 || i === Math.ceil(count / 2)
      var name = "Addon" + (i < 10 ? "0" : "") + i
      items.push({ "op": "update", "addon": name, "client": client, "clientKey": client, "from": "1.0", "to": "1.1",
        "name": name, "detail": "1.0 → 1.1", "text": name + " 1.0 → 1.1", "header": first ? client : "" })
    }
    var out = { "title": "Update " + count + " addons on 2 clients?", "stale": "", "items": items,
      "effects": [
        { "kind": "folders_removed", "severity": "urgent", "glyph": 0xF0258, "text": "Removes the folder Addon00_Options. Undo brings it back." },
        { "kind": "dependency_added", "severity": "normal", "glyph": 0xF0431, "text": "Also installs LibStub 1.0, a new dependency." }],
      "confirmLabel": "Update " + count, "cancelLabel": "Cancel", "destructive": false, "glyph": 0 }
    for (var key in (extra || {})) out[key] = extra[key]
    return out
  }

  Component {
    id: listComponent
    VirtualList {
      width: 400
      height: 400
      model: 500
      delegate: Rectangle {
        required property int index
        width: ListView.view ? ListView.view.width : 0
        height: 40
        Component.onCompleted: ListView.view.built++
      }
    }
  }

  readonly property var actions: [
    { "name": "update", "glyph": String.fromCodePoint(0xF0CE1), "tooltip": "Update (u)" },
    { "name": "notes", "glyph": String.fromCodePoint(0xF0DC9), "tooltip": "Notes (e)" },
    { "name": "pin", "glyph": String.fromCodePoint(0xF0403), "tooltip": "Pin (p)" }
  ]

  // ---- AddonRow

  function test_a_row_shows_what_it_is_handed() {
    var row = make(rowComponent, { "title": "DBM", "fromVersion": "12.1.14", "toVersion": "12.1.15", "meta": "2 days ago",
      "chips": [{ "text": "Retail" }], "band": "accent", "glyph": String.fromCodePoint(0xF0CE1), "actions": actions })
    compare(child(row, "title").text, "DBM")
    compare(child(row, "meta").text, "2 days ago")
    verify(child(row, "band").visible)
    compare(child(row, "band").color, row.accent)
    compare(child(row, "chips").children.length >= 1, true)
    compare(row.shownActions.length, 0, "actions show on the cursored row only")
    row.cursored = true
    compare(row.shownActions.length, 3)
    verify(row.Accessible.name.indexOf("DBM, 12.1.14 to 12.1.15") === 0, row.Accessible.name)
  }

  function test_a_row_signals_its_activation_and_actions() {
    var row = make(rowComponent, { "title": "DBM", "cursored": true, "actions": actions, "meta": "m" })
    var activated = 0
    var named = []
    row.activated.connect(function() { activated++ })
    row.action.connect(function(name) { named.push(name) })
    waitForRendering(row)
    mouseClick(row, 40, row.height / 2)
    compare(activated, 1, "a click on the row opens it")
    var pin = child(row, "action_pin")
    mouseClick(pin, pin.width / 2, pin.height / 2)
    compare(named, ["pin"], "an action button names its action")
    compare(activated, 1, "the action's click is the button's, not the row's")
  }

  function test_a_pending_row_shows_its_verb_and_no_actions() {
    var row = make(rowComponent, { "title": "Plater", "meta": "GitHub", "cursored": true, "actions": actions,
      "pending": true, "pendingText": "Updating… downloading (2 of 3)" })
    compare(child(row, "meta").text, "Updating… downloading (2 of 3)")
    compare(child(row, "glyph").text, String.fromCodePoint(0xF04E6), "the spinner replaces the glyph")
    compare(row.shownActions.length, 0)
  }

  function test_an_inert_row_is_dimmed_without_actions() {
    var row = make(rowComponent, { "title": "DBM", "cursored": true, "actions": actions, "inert": true })
    compare(row.opacity, 0.45)
    compare(row.shownActions.length, 0)
  }

  function test_at_400_the_title_elides_and_the_versions_never_do() {
    var row = make(rowComponent, { "width": 400, "title": "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic",
      "fromVersion": "v661.20251108", "toVersion": "v662.20251109", "chips": [{ "text": "Retail" }, { "text": "beta" }],
      "actions": actions, "cursored": true })
    waitForRendering(row)
    verify(row.narrow)
    verify(row.titleTruncated, "the long title elides")
    var versions = child(row, "versions")
    verify(versions.x + versions.width <= row.width, "the versions stay inside the row")
    compare(row.shownChips.length, 1, "at 400 only the first chip stays")
    compare(row.shownActions.length, 1, "at 400 only the main action shows")
  }

  // The right edge of `item` in `row`'s coordinates.
  function rightIn(row, item) {
    return item.mapToItem(row, item.width, 0).x
  }

  // omad-39he: a row as narrow as 400 wide with the text zoomed to 2 (a 200 px canvas).
  function test_versions_that_leave_the_title_too_little_take_a_line_of_their_own() {
    var row = make(rowComponent, { "width": 200, "title": "Plater", "fromVersion": "Plater-v661",
      "toVersion": "Plater-v662", "chips": [{ "text": "Retail" }] })
    waitForRendering(row)
    var title = child(row, "title")
    var versions = child(row, "versions")
    verify(!row.titleTruncated, "the title keeps its room")
    compare(versions.x, 0, "under the title, at its start")
    verify(versions.y >= title.height, "on a line of its own: " + versions.y + " under " + title.height)
    for (var name of ["fromVersion", "toVersion"]) {
      var label = child(row, name)
      verify(label.visible)
      verify(!label.truncated, name + " in full")
      verify(rightIn(row, label) <= row.width, name + " ends at " + rightIn(row, label) + " in " + row.width)
    }
    verify(rightIn(row, child(row, "chips")) <= row.width, "the chip stays on the title's line, inside the row")
    var plain = make(rowComponent, { "width": 200, "title": "Plater", "chips": [{ "text": "Retail" }] })
    verify(row.implicitHeight > plain.implicitHeight, "the row grows by the versions' line")
  }

  function test_a_version_wider_than_its_line_wraps_whole() {
    var row = make(rowComponent, { "width": 160, "title": "Details", "fromVersion": "Details.20251102.13711.165",
      "toVersion": "Details.20251109.13800.170" })
    waitForRendering(row)
    var title = child(row, "title")
    for (var name of ["fromVersion", "toVersion"]) {
      var label = child(row, name)
      verify(!label.truncated, name + " in full")
      verify(rightIn(row, label) <= row.width, name + " ends at " + rightIn(row, label) + " in " + row.width)
      verify(label.height > title.height, name + " wraps: " + label.height + " for a line of " + title.height)
    }
  }

  function test_short_versions_stay_beside_the_title() {
    var row = make(rowComponent, { "title": "DBM", "fromVersion": "12.1.14", "toVersion": "12.1.15",
      "chips": [{ "text": "Retail" }, { "text": "beta" }] })
    var plain = make(rowComponent, { "title": "DBM", "chips": [{ "text": "Retail" }, { "text": "beta" }] })
    waitForRendering(row)
    var versions = child(row, "versions")
    compare(versions.y, 0, "on the title's line")
    compare(versions.x, child(row, "title").width + Commons.Style.spacing.lg)
    fuzzyCompare(versions.width, versions.natural, 0.5, "on one line")
    compare(row.implicitHeight, plain.implicitHeight, "no taller than a row without versions")
  }

  // ---- Chip, SectionHeader, KeyHint, Separator

  function test_an_urgent_chip_carries_its_glyph() {
    var calm = make(chipComponent, { "text": "Retail" })
    verify(!child(calm, "chipGlyph").visible)
    var urgent = make(chipComponent, { "text": "files changed", "urgent": true })
    verify(child(urgent, "chipGlyph").visible, "never urgent by colour alone")
    compare(child(urgent, "chipText").text, "files changed")
  }

  function test_a_section_header_shows_its_title_count_and_button() {
    var header = make(headerComponent, { "title": "Retail", "count": "3 updates", "buttonText": "Update Retail (3)" })
    compare(child(header, "sectionTitle").text, "Retail")
    compare(child(header, "sectionCount").text, "3 updates")
    var clicks = 0
    header.buttonClicked.connect(function() { clicks++ })
    var button = child(header, "sectionButton")
    waitForRendering(header)
    mouseClick(button, button.width / 2, button.height / 2)
    compare(clicks, 1)
    header.width = 320
    header.title = "Wrath 3.3.5a private server on the old drive"
    waitForRendering(header)
    verify(header.titleTruncated, "a long title elides before the button")
  }

  // A bordered button anchored to the right edge takes a whole-pixel width, so its right
  // border ends on the content's last pixel and is never clipped, whatever its label
  // measures (omad-kldz). The padding makes the label's width fractional, as some
  // labels and fonts do.
  function test_right_anchored_buttons_take_whole_pixel_widths() {
    var header = make(headerComponent, { "title": "Retail", "buttonText": "Update Retail (3)" })
    var bar = make(activityComponent, { "view": { "kind": "check", "text": "Checking for updates",
      "fraction": 0.3, "elapsed": "", "stalled": false, "cancellable": true } })
    var cases = [
      ["sectionButton", header, child(header, "sectionButton")],
      ["activityCancel", bar, child(bar, "activityCancel")]
    ]
    for (var i = 0; i < cases.length; i++) {
      var name = cases[i][0], button = cases[i][2]
      verify(button.visible, name)
      button.horizontalPadding = 10.25
      waitForRendering(cases[i][1])
      verify(button.implicitWidth !== Math.ceil(button.implicitWidth), name + ": a fractional label")
      compare(button.width, Math.ceil(button.implicitWidth), name)
      var right = button.mapToItem(cases[i][1], button.width, 0).x
      compare(right, cases[i][1].width, name + ": the right edge is the content's")
      compare(button.mapToItem(cases[i][1], 0, 0).x % 1, 0, name + ": a whole-pixel x")
    }
  }

  function test_a_key_hint_and_a_separator_render() {
    var hint = make(hintComponent, { "key": "Ctrl+Z" })
    compare(child(hint, "keyText").text, "Ctrl+Z")
    verify(hint.implicitWidth >= hint.implicitHeight)
    var line = make(separatorComponent, { "width": 300 })
    compare(line.height, 1)
  }

  // ---- ViewState's spinner, AlertLine (1n2p.40)

  function test_the_refreshing_spinner_turns_and_stands_still_with_reduced_motion() {
    var state = make(viewStateComponent, { "viewState": { "view": "updates", "state": "rows", "refreshing": true, "notices": [] } })
    var spinner = child(state, "stateRefreshing").children[0]
    tryVerify(function() { return spinner.rotation !== 0 }, 1000, "it turns")
    // Hyprland's animations off: it stands still, "Updating…" beside it (§5.7).
    Commons.Style.reduceMotion = true
    try {
      tryCompare(spinner, "rotation", 0)
      wait(150)
      compare(spinner.rotation, 0)
      verify(child(state, "refreshingText").text !== "")
    } finally {
      Commons.Style.reduceMotion = false
    }
  }

  function test_an_alert_line_is_words_beside_the_urgent_glyph_or_dim_words() {
    var line = make(alertComponent, { "text": "Couldn't read the release notes", "width": 300 })
    var glyph = child(line, "alertGlyph")
    verify(glyph.visible)
    compare(glyph.color, line.urgent)
    compare(child(line, "alertText").color, line.foreground, "its words in the foreground, never urgent")
    compare(line.Accessible.role, Accessible.AlertMessage)
    compare(line.Accessible.name, "Couldn't read the release notes")
    line.marked = false
    verify(!glyph.visible)
    compare(child(line, "alertText").color, line.dim)
    compare(child(line, "alertText").x, 0, "no room kept for a glyph")
    compare(line.Accessible.role, Accessible.StaticText)
  }

  // ---- EmptyState, NoticeCard

  function test_an_empty_state_says_its_sentence_and_offers_one_action() {
    var empty = make(emptyComponent, { "glyph": String.fromCodePoint(0xF05E0), "sentence": "All up to date. Checked 25 min ago.",
      "actionText": "Check now", "actionKey": "c" })
    compare(child(empty, "emptySentence").text, "All up to date. Checked 25 min ago.")
    var fired = 0
    empty.action.connect(function() { fired++ })
    var button = child(empty, "emptyAction")
    waitForRendering(empty)
    mouseClick(button, button.width / 2, button.height / 2)
    compare(fired, 1)
  }

  function test_a_notice_card_has_its_kind_glyph_and_at_most_three_buttons() {
    var failure = make(cardComponent, { "kind": "failure", "title": "Couldn't reach GitHub", "body": "Check the connection.",
      "buttons": [{ "name": "retry", "text": "Try again" }, { "name": "a", "text": "A" }, { "name": "b", "text": "B" },
        { "name": "c", "text": "C" }], "dismissible": true })
    compare(child(failure, "noticeGlyph").text, String.fromCodePoint(0xF0028))
    compare(child(failure, "noticeGlyph").color, failure.urgent)
    verify(findChild(failure, "noticeButton_2") !== null)
    compare(findChild(failure, "noticeButton_3"), null, "a fourth button is never drawn")
    var names = []
    failure.clicked.connect(function(name) { names.push(name) })
    var dismissed = 0
    failure.dismissed.connect(function() { dismissed++ })
    waitForRendering(failure)
    var retry = child(failure, "noticeButton_0")
    mouseClick(retry, retry.width / 2, retry.height / 2)
    compare(names, ["retry"])
    var close = child(failure, "noticeDismiss")
    mouseClick(close, close.width / 2, close.height / 2)
    compare(dismissed, 1)
    var info = make(cardComponent, { "kind": "info", "title": "Some errors couldn't be read yet" })
    compare(child(info, "noticeGlyph").text, String.fromCodePoint(0xF02FD))
    var success = make(cardComponent, { "kind": "success", "title": "Updated 3 addons" })
    compare(child(success, "noticeGlyph").text, String.fromCodePoint(0xF05E0))
  }

  // ---- Sheet

  function test_a_sheet_opens_on_cancel_and_takes_every_key() {
    var sheet = make(sheetComponent)
    var confirmed = 0
    var canceled = 0
    sheet.confirmed.connect(function() { confirmed++ })
    sheet.canceled.connect(function() { canceled++ })
    sheet.opened = true
    compare(sheet.selectedIndex, 0, "a new sheet starts on Cancel")
    verify(sheet.handleKey(Qt.Key_J, "j", false), "a letter is the sheet's")
    compare(confirmed + canceled, 0)
    verify(sheet.handleKey(Qt.Key_Return, "", false))
    compare([confirmed, canceled], [0, 1], "Enter on Cancel cancels")
    sheet.selectedIndex = 0
    verify(sheet.handleKey(Qt.Key_L, "l", false))
    compare(sheet.selectedIndex, 1)
    verify(sheet.handleKey(Qt.Key_Return, "", false))
    compare(confirmed, 1, "Enter on the confirm button confirms")
    verify(sheet.handleKey(Qt.Key_Escape, "", false))
    compare(canceled, 2)
  }

  function test_a_held_key_never_confirms_a_sheet() {
    var sheet = make(sheetComponent)
    var confirmed = 0
    sheet.confirmed.connect(function() { confirmed++ })
    sheet.opened = true
    sheet.selectedIndex = 1
    verify(sheet.handleKey(Qt.Key_Y, "y", true))
    verify(sheet.handleKey(Qt.Key_Return, "", true))
    compare(confirmed, 0, "auto-repeat never confirms (M3-D46)")
    verify(sheet.handleKey(Qt.Key_Y, "y", false))
    compare(confirmed, 1, "a fresh press does")
    // Its buttons name their keys, from KEYMAP's dialog rows.
    compare(child(sheet, "sheetCancel").tooltipText, sheet.cancelText + " (" + Model.keyHint("cancel", "sheet") + ")")
    compare(child(sheet, "sheetConfirm").tooltipText, sheet.confirmText + " (" + Model.keyHint("confirm", "sheet") + ")")
  }

  function test_a_reference_sheet_has_no_buttons_closes_on_its_keys_and_takes_the_rest() {
    var sheet = make(sheetComponent, { "reference": true })
    var confirmed = 0
    var canceled = 0
    sheet.confirmed.connect(function() { confirmed++ })
    sheet.canceled.connect(function() { canceled++ })
    sheet.opened = true
    verify(!child(sheet, "sheetConfirm").visible)
    verify(!child(sheet, "sheetCancel").visible)
    verify(child(sheet, "referenceNote").visible)
    for (var key of [[Qt.Key_Y, "y"], [Qt.Key_Return, ""], [Qt.Key_N, "n"], [Qt.Key_L, "l"], [Qt.Key_U, "u"]])
      verify(sheet.handleKey(key[0], key[1], false), "the reference takes " + key[1])
    compare([confirmed, canceled], [0, 0], "no key confirms or cancels a reference but its own")
    verify(sheet.handleKey(Qt.Key_Question, "?", false))
    compare(canceled, 1)
    verify(sheet.handleKey(Qt.Key_Escape, "", false))
    compare(canceled, 2)
    compare(confirmed, 0)
  }

  function test_a_click_on_the_backdrop_cancels_and_a_destructive_confirm_has_its_border() {
    var sheet = make(sheetComponent, { "destructive": true, "confirmGlyph": String.fromCodePoint(0xF01B4) })
    var canceled = 0
    sheet.canceled.connect(function() { canceled++ })
    sheet.opened = true
    waitForRendering(sheet)
    verify(child(sheet, "destructiveBorder").visible)
    verify(child(sheet, "sheetConfirm").text.indexOf("Remove") !== -1)
    mouseClick(sheet, 5, 5)
    compare(canceled, 1)
    var card = child(sheet, "sheetCard")
    mouseClick(card, 5, 5)
    compare(canceled, 1, "a click on the card is not a cancel")
    // Both buttons are easy to hit: at least the kit's control height (§4).
    verify(child(sheet, "sheetCancel").height >= 28, "Cancel is " + child(sheet, "sheetCancel").height + " px")
    verify(child(sheet, "sheetConfirm").height >= 28, "the confirm is " + child(sheet, "sheetConfirm").height + " px")
  }

  // Design doc §4 (1n2p.15): a choosing sheet's cursor follows the pointer only when the
  // pointer itself moves, a key's move holds against a still pointer, one click chooses.
  function test_a_choosing_sheet_follows_the_pointer_and_chooses_on_one_click() {
    var sheet = make(choosingComponent)
    var chosen = []
    sheet.chosen.connect(function(i) { chosen.push(i) })
    sheet.opened = true
    waitForRendering(sheet)
    var line = child(sheet, "choiceLine_2")
    mouseMove(line, 20, 20)
    mouseMove(line, 30, 20)
    compare(sheet.choice, 2, "the pointer moved over the third choice")
    verify(sheet.handleKey(Qt.Key_K, "k", false))
    compare(sheet.choice, 1)
    mouseMove(line, 30, 20)
    compare(sheet.choice, 1, "a still pointer does not take the cursor back from the keys")
    var first = child(sheet, "choiceLine_0")
    mouseMove(first, 20, 20)
    mouseMove(first, 30, 20)
    compare(sheet.choice, 0)
    compare(chosen, [], "hovering chooses nothing")
    mouseClick(child(sheet, "choiceLine_1"), 30, 20)
    compare(chosen, [1], "one click chooses")
  }

  // ---- PlanSheet (1n2p.17)

  function test_a_plan_sheet_shows_its_title_items_by_client_effects_and_verb() {
    var sheet = makePlan(plan(4))
    sheet.opened = true
    waitForRendering(sheet)
    compare(child(sheet, "sheetTitle").text, "Update 4 addons on 2 clients?")
    compare(child(sheet, "planItems").count, 4)
    // Each client's items under its name.
    var headers = []
    for (var i = 0; i < 4; i++) {
      var group = findChild(child(sheet, "planItems").itemAt(i), "planGroup")
      if (group.visible) headers.push(group.title)
    }
    compare(headers, ["Retail", "Classic Era"])
    compare(findChild(child(sheet, "planItems").itemAt(0), "planItemName").text, "Addon00")
    compare(findChild(child(sheet, "planItems").itemAt(0), "planItemDetail").text, "1.0 → 1.1")
    // The effects as handed, each with its glyph, the urgent one's in the urgent colour.
    var effects = child(sheet, "planEffects")
    compare(effects.count, 2)
    compare(findChild(effects.itemAt(0), "planEffectText").text, "Removes the folder Addon00_Options. Undo brings it back.")
    compare(findChild(effects.itemAt(0), "planEffectGlyph").text, String.fromCodePoint(0xF0258))
    verify(Qt.colorEqual(findChild(effects.itemAt(0), "planEffectGlyph").color, sheet.urgent), "an urgent effect's glyph is urgent")
    verify(findChild(effects.itemAt(0), "planEffectText").font.bold)
    verify(!Qt.colorEqual(findChild(effects.itemAt(1), "planEffectGlyph").color, sheet.urgent))
    // Cancel beside the verb; no urgent border on a plan that is not destructive.
    compare([child(sheet, "sheetCancel").text, child(sheet, "sheetConfirm").text], ["Cancel", "Update 4"])
    verify(!child(sheet, "destructiveBorder").visible)
    verify(!child(sheet, "planStale").visible, "no banner on a first preview")
  }

  function test_a_destructive_plan_sheet_has_its_urgent_verb_and_the_banner_when_stale() {
    var banner = "Changed since you looked: Foo v3 is now the newest"
    var sheet = makePlan(plan(1, { "title": "Remove Addon00 from Retail?", "confirmLabel": "Remove Addon00",
      "destructive": true, "glyph": 0xF01B4, "stale": banner }))
    sheet.opened = true
    waitForRendering(sheet)
    verify(child(sheet, "destructiveBorder").visible)
    compare(child(sheet, "sheetConfirm").text, String.fromCodePoint(0xF01B4) + " Remove Addon00")
    verify(child(sheet, "planStale").visible)
    compare(child(sheet, "planStale").title, banner)
  }

  function test_a_plan_sheet_opens_on_cancel_and_its_verb_takes_only_a_fresh_press() {
    var sheet = makePlan(plan(2))
    var confirmed = 0
    var canceled = 0
    sheet.confirmed.connect(function() { confirmed++ })
    sheet.canceled.connect(function() { canceled++ })
    sheet.opened = true
    compare(sheet.selectedIndex, 0, "it opens on Cancel")
    verify(sheet.handleKey(Qt.Key_Y, "y", true))
    verify(sheet.handleKey(Qt.Key_L, "l", false))
    compare(sheet.selectedIndex, 1, "l moves to the verb")
    verify(sheet.handleKey(Qt.Key_Return, "", true))
    verify(sheet.handleKey(Qt.Key_Space, " ", true))
    compare(confirmed, 0, "a held key never confirms (M3-D46)")
    verify(sheet.handleKey(Qt.Key_Return, "", false))
    compare(confirmed, 1, "a fresh Enter on the verb does")
    sheet.reset()
    compare(sheet.selectedIndex, 0, "a new plan starts on Cancel again")
    verify(sheet.handleKey(Qt.Key_Return, "", false))
    compare([confirmed, canceled], [1, 1], "Enter on Cancel cancels")
    verify(sheet.handleKey(Qt.Key_N, "n", false))
    compare(canceled, 2)
  }

  function test_forty_items_are_all_there_and_scroll_inside_the_plan_sheet() {
    var sheet = makePlan(plan(40), { "height": 500 })
    sheet.opened = true
    waitForRendering(sheet)
    compare(child(sheet, "planItems").count, 40, "no cap")
    var body = child(sheet, "sheetBody")
    verify(body.contentHeight > body.height, "the body overflows: " + body.contentHeight + " in " + body.height)
    verify(body.interactive)
    verify(child(sheet, "sheetCard").height <= sheet.height, "the card stays inside the panel")
    verify(sheet.wantedHeight > sheet.height, "it asks for the room the whole plan needs")
    verify(sheet.handleKey(Qt.Key_J, "j", false))
    verify(sheet.handleKey(Qt.Key_Down, "", false))
    verify(body.contentY > 0, "j and Down scroll")
    verify(sheet.handleKey(Qt.Key_K, "k", false))
    verify(sheet.handleKey(Qt.Key_Up, "", false))
    compare(body.contentY, 0, "k and Up scroll back")
    for (var i = 0; i < 200; i++) sheet.handleKey(Qt.Key_J, "j", false)
    compare(body.contentY, body.contentHeight - body.height, "never past the end")
    sheet.reset()
    compare(body.contentY, 0, "a new plan starts at the top")
  }

  function test_at_400_a_long_addon_name_elides_and_its_versions_never_do() {
    var long = "AnAddonWithAVeryLongNameThatCannotFitBesideItsVersionsAtFourHundred"
    var item = { "op": "update", "addon": long, "client": "Retail", "clientKey": "Retail", "from": "12.1.14-alpha", "to": "12.1.15-beta",
      "name": long, "detail": "12.1.14-alpha → 12.1.15-beta", "text": long + " 12.1.14-alpha → 12.1.15-beta", "header": "" }
    var sheet = makePlan(plan(1, { "items": [item] }), { "width": 400 })
    sheet.opened = true
    waitForRendering(sheet)
    var row = child(sheet, "planItems").itemAt(0)
    verify(findChild(row, "planItemName").truncated, "the name elides")
    var detail = findChild(row, "planItemDetail")
    verify(!detail.truncated, "the versions never do")
    compare(detail.text, "12.1.14-alpha → 12.1.15-beta")
    verify(detail.x + detail.width <= row.width + 1, "the versions fit in the row")
    compare(detail.y, 0, "beside the name, which still has room")
  }

  // omad-39he: as an addon row's, a plan item's versions that leave its name too little
  // take a line of their own, and wrap there when wider than it.
  function test_a_plan_items_long_versions_take_a_line_of_their_own() {
    var item = { "op": "update", "addon": "Plater", "client": "Retail", "clientKey": "Retail", "from": "Plater-v661",
      "to": "Plater-v662", "name": "Plater", "detail": "Plater-v661 → Plater-v662", "text": "Plater Plater-v661 → Plater-v662",
      "header": "" }
    var sheet = makePlan(plan(1, { "items": [item] }), { "width": 200 })
    sheet.opened = true
    waitForRendering(sheet)
    var row = child(sheet, "planItems").itemAt(0)
    var name = findChild(row, "planItemName")
    var detail = findChild(row, "planItemDetail")
    verify(!name.truncated, "the name keeps its room")
    compare(detail.x, 0, "under the name, at its start")
    verify(detail.y >= name.height, "on a line of its own")
    verify(!detail.truncated, "the versions in full")
    verify(rightIn(row, detail) <= row.width, "the versions end at " + rightIn(row, detail) + " in " + row.width)
  }

  // ---- ProgressBar, Badge

  function test_progress_is_determinate_or_a_fixed_fifteen_percent() {
    var bar = make(progressComponent)
    verify(bar.indeterminate)
    compare(child(bar, "progressFill").width, 400 * 0.15)
    bar.fraction = 0.5
    compare(child(bar, "progressFill").width, 200)
    bar.fraction = 3
    compare(bar.shown, 1)
  }

  function test_a_badge_counts_and_hides_at_zero() {
    var badge = make(badgeComponent, { "count": 0 })
    verify(!badge.visible)
    badge.count = 3
    verify(badge.visible)
    compare(child(badge, "badgeCount").text, "3")
    compare(badge.color, badge.accent)
    badge.urgent = true
    compare(badge.color, badge.urgentColor)
    badge.count = 140
    compare(child(badge, "badgeCount").text, "99+")
  }

  // ---- VirtualList

  function test_a_long_list_reuses_its_rows_and_follows_the_cursor() {
    var list = make(listComponent)
    waitForRendering(list)
    for (var i = 0; i < 500; i += 25) {
      list.cursor = i
      wait(0)
    }
    list.cursor = 400
    waitForRendering(list)
    verify(list.built < 60, "rows built for 500: " + list.built)
    var row = list.rowItem(400)
    verify(row !== null, "the cursor's row is built")
    verify(row.y >= list.contentY && row.y + row.height <= list.contentY + list.height, "and in view")
    compare(list.rowItem(5), null, "a row far from the view is not built")
    list.cursor = 0
    waitForRendering(list)
    compare(list.contentY, 0)
  }

  // ---- the dim colour (§5.1)

  function test_the_dim_colour_reaches_four_and_a_half_to_one() {
    var fg = Commons.Color.foreground
    var bg = Commons.Color.popups.background
    var dim = Model.dimColor(fg, bg)
    verify(Model.contrastRatio(dim, bg) >= 4.5, dim)
    verify(Model.contrastRatio(dim, bg) < Model.contrastRatio(fg, bg), "dimmer than the foreground")
  }
}
