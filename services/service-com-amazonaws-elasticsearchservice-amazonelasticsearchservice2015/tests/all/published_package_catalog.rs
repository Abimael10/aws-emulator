// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH packages the package plane reports, driven as real
//! restJson1 requests through the router.
//!
//! RULE: every package a package read reports, or resolves an id against, is one
//! this account CREATED in the Region the call was signed for, projected from
//! that package's own stored record and version history — `DescribePackages`
//! reports those records and nothing else, `GetPackageVersionHistory` and
//! `AssociatePackage` resolve an id only among them, and each entry's
//! `AvailablePackageVersion` and `LastUpdatedAt` are that package's own newest
//! revision. The service serves no catalogue of its own beside the store, so a
//! read reports no package a later read cannot resolve, and an account that
//! created nothing is reported nothing.
//!
//! The suite is the successor of the one that pinned an emulator-authored
//! catalogue of plugin packages in this file: those rows were reference data
//! this emulator invented — packages no account created, reported `AVAILABLE`
//! with ids, descriptions and stamps written here — so the rows below pin their
//! ABSENCE together with the projection that replaces them.
//!
//! The rows differ along what the rule generalises over: how many packages the
//! account created (none, one, several), the Region each was created in, how
//! many revisions each package has, and how an id misses (never minted, minted
//! in another Region, well formed in the other published form). The members
//! asserted are `PackageDetails`'s and `PackageVersionHistory`'s own:
//!
//! ```text
//! smithy_query(query="shape",shape="PackageDetails,PackageVersionHistory",model_path="services/service-com-amazonaws-elasticsearchservice-amazonelasticsearchservice2015/smithy-model/com-amazonaws-elasticsearchservice-amazonelasticsearchservice2015.json")
//! ```

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call_in, router, Answer};

/// The Region most rows are signed for.
const REGION: &str = "us-west-2";

/// A second Region, so nothing here can pass by reporting a package another
/// Region holds.
const OTHER_REGION: &str = "eu-west-1";

/// How often a package is looked at again while the clock owes it its copy, and
/// how long that looking goes on. The span belongs to the service, so nothing
/// here sleeps on its behalf: the wait is on the CONDITION, and the deadline is
/// a ceiling on the wait and nothing else — generous, so a loaded host cannot
/// decide any outcome, and no assertion is about how long anything took.
const LOOK: Duration = Duration::from_millis(20);
/// See [`LOOK`].
const AVAILABLE_BY: Duration = Duration::from_secs(120);

async fn create_package_in(router: &MultiServiceRouter, region: &str, body: &Value) -> Answer {
    call_in(router, region, "POST", "/2015-01-01/packages", Some(body)).await
}

async fn describe_packages_in(router: &MultiServiceRouter, region: &str, body: &Value) -> Answer {
    call_in(
        router,
        region,
        "POST",
        "/2015-01-01/packages/describe",
        Some(body),
    )
    .await
}

async fn history_in(router: &MultiServiceRouter, region: &str, package_id: &str) -> Answer {
    call_in(
        router,
        region,
        "GET",
        &format!("/2015-01-01/packages/{package_id}/history"),
        None,
    )
    .await
}

/// Create one package in `region` and answer the id the service minted for it.
async fn created(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let answer = create_package_in(
        router,
        region,
        &json!({
            "PackageName": name,
            "PackageType": "TXT-DICTIONARY",
            "PackageSource": {"S3BucketName": "pkg-bucket", "S3Key": format!("{name}/v1.txt")}
        }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {name:?}: the create must answer 200, got {} {}",
        answer.status, answer.body
    );
    answer.text(name, "/PackageDetails/PackageID")
}

/// Give the package `package_id` new content, which mints its next revision.
async fn revise(router: &MultiServiceRouter, region: &str, package_id: &str, revision: usize) {
    let answer = call_in(
        router,
        region,
        "POST",
        "/2015-01-01/packages/update",
        Some(&json!({
            "PackageID": package_id,
            "PackageSource": {
                "S3BucketName": "pkg-bucket",
                "S3Key": format!("{package_id}/v{revision}.txt")
            },
            "CommitMessage": format!("revision {revision}")
        })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {package_id:?}: the update must answer 200, got {} {}",
        answer.status, answer.body
    );
}

/// The `PackageDetails` entries a describe reported.
fn reported(answer: &Answer, case: &str) -> Vec<Value> {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the describe must answer 200, got {} {}",
        answer.status, answer.body
    );
    answer
        .at(case, "/PackageDetailsList")
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: PackageDetailsList is not a list, got {}",
                answer.body
            )
        })
        .clone()
}

/// One member of an entry, as text.
fn text(entry: &Value, case: &str, member: &str) -> String {
    entry
        .pointer(&format!("/{member}"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: entry has no string {member}, got {entry}"))
        .to_string()
}

/// The entry `package_id` is reported under, once the clock has finished its
/// copy — looked for again until the condition holds or [`AVAILABLE_BY`] passes.
async fn available_entry(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    package_id: &str,
) -> Value {
    let deadline = Instant::now() + AVAILABLE_BY;
    loop {
        let answer = describe_packages_in(
            router,
            region,
            &json!({"Filters": [{"Name": "PackageID", "Value": [package_id]}]}),
        )
        .await;
        let entries = reported(&answer, case);
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: {package_id} must be reported exactly once, got {entries:?}"
        );
        let entry = entries[0].clone();
        if entry.pointer("/PackageStatus") == Some(&json!("AVAILABLE")) {
            return entry;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {package_id} never reached AVAILABLE, last reported {entry}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// RULE: an account that created nothing is reported no packages at all, in any
/// Region, and no id resolves.
#[tokio::test]
async fn an_account_that_created_nothing_is_reported_no_packages() {
    let router = router();
    for region in [REGION, OTHER_REGION, "ap-southeast-2"] {
        let answer = describe_packages_in(&router, region, &json!({})).await;
        let entries = reported(&answer, region);
        assert!(
            entries.is_empty(),
            "case {region:?}: a Region whose account created nothing must report no package, \
             got {entries:?}"
        );
        assert!(
            answer.missing("/NextToken"),
            "case {region:?}: a page that ended the set carries no NextToken, got {:?}",
            answer.body.pointer("/NextToken")
        );
    }

    // Rows differ along HOW the id misses: both published forms of a well-formed
    // `PackageID`, and one of the form this service itself mints.
    for (case, package_id) in [
        ("the form an account's own package carries", "pkg-00000000000000ff"),
        (
            "the long form an AWS-published package carries",
            "pkg-10b3c5c4ab016794431c100a4963d8d1e07e3673",
        ),
        ("the form published package ids carry", "F1234567890"),
    ] {
        let read = history_in(&router, REGION, package_id).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: an id nothing created must be refused, got {} {}",
            read.status, read.body
        );
        assert!(
            read.error_type.contains("ResourceNotFoundException"),
            "case {case:?}: the error must be ResourceNotFoundException, got {:?} / {}",
            read.error_type,
            read.body
        );
        assert_eq!(
            read.text(case, "/message"),
            format!("Package not found: {package_id}"),
            "case {case:?}: unexpected message, got {}",
            read.body
        );
    }
}

/// RULE: the listing is exactly the packages the account created in the Region
/// the call was signed for.
#[tokio::test]
async fn the_listing_is_exactly_what_this_account_created_here() {
    // Rows differ along how many packages the Region holds.
    let rows: [(&str, &[&str], &[&str]); 3] = [
        ("one package here, none elsewhere", &["dict-only"], &[]),
        (
            "several here, one elsewhere",
            &["dict-alpha", "dict-beta", "dict-gamma"],
            &["dict-elsewhere"],
        ),
        (
            "none here, several elsewhere",
            &[],
            &["dict-far-one", "dict-far-two"],
        ),
    ];
    for (case, here, there) in rows {
        let router = router();
        let mut minted: Vec<(String, String)> = Vec::new();
        for name in here {
            minted.push((created(&router, REGION, name).await, (*name).to_string()));
        }
        for name in there {
            created(&router, OTHER_REGION, name).await;
        }

        let entries = reported(&describe_packages_in(&router, REGION, &json!({})).await, case);
        let mut got: Vec<(String, String)> = entries
            .iter()
            .map(|entry| (text(entry, case, "PackageID"), text(entry, case, "PackageName")))
            .collect();
        got.sort();
        let mut expected = minted.clone();
        expected.sort();
        assert_eq!(
            got, expected,
            "case {case:?}: the listing must report exactly this Region's own packages"
        );

        // Every id the listing gave resolves, and only those: the listing names
        // no package a per-id read cannot serve.
        for (id, name) in &minted {
            // The form every recorded id of this API carries: the prefix and 40
            // lowercase hex digits, so an id this emulator mints is shaped like
            // one the service mints.
            let digits = id.strip_prefix("pkg-").unwrap_or_default();
            assert!(
                digits.len() == 40
                    && digits
                        .chars()
                        .all(|each| each.is_ascii_digit() || ('a'..='f').contains(&each)),
                "case {case:?}: {name:?} must be minted a pkg- id over 40 hex digits, got {id:?}"
            );
            let read = history_in(&router, REGION, id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the history of {name:?} must answer 200, got {} {}",
                read.status, read.body
            );
            assert_eq!(
                read.at(case, "/PackageID"),
                &json!(id),
                "case {case:?}: the history must echo the id it was read by, got {:?}",
                read.body.pointer("/PackageID")
            );
            // …and the same id resolves in no other Region, because a package is
            // held by the Region its create was signed for.
            let elsewhere = history_in(&router, OTHER_REGION, id).await;
            assert_eq!(
                elsewhere.status, 404,
                "case {case:?}: {name:?} must not resolve in another Region, got {} {}",
                elsewhere.status, elsewhere.body
            );
        }
    }
}

/// RULE: each entry's `AvailablePackageVersion` and `LastUpdatedAt` are that
/// package's OWN newest revision, so the listing tracks each package's version
/// history separately.
#[tokio::test]
async fn each_entry_tracks_its_own_version_history() {
    let router = router();
    // Rows differ along how many revisions the package has: the create's alone,
    // one update on top, and several.
    let rows: [(&str, &str, usize); 3] = [
        ("the create's version alone", "dict-one-version", 0),
        ("one update on top", "dict-two-versions", 1),
        ("several updates", "dict-four-versions", 3),
    ];

    let mut seen: Vec<(String, f64)> = Vec::new();
    for (case, name, updates) in rows {
        let id = created(&router, REGION, name).await;
        for revision in 0..updates {
            // A revision is taken only once the previous copy has landed, so
            // every entry of the history is a revision the clock finished.
            available_entry(&router, REGION, case, &id).await;
            revise(&router, REGION, &id, revision + 2).await;
        }
        let entry = available_entry(&router, REGION, case, &id).await;

        let versions = updates + 1;
        let expected_version = format!("v{versions}");
        assert_eq!(
            text(&entry, case, "AvailablePackageVersion"),
            expected_version,
            "case {case:?}: AvailablePackageVersion must name the newest revision, got {entry}"
        );

        let read = history_in(&router, REGION, &id).await;
        let listed = read
            .at(case, "/PackageVersionHistoryList")
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: PackageVersionHistoryList is not a list, got {}",
                    read.body
                )
            })
            .clone();
        assert_eq!(
            listed.len(),
            versions,
            "case {case:?}: a package revised {updates} times must report {versions} versions, \
             got {listed:?}"
        );
        assert_eq!(
            listed[0].pointer("/PackageVersion"),
            Some(&json!(expected_version)),
            "case {case:?}: the newest version must head the history, got {:?}",
            listed[0].pointer("/PackageVersion")
        );

        // The entry's `LastUpdatedAt` is the newest revision's own stamp, and
        // `CreatedAt` the oldest one's.
        let stamp = |value: &Value, member: &str| -> f64 {
            value
                .pointer(&format!("/{member}"))
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("case {case:?}: no {member} timestamp in {value}"))
        };
        assert_eq!(
            stamp(&entry, "LastUpdatedAt"),
            stamp(&listed[0], "CreatedAt"),
            "case {case:?}: LastUpdatedAt must be the newest revision's own stamp"
        );
        assert_eq!(
            stamp(&entry, "CreatedAt"),
            stamp(listed.last().expect("a version"), "CreatedAt"),
            "case {case:?}: CreatedAt must be the oldest revision's own stamp"
        );
        // Newest first: the stamps do not increase down the list.
        let stamps: Vec<f64> = listed.iter().map(|each| stamp(each, "CreatedAt")).collect();
        for pair in stamps.windows(2) {
            assert!(
                pair[0] >= pair[1],
                "case {case:?}: CreatedAt must not increase down a newest-first history, got \
                 {stamps:?}"
            );
        }
        seen.push((expected_version, stamp(&entry, "LastUpdatedAt")));
    }

    // Each package keeps its OWN version: the listing does not report one
    // version for every entry.
    let mut versions: Vec<String> = seen.iter().map(|(version, _)| version.clone()).collect();
    versions.sort();
    versions.dedup();
    assert_eq!(
        versions.len(),
        seen.len(),
        "each package must report its own available version, got {seen:?}"
    );
}
