//! The blob cache across threads and transactions: concurrent stores of the same
//! content, and the orphan sweep of dead transactions' temp dirs.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;

use common::env::TestEnv;
use common::http::paths;
use omaddons::engine::blobs::{BlobCache, Storage};
use omaddons::fsx::{InflightLock, ScopeTable};
use omaddons::model::TxId;

#[test]
fn concurrent_stores_of_the_same_content_end_as_one_blob() {
    let env = TestEnv::new();
    let paths = paths(&env);
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    let cache = BlobCache::new(&paths);
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let refs: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    cache
                        .store_stream(
                            &scopes,
                            &TxId::new(),
                            Cursor::new(bytes.clone()),
                            None,
                            &Storage::Cache,
                        )
                        .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(refs.windows(2).all(|w| w[0] == w[1]), "{refs:?}");
    let blobs: Vec<String> = fs::read_dir(paths.blobs_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.len() == 64)
        .collect();
    assert_eq!(blobs, [refs[0].sha256.clone()]);
    let temp_files = fs::read_dir(paths.cache_dir().join("tmp"))
        .unwrap()
        .flat_map(|tx| fs::read_dir(tx.unwrap().path()).unwrap())
        .count();
    assert_eq!(temp_files, 0, "no temp file survives a store");
}

#[test]
fn the_orphan_sweep_spares_live_transactions() {
    let env = TestEnv::new();
    let paths = paths(&env);
    let scopes = ScopeTable::new(&paths, &[], None).unwrap();
    let (dead, live) = (TxId::new(), TxId::new());
    for tx in [dead, live] {
        let dir = paths.cache_tmp(&tx);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("blob-0"), "partial").unwrap();
    }
    let _held = InflightLock::create(&paths, &live).unwrap();
    let report = BlobCache::new(&paths)
        .evict(&scopes, &BTreeSet::new(), u64::MAX)
        .unwrap();
    assert_eq!(report.orphans_removed, 1);
    assert!(
        !paths.cache_tmp(&dead).exists(),
        "the dead transaction's temp dir is gone"
    );
    assert!(
        paths.cache_tmp(&live).join("blob-0").is_file(),
        "a live transaction keeps its download"
    );
    assert!(report.evicted.is_empty() && !report.over_cap);
}
