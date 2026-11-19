//! Details coverage rows: the bar widget's install (omad-widget-plugin-3bcbh.9). Omarchy's
//! plugin add failing to clone the widget's repository (`download_failed`), and a folder in
//! the widget's place that omaddons did not install (`file_exists`).

use std::fs;

use omaddons::error::ErrorCode;

use super::super::support::check;
use super::Row;
use crate::common::home::Home;
use crate::common::omarchy::stub_desk;
use crate::common::privacy::assert_private_all;

pub const ROWS: &[Row] = &[INSTALL_REFUSED];

const INSTALL_REFUSED: Row = Row {
    id: "the_bar_widgets_install_fails_or_refuses",
    keys: &[
        (ErrorCode::DownloadFailed, "reason"),
        (ErrorCode::DownloadFailed, "said"),
        (ErrorCode::DownloadFailed, "url"),
        (ErrorCode::FileExists, "file"),
        (ErrorCode::FileExists, "reason"),
    ],
    run: || {
        let home = Home::new(&["wow"]);
        let desk = stub_desk(&home.env);
        let env: Vec<(&str, String)> = desk
            .env()
            .iter()
            .map(|(key, value)| (*key, value.display().to_string()))
            .collect();
        let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let install = |code| {
            let argv = ["omarchy", "install", "--json"].map(str::to_owned);
            let checked = check(home.exec_with(&argv, &pairs), code);
            assert_private_all(&checked.run.stdout, &checked.run.stderr);
            checked
        };
        let mode = home.env.root().join("add-mode");
        fs::write(&mode, "fail").unwrap();
        let failed = install(1);
        fs::remove_file(&mode).unwrap();
        fs::create_dir_all(desk.plugin()).unwrap();
        fs::write(
            desk.plugin().join("manifest.json"),
            r#"{"schemaVersion": 1, "id": "a44.omaddons"}"#,
        )
        .unwrap();
        let foreign = install(4);
        vec![failed, foreign]
    },
};
