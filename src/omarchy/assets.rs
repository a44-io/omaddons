//! The widget's runtime files, embedded so the widget is exactly the code of the build
//! that installs it (vision D10). Its tests and the generated `Build.js` are not here;
//! `tests/omarchy_widget.rs` holds this table equal to the runtime files under
//! `omarchy/`, so a view added there is embedded in the same commit.

/// The manifest's path in [`ASSETS`].
pub const MANIFEST: &str = "manifest.json";

/// `(path within the plugin, contents)`, one row per runtime file, sorted by path.
pub const ASSETS: &[(&str, &str)] = &[
    ("BarWidget.qml", include_str!("../../omarchy/BarWidget.qml")),
    ("Model.js", include_str!("../../omarchy/Model.js")),
    (
        "PlainLabel.qml",
        include_str!("../../omarchy/PlainLabel.qml"),
    ),
    ("Service.qml", include_str!("../../omarchy/Service.qml")),
    ("Store.js", include_str!("../../omarchy/Store.js")),
    (MANIFEST, include_str!("../../omarchy/manifest.json")),
];
