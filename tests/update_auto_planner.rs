//! The planner under the unattended policy (RG-52c, design 7.6). Every release
//! the planner finally settles for a request that names no version, the first choice, a
//! look-back's and a new dependency's, is held while it has not stood the wait since this
//! machine first saw it; a hold spreads to every addon whose new release requires the
//! held one, and the client is planned once more without them. Planned at library level
//! on `PlanningHome` (replayed fixtures, the persistent release cache): an attended plan
//! first, as a check would, so the release memo holds each release's first sight; then
//! those sightings aged as each case needs; then the unattended plan. The command that
//! passes the policy (`update --auto`) is RG-52d's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::fs;

use common::addons::toc;
use common::http::{Asset, fixture, gh_feed, gh_latest, gh_release, gh_release_v};
use common::planning::{ManagedAddon, PlanOpts, PlanningHome};
use jiff::{SignedDuration, Timestamp};
use omaddons::engine::plan::{
    AnyFlavorIntent, Held, HeldReason, PinIntent, Plan, RequestedChange, TargetRequest,
    UpdatePolicy,
};
use omaddons::model::{AddonReason, Channel, Target, TxKind};
use omaddons::state::memo::{self, ReleaseMemo};
use omaddons::state::store;
use serde_json::json;

const RETAIL: &str = "120105";

/// One managed folder as [`ManagedAddon`] takes it: its name and its files.
type Folder<'a> = (&'a str, &'a [(&'a str, &'a str)]);

/// Retail, its managed addons at `v1`, and planning as a command does.
struct Scene {
    home: PlanningHome,
    retail: Target,
}

impl Scene {
    /// Retail managing each of `addons` (name, repo under `o/`) at v1, with the catalog
    /// naming each and `extra` (name, repo) as `gh:o/<repo>`, so dependencies resolve.
    fn new(addons: &[(&str, &str)], extra: &[(&str, &str)]) -> Self {
        let home = PlanningHome::new();
        let mut retail = home.retail.clone();
        let tocs: Vec<(String, String)> = addons
            .iter()
            .map(|(name, _)| (format!("{name}.toc"), toc(RETAIL, &[])))
            .collect();
        let folder_files: Vec<[(&str, &str); 1]> = tocs
            .iter()
            .map(|(file, text)| [(file.as_str(), text.as_str())])
            .collect();
        let folders: Vec<[Folder<'_>; 1]> = addons
            .iter()
            .zip(&folder_files)
            .map(|((name, _), files)| [(*name, &files[..])])
            .collect();
        let refs: Vec<String> = addons
            .iter()
            .map(|(_, repo)| format!("gh:o/{repo}"))
            .collect();
        let managed: Vec<ManagedAddon<'_>> = addons
            .iter()
            .zip(&refs)
            .zip(&folders)
            .map(|(((name, _), r), folders)| ManagedAddon {
                name,
                r#ref: r,
                release: "v1",
                version: "v1",
                folders: &folders[..],
            })
            .collect();
        home.manage(&mut retail, &managed);
        let mut text = String::from("schema_version = \"1.0\"\n");
        for (name, repo) in addons.iter().chain(extra) {
            text.push_str(&format!(
                "\n[[addon]]\nname = {name:?}\nref = \"gh:o/{repo}\"\nfolders = [{name:?}]\n"
            ));
        }
        let dir = home.env.config().join("omaddons/catalog.d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("50-auto.toml"), text).unwrap();
        Self { home, retail }
    }

    /// Serves `gh:o/<repo>` at `tag`: one retail zip whose folder `name` requires
    /// `deps`, its Lua file holding `body`.
    fn release(&self, name: &str, repo: &str, tag: &str, deps: &[&str], body: &str) {
        let toc_text = if deps.is_empty() {
            toc(RETAIL, &[&format!("## Version: {tag}")])
        } else {
            toc(
                RETAIL,
                &[
                    &format!("## Version: {tag}"),
                    &format!("## Dependencies: {}", deps.join(", ")),
                ],
            )
        };
        let toc_path = format!("{name}/{name}.toc");
        let lua_path = format!("{name}/{name}.lua");
        gh_release(
            &self.home.scenario,
            &format!("https://github.com/o/{repo}"),
            tag,
            &[(
                &format!("{name}.zip"),
                &[("mainline", 120_105)],
                vec![
                    (toc_path.as_str(), toc_text.as_str()),
                    (lua_path.as_str(), body),
                ],
            )],
        );
    }

    /// A plan of `changes` on retail under `policy`.
    fn plan(&self, changes: Vec<RequestedChange>, policy: UpdatePolicy) -> Plan {
        let opts = PlanOpts {
            policy,
            with_catalog: true,
            ..PlanOpts::default()
        };
        let request = TargetRequest {
            target: self.retail.clone(),
            changes,
            default_kind: TxKind::Update,
            dependency_locks: Default::default(),
        };
        self.home.plan(vec![request], &opts)
    }

    /// The release memo's sightings of `gh:o/<repo>` moved `by` back, as if this machine
    /// had first seen those releases that much earlier.
    fn age(&self, repo: &str, by: SignedDuration) {
        let path = self.home.paths.release_memo();
        let mut memo: ReleaseMemo = store::load::<ReleaseMemo>(&path).unwrap().unwrap().value;
        let r = format!("gh:o/{repo}");
        let mut aged = 0;
        for entry in memo.entries.values_mut() {
            if entry.wanted.r#ref.to_string() == r {
                for seen in &mut entry.seen_releases {
                    seen.first_seen_at -= by;
                    aged += 1;
                }
            }
        }
        assert!(aged > 0, "no sighting of {r} to age");
        fs::write(&path, store::encode(&memo).unwrap()).unwrap();
    }
}

fn update(addon: &str) -> RequestedChange {
    RequestedChange::Update {
        addon: addon.into(),
        installed_primary_interface: None,
    }
}

fn install(r: &str) -> RequestedChange {
    RequestedChange::Install {
        r#ref: r.parse().unwrap(),
        channel: Channel::Stable,
        channel_explicit: false,
        version: None,
        file: None,
        name: None,
        pin: PinIntent::Keep,
        reason: AddonReason::Explicit,
        reinstall: false,
        blob_sha256: None,
        asset: None,
        revision: None,
        any_flavor: AnyFlavorIntent::Keep,
        rollback: None,
    }
}

fn wait(hours: i64) -> UpdatePolicy {
    UpdatePolicy::Unattended {
        min_age: SignedDuration::from_hours(hours).try_into().unwrap(),
        now: Timestamp::now(),
    }
}

/// What the plan holds on retail: (addon, reason).
fn held(plan: &Plan) -> Vec<(String, HeldReason)> {
    plan.targets[0]
        .held
        .iter()
        .map(|h| (h.addon.clone(), h.reason.clone()))
        .collect()
}

/// The addons the plan changes on retail, sorted.
fn changed(plan: &Plan) -> Vec<String> {
    let mut names: Vec<String> = plan.targets[0]
        .work
        .iter()
        .map(|w| w.addon.clone())
        .collect();
    names.sort();
    names
}

fn on(dependency: &str) -> HeldReason {
    HeldReason::DependencyHeld {
        dependency: dependency.into(),
    }
}

fn hours(h: i64) -> SignedDuration {
    SignedDuration::from_hours(h)
}

#[test]
fn under_attended_no_release_is_held() {
    let s = Scene::new(&[("Foo", "foo")], &[]);
    s.release("Foo", "foo", "v2", &[], "-- v2");
    let plan = s.plan(vec![update("Foo")], UpdatePolicy::Attended);
    assert!(held(&plan).is_empty());
    assert_eq!(changed(&plan), ["Foo"]);
}

/// The first choice: seen just now, it is held until its first sight plus the wait; seen
/// seven hours ago, it is planned.
#[test]
fn a_first_choice_younger_than_the_wait_is_held_and_an_older_one_planned() {
    let s = Scene::new(&[("Foo", "foo")], &[]);
    s.release("Foo", "foo", "v2", &[], "-- v2");
    s.plan(vec![update("Foo")], UpdatePolicy::Attended);
    let plan = s.plan(vec![update("Foo")], wait(6));
    assert_eq!(held(&plan), [("Foo".to_owned(), HeldReason::TooNew)]);
    assert!(changed(&plan).is_empty(), "nothing planned for Foo");
    let hold: &Held = &plan.targets[0].held[0];
    assert_eq!(
        (hold.version.as_str(), hold.release_id.as_str()),
        ("v2", "gh:o/foo@v2")
    );
    let (memo, _) = memo::load(&s.home.paths);
    let seen = memo
        .entries
        .values()
        .flat_map(|e| &e.seen_releases)
        .find(|s| s.release.tag == "v2")
        .unwrap()
        .first_seen_at;
    assert_eq!(
        hold.eligible_at,
        Some(seen + hours(6)),
        "eligible at first sight plus the wait"
    );

    s.age("foo", hours(7));
    let plan = s.plan(vec![update("Foo")], wait(6));
    assert!(held(&plan).is_empty(), "{:?}", held(&plan));
    assert_eq!(changed(&plan), ["Foo"]);
}

/// A settled update newly requires a library first seen ten minutes ago: neither is
/// planned, the update's hold is `dependency_held`, and an unrelated settled update in
/// the same run is planned. With the library seven hours old, all three are.
#[test]
fn a_new_dependency_younger_than_the_wait_holds_its_requester_alone() {
    let s = Scene::new(&[("Foo", "foo"), ("Bar", "bar")], &[("Lib", "lib")]);
    s.release("Foo", "foo", "v2", &["Lib"], "-- v2");
    s.release("Bar", "bar", "v2", &[], "-- v2");
    s.release("Lib", "lib", "v1", &[], "-- lib");
    let both = || vec![update("Foo"), update("Bar")];
    s.plan(both(), UpdatePolicy::Attended);
    s.age("foo", hours(7));
    s.age("bar", hours(7));
    s.age("lib", SignedDuration::from_mins(10));
    let plan = s.plan(both(), wait(6));
    assert_eq!(
        held(&plan),
        [
            ("Lib".to_owned(), HeldReason::TooNew),
            ("Foo".to_owned(), on("Lib"))
        ]
    );
    assert_eq!(changed(&plan), ["Bar"], "the unrelated update goes");
    let holds = &plan.targets[0].held;
    assert_eq!(
        holds[1].eligible_at, holds[0].eligible_at,
        "as long as the library waits"
    );

    s.age("lib", hours(7));
    let plan = s.plan(both(), wait(6));
    assert!(held(&plan).is_empty(), "{:?}", held(&plan));
    assert_eq!(changed(&plan), ["Bar", "Foo", "Lib"]);
}

/// Two settled updates both newly require the same too-new library: both are held.
#[test]
fn a_shared_too_new_dependency_holds_both_requesters() {
    let s = Scene::new(&[("Foo", "foo"), ("Baz", "baz")], &[("Lib", "lib")]);
    s.release("Foo", "foo", "v2", &["Lib"], "-- v2");
    s.release("Baz", "baz", "v2", &["Lib"], "-- v2");
    s.release("Lib", "lib", "v1", &[], "-- lib");
    let both = || vec![update("Foo"), update("Baz")];
    s.plan(both(), UpdatePolicy::Attended);
    s.age("foo", hours(7));
    s.age("baz", hours(7));
    let plan = s.plan(both(), wait(6));
    let mut got = held(&plan);
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        [
            ("Baz".to_owned(), on("Lib")),
            ("Foo".to_owned(), on("Lib")),
            ("Lib".to_owned(), HeldReason::TooNew),
        ]
    );
    assert!(changed(&plan).is_empty());
}

/// Bytes replaced under a release's tag are new: the archive downloaded now is not the
/// one the memo knew, so the release, seven hours old by its tag, is held.
#[test]
fn replaced_bytes_under_the_tag_hold_the_release() {
    let s = Scene::new(&[("Foo", "foo")], &[]);
    s.release("Foo", "foo", "v2", &[], "-- v2");
    s.plan(vec![update("Foo")], UpdatePolicy::Attended);
    s.age("foo", hours(7));
    let blobs = s.home.paths.blobs_dir();
    let uncache = || {
        if blobs.exists() {
            fs::remove_dir_all(&blobs).unwrap();
        }
    };
    // The same bytes downloaded again: seven hours old, planned.
    uncache();
    let plan = s.plan(vec![update("Foo")], wait(6));
    assert!(held(&plan).is_empty(), "{:?}", held(&plan));
    assert_eq!(changed(&plan), ["Foo"]);
    // The publisher replaces v2's archive; the cached one is gone, so it is downloaded.
    s.release("Foo", "foo", "v2", &[], "-- v2, rebuilt");
    uncache();
    let plan = s.plan(vec![update("Foo")], wait(6));
    assert_eq!(held(&plan), [("Foo".to_owned(), HeldReason::TooNew)]);
    assert!(changed(&plan).is_empty());
}

/// A look-back: the newest release has no build for retail and is old; the release the
/// look-back finds was first seen when the look-back was made, minutes ago: it is held.
#[test]
fn a_look_back_choice_first_seen_recently_is_held() {
    let s = Scene::new(&[], &[]);
    let base = "https://github.com/o/vuhdo";
    let publish = |tag: &str, flavor: &str, interface: u32| {
        let toc_text = toc(&interface.to_string(), &[&format!("## Version: {tag}")]);
        let file = format!("VuhDo-{tag}.zip");
        let flavors = [(flavor, interface)];
        let assets: [Asset<'_>; 1] = [(
            file.as_str(),
            &flavors,
            vec![
                ("VuhDo/VuhDo.toc", toc_text.as_str()),
                ("VuhDo/VuhDo.lua", "--"),
            ],
        )];
        gh_release_v(&s.home.scenario, base, tag, &assets);
        fixture(
            &s.home.scenario,
            &format!("rest-{tag}"),
            &format!("https://api.github.com/repos/o/vuhdo/releases/tags/{tag}"),
            200,
            &[("x-ratelimit-remaining", "59")],
            Some(
                &json!({"tag_name": tag, "prerelease": false, "draft": false, "assets": []})
                    .to_string(),
            ),
        );
    };
    publish("v3-titan", "titan", 38_002);
    publish("v2-main", "mainline", 120_105);
    gh_latest(&s.home.scenario, base, "v3-titan");
    gh_feed(
        &s.home.scenario,
        base,
        &[("v3-titan", "notes"), ("v2-main", "notes")],
        None,
    );
    let first = s.plan(vec![install("gh:o/vuhdo")], UpdatePolicy::Attended);
    assert_eq!(
        changed(&first),
        ["VuhDo"],
        "attended, the look-back installs v2"
    );
    s.age("vuhdo", hours(7));
    let plan = s.plan(vec![install("gh:o/vuhdo")], wait(6));
    let hold = &plan.targets[0].held;
    assert_eq!(
        held(&plan),
        [("VuhDo".to_owned(), HeldReason::TooNew)],
        "{hold:?}"
    );
    assert_eq!(hold[0].version, "v2-main");
    // The look-back made seven hours ago as well: its release has stood the wait.
    let path = s.home.paths.release_memo();
    let mut memo: ReleaseMemo = store::load::<ReleaseMemo>(&path).unwrap().unwrap().value;
    for look_back in memo.look_backs.values_mut() {
        look_back.at -= hours(7);
    }
    fs::write(&path, store::encode(&memo).unwrap()).unwrap();
    let plan = s.plan(vec![install("gh:o/vuhdo")], wait(6));
    assert!(held(&plan).is_empty(), "{:?}", held(&plan));
    assert_eq!(changed(&plan), ["VuhDo"]);
}
