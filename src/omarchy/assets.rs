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
    ("PanelView.qml", include_str!("../../omarchy/PanelView.qml")),
    (
        "PlainLabel.qml",
        include_str!("../../omarchy/PlainLabel.qml"),
    ),
    (
        "RestoreChooser.qml",
        include_str!("../../omarchy/RestoreChooser.qml"),
    ),
    ("Service.qml", include_str!("../../omarchy/Service.qml")),
    ("Store.js", include_str!("../../omarchy/Store.js")),
    (MANIFEST, include_str!("../../omarchy/manifest.json")),
    (
        "ui/ActivityBar.qml",
        include_str!("../../omarchy/ui/ActivityBar.qml"),
    ),
    (
        "ui/AddonRow.qml",
        include_str!("../../omarchy/ui/AddonRow.qml"),
    ),
    ("ui/Badge.qml", include_str!("../../omarchy/ui/Badge.qml")),
    ("ui/Chip.qml", include_str!("../../omarchy/ui/Chip.qml")),
    (
        "ui/EmptyState.qml",
        include_str!("../../omarchy/ui/EmptyState.qml"),
    ),
    (
        "ui/KeyHint.qml",
        include_str!("../../omarchy/ui/KeyHint.qml"),
    ),
    (
        "ui/NoticeCard.qml",
        include_str!("../../omarchy/ui/NoticeCard.qml"),
    ),
    (
        "ui/PlanSheet.qml",
        include_str!("../../omarchy/ui/PlanSheet.qml"),
    ),
    (
        "ui/ProgressBar.qml",
        include_str!("../../omarchy/ui/ProgressBar.qml"),
    ),
    (
        "ui/SectionHeader.qml",
        include_str!("../../omarchy/ui/SectionHeader.qml"),
    ),
    (
        "ui/Separator.qml",
        include_str!("../../omarchy/ui/Separator.qml"),
    ),
    ("ui/Sheet.qml", include_str!("../../omarchy/ui/Sheet.qml")),
    (
        "ui/TabCount.qml",
        include_str!("../../omarchy/ui/TabCount.qml"),
    ),
    (
        "ui/TabStrip.qml",
        include_str!("../../omarchy/ui/TabStrip.qml"),
    ),
    (
        "ui/ViewState.qml",
        include_str!("../../omarchy/ui/ViewState.qml"),
    ),
    (
        "ui/VirtualList.qml",
        include_str!("../../omarchy/ui/VirtualList.qml"),
    ),
    (
        "views/AddClientPage.qml",
        include_str!("../../omarchy/views/AddClientPage.qml"),
    ),
    (
        "views/AddonPage.qml",
        include_str!("../../omarchy/views/AddonPage.qml"),
    ),
    (
        "views/BackupsView.qml",
        include_str!("../../omarchy/views/BackupsView.qml"),
    ),
    (
        "views/ErrorPage.qml",
        include_str!("../../omarchy/views/ErrorPage.qml"),
    ),
    (
        "views/ErrorsView.qml",
        include_str!("../../omarchy/views/ErrorsView.qml"),
    ),
    (
        "views/InstalledView.qml",
        include_str!("../../omarchy/views/InstalledView.qml"),
    ),
    (
        "views/IssuesView.qml",
        include_str!("../../omarchy/views/IssuesView.qml"),
    ),
    (
        "views/ProblemPage.qml",
        include_str!("../../omarchy/views/ProblemPage.qml"),
    ),
    (
        "views/SearchView.qml",
        include_str!("../../omarchy/views/SearchView.qml"),
    ),
    (
        "views/UpdatesView.qml",
        include_str!("../../omarchy/views/UpdatesView.qml"),
    ),
];
