//! SavedVariables: any bytes parse within every bound (the node cap, the issue cap and
//! the linear step bound), the same bytes always parse the same, and nothing is built
//! when no global is asked for.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omaddons::limits::{LUA_MAX_ISSUES, LUA_MAX_NODES};
use omaddons::lua::{parse_saved_variables, parse_with_stats, step_bound};

fuzz_target!(|data: &[u8]| {
    let wanted = ["BugGrabberDB", "X"];
    let (parsed, stats) = parse_with_stats(data, &wanted);
    assert!(stats.nodes <= LUA_MAX_NODES);
    assert!(parsed.issues.len() <= LUA_MAX_ISSUES);
    assert!(stats.steps <= step_bound(data.len()));
    // NaN never equals itself, so compare what was read as text.
    let again = parse_saved_variables(data, &wanted);
    assert_eq!(format!("{parsed:?}"), format!("{again:?}"));
    let (skipped, stats) = parse_with_stats(data, &[]);
    assert!(stats.nodes == 0 && skipped.globals.is_empty());
});
