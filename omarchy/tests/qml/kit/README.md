# Kit-mode stand-ins for `argc widget-snapshots`

`scripts/widget-test.sh --snapshots <dir>` renders `tst_snapshots.qml` with Omarchy's own
shell kit (`qs.Ui`, `qs.Commons` from `/usr/share/omarchy/shell`, or `$OMARCHY_SHELL_DIR`)
and the machine's current theme, so the renders look like the bar. The kit's QML needs a
few Quickshell types that only the shell binary provides; these files stand in for them,
laid over a copy of `../imports/Quickshell` in the snapshot run's own import tree:

- `Quickshell/Singleton.qml`: Quickshell's `Singleton` base type, which the kit's
  singletons (`IpcRegistry`, `I18n`) extend.
- `Quickshell/Quickshell.qml`: the test stub's singleton, with `env()` answering `HOME`
  (from the generated `KitEnv.js`) so `Color` and `Style` find the current theme.
- `Quickshell/Io/FileView.qml`: reads the file it names once, through `XMLHttpRequest`
  (`QML_XHR_ALLOW_FILE_READ=1`), so the theme's `colors.toml` and `shell.toml` load.

The kit's `KeyboardPanel` and `Panel` (layer-shell windows) keep the test stubs, and the
kit's audio and input singletons are left out of its `qmldir`: the widget uses none.
The gate never uses this mode; it renders with the stubs under `../imports`.
