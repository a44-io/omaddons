//! Targets that share one AddOns directory, such as an `Interface/AddOns` symlinked
//! between two Classic clients. Each target keeps its own records and files index, so
//! an addon installed through one shows up on the other as unmanaged folders, and
//! adopting them there would record the same folders twice. `status` warns
//! (`target_addons_shared`); M4's doctor reports the same groups.
//!
//! Read-only: each AddOns directory is resolved with `canonicalize` only to compare it.

use std::path::PathBuf;

use crate::model::Target;

/// Targets whose AddOns directories resolve to one directory, or one inside another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedAddons {
    /// The outermost resolved directory of the group.
    pub path: PathBuf,
    /// Target ids, in the order given.
    pub targets: Vec<String>,
}

/// Every group of two or more `targets` sharing an AddOns directory. A target whose
/// AddOns directory does not exist (a never-launched client) shares nothing.
pub fn shared_addons<'a>(targets: impl IntoIterator<Item = &'a Target>) -> Vec<SharedAddons> {
    let mut groups: Vec<SharedAddons> = Vec::new();
    for target in targets {
        let Ok(resolved) = std::fs::canonicalize(target.addons_dir()) else {
            continue;
        };
        let overlapping = groups
            .iter_mut()
            .find(|g| g.path.starts_with(&resolved) || resolved.starts_with(&g.path));
        match overlapping {
            Some(group) => {
                if resolved.components().count() < group.path.components().count() {
                    group.path = resolved;
                }
                group.targets.push(target.id.clone());
            }
            None => groups.push(SharedAddons {
                path: resolved,
                targets: vec![target.id.clone()],
            }),
        }
    }
    groups.retain(|g| g.targets.len() > 1);
    groups
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use super::*;

    /// A target `id` whose client dir is `dir`.
    fn target(id: &str, dir: &Path) -> Target {
        let mut t = crate::engine::testing::target(dir);
        t.id = id.to_owned();
        t
    }

    #[test]
    fn a_symlinked_addons_dir_is_one_group_and_the_rest_share_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let shared = root.join("shared-addons");
        std::fs::create_dir_all(&shared).unwrap();
        let era = root.join("wow/_classic_era_");
        let anniversary = root.join("wow/_anniversary_");
        let retail = root.join("wow/_retail_");
        let linked = root.join("wow/_ptr_");
        for client in [&era, &anniversary, &linked] {
            std::fs::create_dir_all(client.join("Interface")).unwrap();
        }
        symlink(&shared, era.join("Interface/AddOns")).unwrap();
        symlink(&shared, anniversary.join("Interface/AddOns")).unwrap();
        std::fs::create_dir_all(retail.join("Interface/AddOns")).unwrap();
        // A link to a directory no other target uses.
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, linked.join("Interface/AddOns")).unwrap();
        let fresh = root.join("wow/_beta_");
        std::fs::create_dir_all(&fresh).unwrap();

        let targets = [
            target("era", &era),
            target("retail", &retail),
            target("anniversary", &anniversary),
            target("ptr", &linked),
            target("beta", &fresh),
        ];
        let groups = shared_addons(&targets);
        assert_eq!(
            groups,
            [SharedAddons {
                path: std::fs::canonicalize(&shared).unwrap(),
                targets: vec!["era".into(), "anniversary".into()],
            }]
        );
        assert!(shared_addons(&targets[1..2]).is_empty());
    }

    #[test]
    fn an_addons_dir_inside_another_is_shared() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("a");
        std::fs::create_dir_all(outer.join("Interface/AddOns/Nested")).unwrap();
        let inner = dir.path().join("b");
        std::fs::create_dir_all(inner.join("Interface")).unwrap();
        symlink(
            outer.join("Interface/AddOns/Nested"),
            inner.join("Interface/AddOns"),
        )
        .unwrap();
        let groups = shared_addons(&[target("inner", &inner), target("outer", &outer)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].path,
            std::fs::canonicalize(outer.join("Interface/AddOns")).unwrap(),
            "the outermost directory"
        );
        assert_eq!(groups[0].targets, ["inner", "outer"]);
    }
}
