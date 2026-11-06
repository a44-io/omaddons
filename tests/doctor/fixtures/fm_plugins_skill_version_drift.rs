use std::fs;
use std::path::PathBuf;

use super::super::Fixture;
use super::found_ids;
use crate::common::home::Home;

const ID: &str = "fm-plugins-skill-version-drift";

/// An older guide: a document stamped as the renderer stamps one, whose digest is not this
/// build's.
const OLDER_GUIDE: &str = "---\nname: omaddons\ndescription: an older guide\n---\n";

/// `fm-plugins-skill-version-drift`: the agent skill installed in Claude Code's default
/// folder (`$HOME/.claude/skills/omaddons/SKILL.md`) holds another guide than this
/// binary's: a small document carrying its own correct stamp, as an older build's
/// `robot-docs install-skill` would have left it.
///
/// The finding: P3, tier `cached`, about the machine
/// (`fm-plugins-skill-version-drift@machine:path=<file>`), evidence `{path: <file>,
/// observed: sha256:<its digest>, expected: sha256:<this build's guide digest>, reason:
/// outdated, edited: false}`, the remediation argv `omaddons robot-docs install-skill`,
/// never auto-fixable (report-only, M4-D64). The remedy runs that command, which replaces
/// a pristine guide without keeping a copy.
pub struct SkillVersionDrift;

impl SkillVersionDrift {
    fn file(home: &Home) -> PathBuf {
        home.env.home().join(".claude/skills/omaddons/SKILL.md")
    }
}

impl Fixture for SkillVersionDrift {
    fn id(&self) -> &'static str {
        ID
    }

    fn corrupt(&self, home: &Home) {
        let file = Self::file(home);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, stamped(OLDER_GUIDE, "the older guide's text\n")).unwrap();
    }

    fn assert_healthy(&self, home: &Home) {
        let ids = found_ids(home, &["--only".to_owned(), ID.to_owned()]);
        assert!(!ids.iter().any(|id| id == ID), "{ID} still found: {ids:?}");
    }

    fn subject(&self, _: &Home) -> Vec<PathBuf> {
        Vec::new()
    }

    fn remedy(&self, home: &Home) {
        let run = home.run(&["robot-docs", "install-skill"]);
        assert_eq!(run.code, 0, "install-skill: {}", run.stdout);
    }
}

/// `frontmatter` and `body` with the stamp line between them, its digits the document's
/// own digest with that line omitted (the renderer's rule).
fn stamped(frontmatter: &str, body: &str) -> String {
    let zeros = "0".repeat(64);
    let provisional = format!("{frontmatter}{}\n{body}", line(&zeros));
    let digest = omaddons::contract::guide::stamp::digest_without_stamp(&provisional).unwrap();
    provisional.replacen(&zeros, &omaddons::contract::guide::stamp::hex(&digest), 1)
}

fn line(hex: &str) -> String {
    omaddons::contract::guide::stamp::line(hex)
}
