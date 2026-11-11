//! The completion scripts' dynamic part: where clap's static script can offer only file
//! names (a client id, an addon name, a plan, backup, transaction, run or doctor run id, a
//! failure-mode id), a shell function asks `omaddons __complete` for the values
//! omaddons' own files hold (`app::complete`).
//!
//! Each place is a [`Slot`]: an argument of a command path, by its clap id, placed in the
//! live command tree when a script is generated, so a renamed flag fails generation (and
//! the unit tests) instead of shipping a script that completes the wrong word. The shell
//! code walks the words typed before the cursor as clap would: the command path (names
//! and aliases), each option that takes a value and that value, and the positionals
//! counted at the path reached. `-t` (or `--from`, for `copy`'s addons) names the client
//! whose addons or backups it offers.
//!
//! bash: a function that falls back to clap's `_omaddons`, registered after it. zsh:
//! clap's `_omaddons` renamed `_omaddons_static`, and a new `_omaddons` that falls back
//! to it, so the file's `#compdef` and its last lines work as before. fish: rules whose
//! condition is the same walk. elvish and PowerShell stay static.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::app::complete::{Kind, WORD};

/// Where a kind of value completes: the argument `arg` (clap's id) of the command at
/// `path`, its words joined by spaces; `""` is the root, whose global flags every command
/// takes.
struct Slot {
    path: &'static str,
    arg: &'static str,
    kind: Kind,
}

const fn slot(path: &'static str, arg: &'static str, kind: Kind) -> Slot {
    Slot { path, arg, kind }
}

/// Every place a value completes from omaddons' files.
/// A client id is a `client` (the discovery cache's unregistered clients too) wherever a
/// provisional id works: `-t`, `copy --to`, and `targets rename` and `default`, which
/// register it. `copy --from` needs a record to copy from and `targets rm` a registration
/// to remove, so they offer registered ids only.
const SLOTS: &[Slot] = &[
    slot("", "target", Kind::Client),
    slot("copy", "from", Kind::Target),
    slot("copy", "to", Kind::Client),
    slot("targets rm", "id", Kind::Target),
    slot("targets rename", "id", Kind::Client),
    slot("targets default", "id", Kind::Client),
    slot("update", "addons", Kind::Addon),
    slot("remove", "addons", Kind::Addon),
    slot("pin", "addon", Kind::Addon),
    slot("unpin", "addon", Kind::Addon),
    slot("copy", "addons", Kind::Addon),
    slot("errors", "addon", Kind::Addon),
    slot("backup restore", "addon", Kind::Addon),
    slot("apply", "plan_id", Kind::Plan),
    slot("plans show", "plan_id", Kind::Plan),
    slot("backup list", "id", Kind::Backup),
    slot("backup restore", "id", Kind::Backup),
    slot("backup prune", "ids", Kind::Backup),
    slot("backup export", "id", Kind::Backup),
    slot("backup verify", "ids", Kind::Backup),
    slot("undo", "tx", Kind::Tx),
    slot("undo", "run", Kind::Run),
    slot("history", "run", Kind::Run),
    slot("doctor", "since", Kind::DoctorRun),
    slot("doctor diagnose", "since", Kind::DoctorRun),
    slot("doctor diff", "run", Kind::DoctorRun),
    slot("doctor undo", "run", Kind::DoctorRun),
    slot("doctor explain", "id", Kind::Fm),
    slot("doctor", "only", Kind::Fm),
    slot("doctor", "skip", Kind::Fm),
    slot("doctor diagnose", "only", Kind::Fm),
    slot("doctor diagnose", "skip", Kind::Fm),
    slot("doctor fix", "only", Kind::Fm),
    slot("doctor fix", "skip", Kind::Fm),
];

/// The path whose addon slot lists the addons of the client its `--from` names (the
/// client it copies from), not `-t`'s.
const FROM_PATH: &str = "copy";
/// The clap id of [`FROM_PATH`]'s option naming that client.
const FROM_ARG: &str = "from";

/// A subcommand: the words that reach it from its parent (its name and every alias).
struct Child {
    parent: String,
    words: Vec<String>,
    path: String,
}

/// A positional slot: its 0-based place among the path's positionals, and whether it
/// takes every positional from there on.
struct Positional {
    path: String,
    index: usize,
    many: bool,
    kind: Kind,
}

/// The command tree as the walk needs it, and every slot placed in it.
struct Tree {
    children: Vec<Child>,
    /// Every spelling of every option that takes a value (`-t`, `--target`, …).
    value_options: BTreeSet<String>,
    /// The root's option slots (`-t`): (spellings, kind), on every path.
    global: Vec<(Vec<String>, Kind)>,
    /// Option slots on a path: (path, spellings, kind).
    options: Vec<(String, Vec<String>, Kind)>,
    positionals: Vec<Positional>,
    /// `-t`'s spellings, whose value names the client.
    target: Vec<String>,
    /// [`FROM_ARG`]'s spellings.
    from: Vec<String>,
}

/// An argument's spellings: `-x` for its short and short aliases, `--xx` for its long
/// and long aliases.
fn spellings(arg: &clap::Arg) -> Vec<String> {
    let mut words: Vec<String> = arg
        .get_short()
        .into_iter()
        .chain(arg.get_all_short_aliases().unwrap_or_default())
        .map(|c| format!("-{c}"))
        .collect();
    words.extend(
        arg.get_long()
            .into_iter()
            .chain(arg.get_all_aliases().unwrap_or_default())
            .map(|long| format!("--{long}")),
    );
    words
}

/// Every word the scripts embed is a plain word of letters, digits and `-`, so no shell
/// reads it as anything but itself (no quoting, globbing or expansion).
fn plain(word: &str) -> bool {
    !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn join(path: &str, word: &str) -> String {
    if path.is_empty() {
        word.to_owned()
    } else {
        format!("{path} {word}")
    }
}

/// Collects `cmd`'s value-taking options and its subcommands, recursively.
fn walk(cmd: &clap::Command, path: &str, tree: &mut Tree) {
    for arg in cmd.get_arguments() {
        if !arg.is_positional() && arg.get_action().takes_values() {
            tree.value_options.extend(spellings(arg));
        }
    }
    for sub in cmd.get_subcommands() {
        let child = join(path, sub.get_name());
        let words = std::iter::once(sub.get_name())
            .chain(sub.get_all_aliases())
            .map(str::to_owned)
            .collect();
        tree.children.push(Child {
            parent: path.to_owned(),
            words,
            path: child.clone(),
        });
        walk(sub, &child, tree);
    }
}

/// The command at `path` under `root`.
fn command_at<'a>(root: &'a clap::Command, path: &str) -> Option<&'a clap::Command> {
    path.split(' ')
        .filter(|word| !word.is_empty())
        .try_fold(root, |cmd, word| cmd.find_subcommand(word))
}

/// The tree of `root` (built), every slot placed in it; the first slot or word that does
/// not fit, as an error.
fn tree(root: &clap::Command) -> Result<Tree, String> {
    let mut tree = Tree {
        children: Vec::new(),
        value_options: BTreeSet::new(),
        global: Vec::new(),
        options: Vec::new(),
        positionals: Vec::new(),
        target: Vec::new(),
        from: Vec::new(),
    };
    walk(root, "", &mut tree);
    for slot in SLOTS {
        let cmd =
            command_at(root, slot.path).ok_or_else(|| format!("no command at {:?}", slot.path))?;
        let arg = cmd
            .get_arguments()
            .find(|arg| arg.get_id() == slot.arg)
            .ok_or_else(|| format!("{:?} has no argument {:?}", slot.path, slot.arg))?;
        if arg.is_positional() {
            let index = arg
                .get_index()
                .and_then(|i| i.checked_sub(1))
                .ok_or_else(|| format!("{:?} {:?} has no index", slot.path, slot.arg))?;
            let many = arg.get_num_args().is_some_and(|n| n.max_values() > 1);
            tree.positionals.push(Positional {
                path: slot.path.to_owned(),
                index,
                many,
                kind: slot.kind,
            });
        } else if arg.get_action().takes_values() {
            let words = spellings(arg);
            if slot.path.is_empty() {
                tree.target = words.clone();
                tree.global.push((words, slot.kind));
            } else {
                if slot.path == FROM_PATH && slot.arg == FROM_ARG {
                    tree.from = words.clone();
                }
                tree.options.push((slot.path.to_owned(), words, slot.kind));
            }
        } else {
            return Err(format!("{:?} {:?} takes no value", slot.path, slot.arg));
        }
    }
    if tree.target.is_empty() || tree.from.is_empty() {
        return Err("no -t or no copy --from slot".to_owned());
    }
    let mut words = tree
        .children
        .iter()
        .flat_map(|c| c.words.iter().map(String::as_str))
        .chain(tree.value_options.iter().map(|w| w.trim_start_matches('-')));
    if let Some(word) = words.find(|w| !plain(w)) {
        return Err(format!("{word:?} is not a plain word"));
    }
    Ok(tree)
}

/// The shell a glue is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

/// The dynamic part of `shell`'s script for the built command tree `root`.
pub fn glue(shell: Shell, root: &clap::Command) -> Result<String, String> {
    let tree = tree(root)?;
    Ok(match shell {
        Shell::Bash => bash(&tree),
        Shell::Zsh => zsh(&tree),
        Shell::Fish => fish(&tree),
    })
}

/// The comment heading every shell's glue.
const HEADER: &str = "\
# Values from omaddons' own files, where the script above offers only file names:
# client ids, addon names, and plan, backup, transaction, run, doctor run and
# failure-mode ids. The hidden reader, omaddons __complete, reads them without scanning,
# locking, writing or going online, and prints nothing when it has nothing to offer.
";

/// `a|b|c`, each word quoted when `quote`.
fn alternatives(words: &[String], quote: bool) -> String {
    let words: Vec<String> = words
        .iter()
        .map(|w| if quote { format!("\"{w}\"") } else { w.clone() })
        .collect();
    words.join("|")
}

/// The walk over the words before the cursor, shared by bash and zsh (`word` is set by
/// the loop around it): it leaves `cmdpath`, `value_of` (the option whose value the next
/// word is), `n` (positionals at `cmdpath`), `target` and `from`.
fn sh_walk_body(tree: &Tree, out: &mut String) {
    let target = alternatives(&tree.target, false);
    let from = alternatives(&tree.from, false);
    let value_options: Vec<String> = tree.value_options.iter().cloned().collect();
    let _ = write!(
        out,
        "        if [[ -n $value_of ]]; then
            [[ $word == \"=\" ]] && continue
            case $value_of in
                {target}) target=$word ;;
                {from}) from=$word ;;
            esac
            value_of=
            continue
        fi
        if (( ! rest )); then
            case $word in
                --) rest=1; continue ;;
                --*=*)
                    case ${{word%%=*}} in
                        {target}) target=${{word#*=}} ;;
                        {from}) from=${{word#*=}} ;;
                    esac
                    continue ;;
                -t?*) target=${{word#-t}}; continue ;;
                -*)
                    case $word in
                        {}) value_of=$word ;;
                    esac
                    continue ;;
            esac
            case \"$cmdpath,$word\" in
",
        alternatives(&value_options, false)
    );
    for child in &tree.children {
        let keys: Vec<String> = child
            .words
            .iter()
            .map(|w| format!("{},{w}", child.parent))
            .collect();
        let _ = writeln!(
            out,
            "                {}) cmdpath=\"{}\"; n=0; continue ;;",
            alternatives(&keys, true),
            child.path
        );
    }
    out.push_str(
        "            esac
        fi
        n=$((n + 1))
",
    );
}

/// The kind at the cursor, shared by bash and zsh: sets `kind` from `cmdpath`,
/// `value_of`, `n` and `cur`.
fn sh_kind(tree: &Tree, out: &mut String) {
    out.push_str(
        "    if [[ -n $value_of ]]; then
        case \"$cmdpath $value_of\" in
",
    );
    for (path, words, kind) in &tree.options {
        let keys: Vec<String> = words.iter().map(|w| format!("{path} {w}")).collect();
        let _ = writeln!(
            out,
            "            {}) kind={} ;;",
            alternatives(&keys, true),
            kind.word()
        );
    }
    out.push_str("            *)\n                case $value_of in\n");
    for (words, kind) in &tree.global {
        let _ = writeln!(
            out,
            "                    {}) kind={} ;;",
            alternatives(words, false),
            kind.word()
        );
    }
    out.push_str(
        "                esac ;;
        esac
    elif [[ $cur != -* ]]; then
        case $cmdpath in
",
    );
    for (path, slots) in positionals_by_path(tree) {
        let mut arms = Vec::new();
        for slot in slots {
            let test = if slot.many { ">=" } else { "==" };
            arms.push(format!(
                "(( n {test} {} )); then kind={}",
                slot.index,
                slot.kind.word()
            ));
        }
        let _ = writeln!(
            out,
            "            \"{path}\") if {}; fi ;;",
            arms.join("; elif ")
        );
    }
    out.push_str("        esac\n    fi\n");
}

/// The positional slots grouped by path, each path's in index order.
fn positionals_by_path(tree: &Tree) -> BTreeMap<&str, Vec<&Positional>> {
    let mut by_path: BTreeMap<&str, Vec<&Positional>> = BTreeMap::new();
    for slot in &tree.positionals {
        by_path.entry(slot.path.as_str()).or_default().push(slot);
    }
    for slots in by_path.values_mut() {
        slots.sort_by_key(|slot| slot.index);
    }
    by_path
}

/// The declarations and the `--opt=value` handling of the current word, shared by bash
/// and zsh; `cur` is set before it.
const SH_LOCALS: &str =
    "    local word cmdpath= value_of= target= from= kind= eq= client i n=0 rest=0\n";

fn sh_current_word(out: &mut String) {
    out.push_str(
        "    [[ $cur == \"=\" ]] && cur=
    if [[ -z $value_of && $cur == --*=* ]]; then
        value_of=${cur%%=*}
        eq=\"$value_of=\"
        cur=${cur#*=}
    fi
",
    );
}

fn bash(tree: &Tree) -> String {
    let mut out = format!("\n{HEADER}_omaddons_dynamic() {{\n    local cur=\"$2\"\n");
    out.push_str(SH_LOCALS);
    out.push_str(
        "    for (( i = 1; i < COMP_CWORD; i++ )); do\n        word=\"${COMP_WORDS[i]}\"\n",
    );
    sh_walk_body(tree, &mut out);
    out.push_str("    done\n");
    sh_current_word(&mut out);
    sh_kind(tree, &mut out);
    let _ = write!(
        out,
        "    if [[ -n $kind && $cur != [/.~]* ]]; then
        client=$target
        [[ $cmdpath == {FROM_PATH} ]] && client=$from
        mapfile -t COMPREPLY < <(omaddons {WORD} \"$kind\" ${{client:+\"--target=$client\"}} \"--prefix=$cur\" 2>/dev/null)
        if (( ${{#COMPREPLY[@]}} )); then
            [[ -n $eq ]] && COMPREPLY=(\"${{COMPREPLY[@]/#/$eq}}\")
            return 0
        fi
    fi
    _omaddons \"$@\"
}}

if [[ \"${{BASH_VERSINFO[0]}}\" -eq 4 && \"${{BASH_VERSINFO[1]}}\" -ge 4 || \"${{BASH_VERSINFO[0]}}\" -gt 4 ]]; then
    complete -F _omaddons_dynamic -o nosort -o bashdefault -o default omaddons
else
    complete -F _omaddons_dynamic -o bashdefault -o default omaddons
fi
"
    );
    out
}

fn zsh(tree: &Tree) -> String {
    let mut out = format!("{HEADER}_omaddons() {{\n    local cur=$PREFIX\n");
    out.push_str(SH_LOCALS);
    out.push_str("    for (( i = 2; i < CURRENT; i++ )); do\n        word=${words[i]}\n");
    sh_walk_body(tree, &mut out);
    out.push_str("    done\n");
    sh_current_word(&mut out);
    sh_kind(tree, &mut out);
    let _ = write!(
        out,
        "    if [[ -n $kind && $cur != [/.~]* ]]; then
        client=$target
        [[ $cmdpath == {FROM_PATH} ]] && client=$from
        local -a found
        found=(${{(f)\"$(omaddons {WORD} $kind ${{client:+--target=$client}} 2>/dev/null)\"}})
        if (( ${{#found}} )); then
            [[ -n $eq ]] && compset -P '*='
            [[ $kind == fm ]] && compset -P '*,'
            compadd -a found
            return 0
        fi
    fi
    _omaddons_static \"$@\"
}}

"
    );
    out
}

fn fish(tree: &Tree) -> String {
    let target = tree.target.join(" ");
    let from = tree.from.join(" ");
    let value_options: Vec<String> = tree.value_options.iter().cloned().collect();
    let mut out = format!("\n{HEADER}");
    let _ = write!(
        out,
        "function __fish_omaddons_slot --description 'What omaddons completes at the cursor: its kind, option or positional, and the client'
    set -l words (commandline -opc)
    set -e words[1]
    set -l cur (commandline -ct)
    set -l cmdpath ''
    set -l value_of ''
    set -l target ''
    set -l from ''
    set -l n 0
    set -l rest 0
    for word in $words
        if test -n \"$value_of\"
            if contains -- $value_of {target}
                set target $word
            else if contains -- $value_of {from}
                set from $word
            end
            set value_of ''
            continue
        end
        if test $rest -eq 0
            if test \"x$word\" = x--
                set rest 1
                continue
            else if string match -q -- '--*=*' $word
                set -l pair (string split -m 1 = -- $word)
                if contains -- $pair[1] {target}
                    set target $pair[2]
                else if contains -- $pair[1] {from}
                    set from $pair[2]
                end
                continue
            else if string match -qr -- '^-t.' $word
                set target (string sub -s 3 -- $word)
                continue
            else if string match -q -- '-*' $word
                if contains -- $word {}
                    set value_of $word
                end
                continue
            end
            switch \"$cmdpath,$word\"
",
        value_options.join(" ")
    );
    for child in &tree.children {
        let keys: Vec<String> = child
            .words
            .iter()
            .map(|w| format!("'{},{w}'", child.parent))
            .collect();
        let _ = write!(
            out,
            "                case {}
                    set cmdpath '{}'
                    set n 0
                    continue
",
            keys.join(" "),
            child.path
        );
    }
    out.push_str(
        "            end
        end
        set n (math $n + 1)
    end
    set -l where option
    if test -z \"$value_of\"
        set where positional
        if string match -q -- '--*=*' $cur
            set value_of (string split -m 1 = -- $cur)[1]
            set where option
        end
    end
    set -l kind ''
    if test -n \"$value_of\"
        switch \"$cmdpath $value_of\"
",
    );
    for (path, words, kind) in &tree.options {
        let keys: Vec<String> = words.iter().map(|w| format!("'{path} {w}'")).collect();
        let _ = write!(
            out,
            "            case {}
                set kind {}
",
            keys.join(" "),
            kind.word()
        );
    }
    out.push_str("            case '*'\n");
    for (at, (words, kind)) in tree.global.iter().enumerate() {
        let keyword = if at == 0 { "if" } else { "else if" };
        let _ = write!(
            out,
            "                {keyword} contains -- $value_of {}
                    set kind {}
",
            words.join(" "),
            kind.word()
        );
    }
    if !tree.global.is_empty() {
        out.push_str("                end\n");
    }
    out.push_str(
        "        end
    else if not string match -q -- '-*' $cur
        switch \"$cmdpath\"
",
    );
    for (path, slots) in positionals_by_path(tree) {
        let _ = writeln!(out, "            case '{path}'");
        for (at, slot) in slots.iter().enumerate() {
            let test = if slot.many { "-ge" } else { "-eq" };
            let keyword = if at == 0 { "if" } else { "else if" };
            let _ = write!(
                out,
                "                {keyword} test $n {test} {}
                    set kind {}
",
                slot.index,
                slot.kind.word()
            );
        }
        out.push_str("                end\n");
    }
    let _ = write!(
        out,
        "        end
    end
    test -n \"$kind\"; or return 1
    set -l client $target
    test \"$cmdpath\" = {FROM_PATH}; and set client $from
    printf '%s\\n' $kind $where
    test -n \"$client\"; and printf '%s\\n' $client
    return 0
end

function __fish_omaddons_at --description 'Whether omaddons completes a value at the cursor, as an option value or as a positional'
    set -l slot (__fish_omaddons_slot); or return 1
    test \"$slot[2]\" = $argv[1]
end

function __fish_omaddons_values --description 'The values omaddons offers at the cursor'
    set -l slot (__fish_omaddons_slot); or return
    # A comma-separated list of failure-mode ids completes its last item, the rest kept:
    # the reader is given the word, an --option= part aside (fish puts that back).
    set -l prefix
    if test \"$slot[1]\" = fm
        set -l word (commandline -ct)
        set word (string replace -r -- '^--[^=]*=' '' \"$word\")
        string match -qr ',' -- \"$word\"; and set prefix \"--prefix=$word\"
    end
    if set -q slot[3]
        omaddons {WORD} $slot[1] --target=$slot[3] $prefix 2>/dev/null
    else
        omaddons {WORD} $slot[1] $prefix 2>/dev/null
    end
end

complete -c omaddons -n '__fish_omaddons_at positional' -f -a '(__fish_omaddons_values)'
"
    );
    // One rule per option the slots name, so fish offers the values as that option's
    // argument; files stay offered for a client, which a path may name.
    let mut rules: BTreeMap<Vec<String>, bool> = BTreeMap::new();
    for (words, kind) in tree.global.iter().cloned().chain(
        tree.options
            .iter()
            .map(|(_, words, kind)| (words.clone(), *kind)),
    ) {
        let files = rules.entry(words).or_insert(true);
        *files &= matches!(kind, Kind::Target | Kind::Client);
    }
    for (words, files) in rules {
        let flags: Vec<String> = words
            .iter()
            .map(|w| match w.strip_prefix("--") {
                Some(long) => format!("-l {long}"),
                None => format!("-s {}", w.trim_start_matches('-')),
            })
            .collect();
        let _ = writeln!(
            out,
            "complete -c omaddons -n '__fish_omaddons_at option' {} -r{} -a '(__fish_omaddons_values)'",
            flags.join(" "),
            if files { "" } else { " -f" }
        );
    }
    out
}

#[cfg(test)]
mod tests;
