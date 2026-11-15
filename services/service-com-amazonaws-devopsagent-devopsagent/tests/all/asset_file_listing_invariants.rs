// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests pinning the two invariants of `ListAssetFiles` that the
//! listing's own cases do not cover: that listing an asset changes nothing a later
//! caller sees, and that a page cursor names a POSITION in the path order rather
//! than an index into one snapshot of it, so a walk stays correct while files are
//! created and deleted around it.
//!
//! Every case drives the service through the same router the emulator serves, and
//! reuses the listing suite's own wire helpers, so the routes, status codes,
//! protocol envelope and state are all exercised.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_file_listing::{
    add_file, asset_file_uri, call, create_asset, create_space, items, list_files, new_router,
    paths, rows, seed,
};

/// Read one asset through `GetAsset`, asserting a 200, and return its body.
async fn get_asset(router: &MultiServiceRouter, space: &str, asset: &str, case: &str) -> Value {
    let uri = format!("/asset/agent-space/{space}/assets/{asset}");
    let (status, body) = call(router, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetAsset {uri}, got {status}: {body}"
    );
    body
}

/// Read one asset's revision list through `ListAssetVersions`, asserting a 200,
/// and return its body.
async fn list_versions(router: &MultiServiceRouter, space: &str, asset: &str, case: &str) -> Value {
    let uri = format!("/asset/agent-space/{space}/assets/{asset}/versions");
    let (status, body) = call(router, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ListAssetVersions {uri}, got {status}: {body}"
    );
    body
}

/// Read one file of one asset through `GetAssetFile`, asserting a 200, and return
/// its body.
async fn get_file(
    router: &MultiServiceRouter,
    space: &str,
    asset: &str,
    path: &str,
    case: &str,
) -> Value {
    let uri = asset_file_uri(space, asset, &path.replace(' ', "%20"));
    let (status, body) = call(router, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetAssetFile {uri}, got {status}: {body}"
    );
    body
}

/// Everything else a caller can read about one asset: the asset itself, its
/// revision list, and each file it holds.
async fn readable_state(
    router: &MultiServiceRouter,
    space: &str,
    asset: &str,
    held: &[String],
    case: &str,
) -> Value {
    let mut files = serde_json::Map::new();
    for path in held {
        files.insert(
            path.clone(),
            get_file(router, space, asset, path, case).await,
        );
    }
    json!({
        "asset": get_asset(router, space, asset, case).await,
        "versions": list_versions(router, space, asset, case).await,
        "files": Value::Object(files),
    })
}

/// Walk a listing from `query` to its end at `page_size`, and return every path it
/// served, in order, asserting no page is longer than the page size and that a
/// `nextToken` only ever follows a full page.
async fn walk(
    router: &MultiServiceRouter,
    space: &str,
    asset: &str,
    case: &str,
    page_size: usize,
) -> Vec<String> {
    let mut served: Vec<String> = Vec::new();
    let mut query = format!("?maxResults={page_size}");
    // A walk of a bounded asset cannot need more pages than it holds files, so a
    // generous page budget bounds the loop without bounding the listing.
    for _ in 0..512 {
        let body = list_files(router, space, asset, case, &query).await;
        let page = paths(case, &body);
        assert!(
            page.len() <= page_size,
            "case {case:?}: a page carried {} files: {page:?}",
            page.len()
        );
        served.extend(page.clone());
        match body.get("nextToken").and_then(Value::as_str) {
            Some(token) => {
                assert_eq!(
                    page.len(),
                    page_size,
                    "case {case:?}: a short page carried a nextToken: {page:?}"
                );
                query = format!("?maxResults={page_size}&nextToken={token}");
            }
            None => return served,
        }
    }
    panic!("case {case:?}: the walk did not end, having served {served:?}");
}

/// RULE: `ListAssetFiles` reports state and writes none, so listing an asset — once,
/// again, or page by page — leaves every later read of it (the asset's own revision,
/// its revision history, and each file's content, version and timestamps) reporting
/// exactly what it reported before the listing.
#[tokio::test]
async fn listing_an_asset_changes_nothing_a_later_caller_sees() {
    let router = new_router();
    let space = create_space(&router, "asset-file-listing-is-a-read").await;

    // `maxResults` carries `@range(min: 1, max: 100)`
    // (`smithy_query(query="operation", operation="ListAssetFiles")`), so the walk
    // that must change nothing is driven at both published bounds and between them.
    for page_size in [1usize, 2, 100] {
        for row in rows() {
            let (asset, held) = seed(&router, &space, &row).await;
            let case = format!("{} listed at maxResults={page_size}", row.case);

            // Snapshotted BEFORE any listing is issued, so a write made on the
            // FIRST read — a default materialised on first access, a timestamp
            // touched once — is not baked into the baseline.
            let before = readable_state(&router, &space, &asset, &held, &case).await;
            let before_listing = list_files(&router, &space, &asset, &case, "").await;

            // List it again, every way a caller can: whole, paged to the end, and
            // at the revision the asset is at.
            let again = list_files(&router, &space, &asset, &case, "").await;
            assert_eq!(
                again, before_listing,
                "case {case:?}: a repeated listing reported {again} after {before_listing}"
            );
            let walked = walk(&router, &space, &asset, &case, page_size).await;
            assert_eq!(
                walked, held,
                "case {case:?}: the paged walk served {walked:?}"
            );
            let asset_read = &before["asset"];
            let version = asset_read["asset"]["version"]
                .as_i64()
                .unwrap_or_else(|| panic!("case {case:?}: no asset version in {asset_read}"));
            let at_version = list_files(
                &router,
                &space,
                &asset,
                &case,
                &format!("?assetVersion={version}"),
            )
            .await;
            assert_eq!(
                at_version, before_listing,
                "case {case:?}: the listing at version {version} reported {at_version}"
            );

            let after = readable_state(&router, &space, &asset, &held, &case).await;
            assert_eq!(
                after, before,
                "case {case:?}: reads after the listings reported {after} where they had reported {before}"
            );
        }
    }
}

/// One walk interrupted by a change: the paths the asset starts with, the change
/// made after its first page, and the paths the whole walk then serves.
struct Interrupted {
    case: &'static str,
    /// What to do after the first page: create or delete one path.
    change: Change,
    /// Every path the walk serves across all of its pages, in order.
    serves: &'static [&'static str],
    /// Every path the asset holds when the walk ends, in path order.
    holds: &'static [&'static str],
}

/// The change made to an asset in the middle of a walk of its files.
enum Change {
    Create(&'static str),
    Delete(&'static str),
}

/// RULE: a page cursor names a position in the listing's path order, not an index
/// into the page it was issued from, so a walk resumes at the next path the asset
/// still holds after that position: a file created or deleted at or before the
/// cursor leaves the rest of the walk where it was, a file deleted ahead of it is
/// not served, and one created ahead of it is — each remaining file exactly once,
/// in path order.
#[tokio::test]
async fn a_walk_resumes_after_its_cursor_while_files_change_around_it() {
    let router = new_router();
    let space = create_space(&router, "asset-file-cursor-stability").await;

    // Every case starts from the same five files and the same first page
    // (`["a.md", "b.md"]` at `maxResults=2`), so the cursor is always `"b.md"`
    // and the rows differ only in WHERE the change falls relative to it.
    const START: [&str; 5] = ["a.md", "b.md", "c.md", "d.md", "e.md"];
    let cases = [
        Interrupted {
            case: "the cursor's own file is deleted",
            change: Change::Delete("b.md"),
            serves: &["a.md", "b.md", "c.md", "d.md", "e.md"],
            holds: &["a.md", "c.md", "d.md", "e.md"],
        },
        Interrupted {
            case: "a file before the cursor is deleted",
            change: Change::Delete("a.md"),
            serves: &["a.md", "b.md", "c.md", "d.md", "e.md"],
            holds: &["b.md", "c.md", "d.md", "e.md"],
        },
        Interrupted {
            case: "the file just after the cursor is deleted",
            change: Change::Delete("c.md"),
            serves: &["a.md", "b.md", "d.md", "e.md"],
            holds: &["a.md", "b.md", "d.md", "e.md"],
        },
        Interrupted {
            case: "a file a page beyond the cursor is deleted",
            change: Change::Delete("e.md"),
            serves: &["a.md", "b.md", "c.md", "d.md"],
            holds: &["a.md", "b.md", "c.md", "d.md"],
        },
        Interrupted {
            case: "a file is created after the cursor",
            change: Change::Create("bb.md"),
            serves: &["a.md", "b.md", "bb.md", "c.md", "d.md", "e.md"],
            holds: &["a.md", "b.md", "bb.md", "c.md", "d.md", "e.md"],
        },
        Interrupted {
            case: "a file is created before the cursor",
            change: Change::Create("aa.md"),
            serves: &["a.md", "b.md", "c.md", "d.md", "e.md"],
            holds: &["a.md", "aa.md", "b.md", "c.md", "d.md", "e.md"],
        },
    ];

    for row in cases {
        let case = row.case;
        // An asset imported from a `sourceUrl` holds a SET of files, so the files
        // of this walk can be created and deleted without the asset itself going
        // with the last of them.
        let asset = create_asset(
            &router,
            &space,
            case,
            json!({"sourceUrl": {"url": "https://example.com/bundle.zip"}}),
        )
        .await;
        for path in START {
            add_file(&router, &space, &asset, case, path).await;
        }

        let first = list_files(&router, &space, &asset, case, "?maxResults=2").await;
        assert_eq!(
            paths(case, &first),
            vec!["a.md".to_string(), "b.md".to_string()],
            "case {case:?}: the first page served {:?}",
            paths(case, &first)
        );
        let mut token = first["nextToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no nextToken in {first}"))
            .to_string();

        match row.change {
            Change::Create(path) => add_file(&router, &space, &asset, case, path).await,
            Change::Delete(path) => {
                let uri = asset_file_uri(&space, &asset, path);
                let (status, body) = call(&router, "DELETE", &uri, None).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200 from DeleteAssetFile {uri}, got {status}: {body}"
                );
            }
        }

        let mut served = paths(case, &first);
        for _ in 0..START.len() + 2 {
            let body = list_files(
                &router,
                &space,
                &asset,
                case,
                &format!("?maxResults=2&nextToken={token}"),
            )
            .await;
            served.extend(paths(case, &body));
            match body.get("nextToken").and_then(Value::as_str) {
                Some(next) => token = next.to_string(),
                None => break,
            }
        }

        assert_eq!(
            served,
            row.serves.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            "case {case:?}: the interrupted walk served {served:?}"
        );

        // The asset itself holds what the change left it holding, whatever the
        // walk in flight did or did not serve.
        let whole = list_files(&router, &space, &asset, case, "").await;
        assert_eq!(
            paths(case, &whole),
            row.holds.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            "case {case:?}: the asset's whole listing reported {:?}",
            paths(case, &whole)
        );
        assert_eq!(
            items(case, &whole).len(),
            row.holds.len(),
            "case {case:?}: the whole listing carried {} summaries for {:?}",
            items(case, &whole).len(),
            row.holds
        );
    }
}
