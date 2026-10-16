//! Release graphs for dependency tests: one GitHub release per node, whose folders'
//! TOCs declare what they require. Graphs express chains (A → B → C), cycles (A ↔ B),
//! in-archive requirements (A requires A_Lib in the same zip) and bridge folders (a
//! second folder requiring an addon outside the graph).

use std::path::Path;

use super::addons::toc;
use super::http::{Asset, gh_latest, gh_release_v};

/// One release: the repository at `base`, tag `tag`, holding `folders`.
#[derive(Debug, Clone, Copy)]
pub struct Node<'a> {
    pub base: &'a str,
    pub tag: &'a str,
    pub folders: &'a [Folder<'a>],
}

/// One addon folder of a release.
#[derive(Debug, Clone, Copy)]
pub struct Folder<'a> {
    pub name: &'a str,
    pub interface: &'a str,
    /// `## Dependencies`.
    pub deps: &'a [&'a str],
    /// `## OptionalDeps`.
    pub optional: &'a [&'a str],
}

/// One GitHub release per node ([`gh_release_v`] plus [`gh_latest`]), each zip holding
/// its folders with `## Interface`, `## Version: <tag>`, `## Dependencies: …` and
/// `## OptionalDeps: …` (each line only when the list is not empty). The release.json
/// lists no flavors, so the TOCs decide where a release loads.
pub fn release_graph(scenario: &Path, nodes: &[Node<'_>]) {
    for node in nodes {
        let tocs: Vec<(String, String)> = node
            .folders
            .iter()
            .map(|f| (format!("{0}/{0}.toc", f.name), folder_toc(f, node.tag)))
            .collect();
        let file = format!("{}.zip", node.base.rsplit('/').next().unwrap());
        let entries: Vec<(&str, &str)> = tocs
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        let assets: [Asset<'_>; 1] = [(file.as_str(), &[], entries)];
        gh_release_v(scenario, node.base, node.tag, &assets);
        gh_latest(scenario, node.base, node.tag);
    }
}

/// The TOC of `folder` in a release tagged `tag`.
pub fn folder_toc(folder: &Folder<'_>, tag: &str) -> String {
    let title = format!("## Title: {}", folder.name);
    let version = format!("## Version: {tag}");
    let deps = format!("## Dependencies: {}", folder.deps.join(", "));
    let optional = format!("## OptionalDeps: {}", folder.optional.join(", "));
    let mut lines = vec![title.as_str(), version.as_str()];
    if !folder.deps.is_empty() {
        lines.push(&deps);
    }
    if !folder.optional.is_empty() {
        lines.push(&optional);
    }
    toc(folder.interface, &lines)
}
