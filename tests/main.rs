//! The one integration test binary. Every suite under `tests/` is a module of it: each
//! `tests/<suite>.rs`, and `tests/contract/main.rs` for the contract suite. A binary of
//! its own per suite linked the whole library ~130 times, so a one-line change wrote
//! ~16 GB to the disk; one binary links it once.
//!
//! A suite's tests are named `<suite>::…` (`cargo test --test integration skill::`), the
//! test logs are under `target/tmp/test-logs/<suite>/`, and `argc e2e` selects suites by
//! these module names. The modules every suite shares are declared here once: `common`,
//! the doctor's fixtures (`fixture`) and the contract's schema validator (`validate`).

mod common;
// Test-only, as in the suites that included it (`#![cfg(test)]`), so clippy's
// unwrap-in-tests allowance covers the fixtures.
#[cfg(test)]
#[path = "doctor/mod.rs"]
mod fixture;
#[path = "contract/validate.rs"]
mod validate;

mod adopt;
mod adopt_hints;
mod adopt_identify;
mod apply;
mod apply_crash;
mod auth;
mod backup_auto;
mod backup_create;
mod backup_export;
mod backup_import;
mod backup_list;
mod backup_prune;
mod backup_verify;
mod barrier_crash;
mod blobs;
mod capabilities;
mod catalog;
mod changelog;
mod cli_config;
mod cli_skeleton;
mod common_selftest;
mod compat_m2;
mod compat_m3;
mod compat_v021;
mod completions;
#[path = "contract/main.rs"]
mod contract;
mod copy;
mod deps;
mod desired;
mod determinism;
mod discover_cache;
mod discover_custom;
mod discover_launchers;
mod discover_matrix;
mod discover_scan;
mod display_names;
// `tests/doctor/` is also the fixtures' directory (`mod.rs`), so the suite is named by path.
#[path = "doctor.rs"]
mod doctor;
mod doctor_caches;
mod doctor_concurrency;
mod doctor_plugins_secrets;
mod doctor_repair;
mod doctor_repair_fences;
mod doctor_round_trip;
mod doctor_view;
mod doorbell;
mod drift;
mod drift_surfaces;
mod e2e_manifest;
mod e2e_misplaced_clients;
mod e2e_plan_retention;
mod e2e_slice2;
mod e2e_status_issues;
mod e2e_target_typos;
mod engine_concurrency;
mod engine_crash;
mod engine_fallbacks;
mod engine_multi_target;
mod engine_undo;
mod engine_witness;
mod error_details;
mod errors;
mod errors_decode;
mod errors_dismiss;
mod errors_privacy;
mod export;
mod fail_closed;
mod files_index;
mod fsx_lock;
mod fsx_predicates;
mod fsx_prune;
mod fsx_scope;
mod fuzz_seeds;
mod game_modes;
mod git_github;
mod help_pages;
mod history;
mod history_items;
mod info_about;
mod info_netlog;
mod info_releases;
mod inspect;
mod install;
mod install_any_flavor;
mod install_first_launch;
mod install_look_back;
mod install_multi_flavor;
mod inventory;
mod journal;
mod list;
mod live_probes;
mod lua;
mod multi_addon;
mod net_replay;
mod omarchy;
mod omarchy_widget;
mod outdated;
mod ownership;
mod perf;
mod pin;
mod plan;
mod plans_show;
mod plans_show_commands;
mod preconditions;
mod preconditions_matrix;
mod purge;
mod readme;
mod real_machine;
mod registry;
mod release_cache;
mod remove;
mod restore;
mod restore_addon;
mod restore_undo;
mod retention_sweep;
mod robot_docs;
mod running;
mod search;
mod shared_addons;
mod single_writer;
mod skill;
mod sources_github;
mod sources_github_rest;
mod sources_tukui;
mod sources_wowace;
mod sources_wowi;
mod stage;
mod status;
mod status_errors;
mod sync;
mod target_selection;
mod targets;
mod terminal;
mod text;
mod tlog_selftest;
mod toc_loaded;
mod toc_matrix;
mod undo_history;
mod uninstalled_client;
mod update;
mod witness;
mod wowi_index;
