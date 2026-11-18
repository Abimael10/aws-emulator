// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListPackageVersions`: the versions ONE software package
//! holds, as one page of summaries read off the very records `CreatePackageVersion`
//! persisted and `UpdatePackageVersion` moved through the package version lifecycle.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /packages/{packageName}/versions` signed for the `iot` signing name — against the
//! `CreatePackage` / `CreatePackageVersion` / `GetPackageVersion` helpers of the parent
//! suite, so a listing is compared with what the single-version reader reports for the
//! same pair of names.
//!
//! The rules under test:
//!
//! * the listing reports every version of the named package, the most recently created
//!   one first, each summarised as the five members `PackageVersionSummary` declares and
//!   nothing beside them; a package holding none is answered an empty list rather than an
//!   absent one, and a version of another package is never in it;
//! * the order is by the instant a version was CREATED, so updating a version — which
//!   moves its `lastModifiedDate` and not its `creationDate` — leaves its position alone;
//! * `maxResults` bounds one page and `nextToken` resumes the SAME listing exactly where
//!   the page stopped, so walking the token chain visits every version once and the last
//!   page carries no token;
//! * `status` selects exactly the versions holding that status, so the three modelled
//!   statuses PARTITION the unfiltered listing;
//! * a request the listing does not admit — a `nextToken` it never issued, one issued for
//!   another package or another filter, a `maxResults` outside the modelled range, a
//!   package the catalog does not hold — is refused and lists nothing;
//! * the listing projects the store as it IS and changes nothing: a version a delete
//!   removed is listed no more, and two identical reads answer identically.
//!
//! The tables span what the rules generalise over: how many versions a package holds, in
//! which name order they were created (never the alphabetical one, so "newest first"
//! cannot hold by accident), the forms and lengths `VersionName` admits, which page size
//! the request asks for (both modelled bounds included, and one past each), and which of
//! the modelled statuses it filters on.

use super::*;

/// Every value of the modelled `PackageVersionStatus`
/// (`smithy_query(query="shape",shape="PackageVersionStatus")`): the whole catalog the
/// `status` filter is drawn from, so the partition rule is checked over all of it.
const PACKAGE_VERSION_STATUSES: [&str; 3] = ["DRAFT", "PUBLISHED", "DEPRECATED"];

/// The `nextToken` refusal this listing answers a token it did not issue with.
const REFUSED_TOKEN_MESSAGE: &str = "nextToken is not a continuation token of this listing";

/// The `PackageVersionAction` that assigns `status`, or `None` for the status a create
/// enters the lifecycle at (`smithy_query(query="shape",shape="PackageVersionAction")`:
/// `PUBLISH`, `DEPRECATE`).
fn action_assigning(status: &str) -> Option<&'static str> {
    match status {
        "PUBLISHED" => Some("PUBLISH"),
        "DEPRECATED" => Some("DEPRECATE"),
        _ => None,
    }
}

/// One `GET /packages/{packageName}/versions` with `query` appended (no leading `?`), the
/// listing's modelled HTTP binding
/// (`smithy_query(query="operation",operation="ListPackageVersions")`).
async fn list(emulator: &Emulator, package: &str, query: &str) -> Reply {
    let path = if query.is_empty() {
        format!("/packages/{package}/versions")
    } else {
        format!("/packages/{package}/versions?{query}")
    };
    emulator.call("GET", &path, None, REGION).await
}

/// The `packageVersionSummaries` member of a listing, which a served listing always
/// carries.
fn summaries(case: &str, reply: &Reply) -> Vec<Value> {
    let listed = reply.body.get("packageVersionSummaries").unwrap_or_else(|| {
        panic!(
            "case {case:?}: a served listing carries packageVersionSummaries, got {:?}",
            reply.body
        )
    });
    listed
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: packageVersionSummaries is a list, got {:?}",
                reply.body
            )
        })
        .clone()
}

/// The version names a listing reported, in the order it reported them.
fn listed_names(case: &str, reply: &Reply) -> Vec<String> {
    summaries(case, reply)
        .iter()
        .map(|summary| {
            summary
                .get("versionName")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a summary names its version, got {summary:?}")
                })
                .to_string()
        })
        .collect()
}

/// The `creationDate` of each summary a listing reported, in the order it reported them.
fn listed_creation_dates(case: &str, reply: &Reply) -> Vec<f64> {
    summaries(case, reply)
        .iter()
        .map(|summary| {
            summary
                .get("creationDate")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a summary carries its creationDate, got {summary:?}")
                })
        })
        .collect()
}

/// A package holding one DRAFT version per name, created in the order given.
async fn seeded(case: &str, package: &str, versions: &[&str]) -> Emulator {
    let emulator = Emulator::new();
    let created = emulator.create_package(package).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the package must be creatable, got {created:?}"
    );
    for version in versions {
        let recorded = emulator.create_version(package, version, &json!({})).await;
        assert_eq!(
            recorded.status, 200,
            "case {case:?}: version {version:?} must be creatable, got {recorded:?}"
        );
    }
    emulator
}

/// The names `versions` in the order the listing must report them: the most recently
/// created version first, which for a set created in order is the reverse of it.
fn newest_first(versions: &[&str]) -> Vec<String> {
    versions
        .iter()
        .rev()
        .map(|version| (*version).to_string())
        .collect()
}

/// Assign `status` to one version with `UpdatePackageVersion`
/// (`PATCH /packages/{packageName}/versions/{versionName}`).
async fn assign_status(case: &str, emulator: &Emulator, package: &str, version: &str, status: &str) {
    let Some(action) = action_assigning(status) else {
        return;
    };
    let updated = emulator
        .call(
            "PATCH",
            &format!("/packages/{package}/versions/{version}"),
            Some(&json!({ "action": action })),
            REGION,
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: {version:?} must be assignable {status}, got {updated:?}"
    );
}

/// Walk every page of `package`'s listing, `page_size` versions at a time, and answer the
/// names in the order they were reported together with how many pages it took.
async fn walk_pages(
    case: &str,
    emulator: &Emulator,
    package: &str,
    query: &str,
    page_size: usize,
    total: usize,
) -> (Vec<String>, usize) {
    let mut walked: Vec<String> = Vec::new();
    let mut pages = 0usize;
    let mut token: Option<String> = None;
    // One page per version is the most a correct listing can need; a token chain that
    // outruns that is reporting the same page for ever rather than advancing.
    let ceiling = total + 2;
    loop {
        let mut request = format!("maxResults={page_size}");
        if !query.is_empty() {
            request = format!("{query}&{request}");
        }
        if let Some(token) = &token {
            request = format!("{request}&nextToken={token}");
        }
        let page = list(emulator, package, &request).await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: page {pages} must be served, got {page:?}"
        );
        let names = listed_names(case, &page);
        assert!(
            names.len() <= page_size,
            "case {case:?}: page {pages} must hold at most {page_size} versions, got {names:?}"
        );
        walked.extend(names);
        pages += 1;
        token = page.text("nextToken");
        if token.is_none() {
            return (walked, pages);
        }
        assert!(
            pages < ceiling,
            "case {case:?}: the token chain must end within {ceiling} pages, got {walked:?}"
        );
    }
}

/// RULE: the listing reports every version of the named package, the most recently created
/// first, as the five members `PackageVersionSummary` declares — each read off the record
/// `CreatePackageVersion` persisted, so the listing and `GetPackageVersion` report one
/// value.
#[tokio::test]
async fn every_version_is_listed_newest_first_as_the_record_the_create_persisted() {
    // A package that holds nothing: "no version yet" is an answer, not a missing one.
    let empty = Emulator::new();
    assert_eq!(
        empty.create_package("holds-nothing").await.status,
        200,
        "the package must be creatable"
    );
    let none = list(&empty, "holds-nothing", "").await;
    assert_eq!(
        none.status, 200,
        "case \"a package holding no version\": expected 200, got {none:?}"
    );
    assert_eq!(
        listed_names("a package holding no version", &none),
        Vec::<String>::new(),
        "case \"a package holding no version\": expected an empty listing, got {:?}",
        none.body
    );
    assert_eq!(
        none.text("nextToken"),
        None,
        "case \"a package holding no version\": an exhausted listing carries no token, got {:?}",
        none.body
    );

    // Each row creates its versions in an order that is NOT their sorted order and spans
    // the forms `VersionName` admits (letters, digits, `-`, `_`, `.`, upper case, both
    // length bounds), so "the most recently created first" cannot hold by accident.
    let table: [(&str, &str, Vec<&str>); 4] = [
        ("one version", "single.package", vec!["1.0.0"]),
        (
            "three versions created against the name order",
            "ordering-package",
            vec!["v2.0.0", "v1.5.0", "v3.0.0"],
        ),
        (
            "name forms and both length bounds",
            "Forms_Package.1",
            vec!["z", "A_b-c.9", "0.0.1", "a"],
        ),
        (
            "six versions created newest-last",
            "six.versions",
            vec!["r6", "q5", "p4", "n3", "m2", "l1"],
        ),
    ];

    for (label, package, created) in table {
        let emulator = seeded(label, package, &created).await;
        let listed = list(&emulator, package, "").await;
        assert_eq!(
            listed.status, 200,
            "case {label:?}: expected 200, got {listed:?}"
        );
        assert_eq!(
            listed_names(label, &listed),
            newest_first(&created),
            "case {label:?}: expected every version newest first, got {:?}",
            listed.body
        );
        assert_eq!(
            listed.text("nextToken"),
            None,
            "case {label:?}: a listing that returned every version carries no token, got {:?}",
            listed.body
        );

        // The order the listing reports is the order of the instants it itself reports.
        let dates = listed_creation_dates(label, &listed);
        for pair in dates.windows(2) {
            assert!(
                pair[0] >= pair[1],
                "case {label:?}: expected non-increasing creationDates, got {dates:?}"
            );
        }

        for summary in summaries(label, &listed) {
            let version = summary["versionName"]
                .as_str()
                .expect("a summary names its version");
            let read = emulator.get_version(package, version).await;
            assert_eq!(
                read.status, 200,
                "case {label:?}/{version:?}: the listed version must be readable, got {read:?}"
            );
            for member in ["packageName", "versionName", "status", "creationDate"] {
                assert_eq!(
                    summary.get(member),
                    read.body.get(member),
                    "case {label:?}/{version:?}: the listing and the reader must report one \
                     {member}, got {summary:?} and {:?}",
                    read.body
                );
            }
            assert_eq!(
                summary.get("status").and_then(Value::as_str),
                Some(NEW_PACKAGE_VERSION_STATUS),
                "case {label:?}/{version:?}: a created version is listed in the status the \
                 lifecycle is entered at, got {summary:?}"
            );
            assert_eq!(
                summary.get("packageName").and_then(Value::as_str),
                Some(package),
                "case {label:?}/{version:?}: a summary names the package that holds it, got \
                 {summary:?}"
            );
            // `PackageVersionSummary` declares exactly five members, so a listing states
            // those and invents nothing beside them.
            let mut members: Vec<&str> = summary
                .as_object()
                .expect("a summary is an object")
                .keys()
                .map(String::as_str)
                .collect();
            members.sort_unstable();
            assert_eq!(
                members,
                [
                    "creationDate",
                    "lastModifiedDate",
                    "packageName",
                    "status",
                    "versionName"
                ],
                "case {label:?}/{version:?}: expected the five members \
                 PackageVersionSummary declares, got {summary:?}"
            );
        }

        // A read changes nothing a later caller sees.
        let again = list(&emulator, package, "").await;
        assert_eq!(
            summaries(label, &again),
            summaries(label, &listed),
            "case {label:?}: two identical reads must answer identically, got {:?}",
            again.body
        );
    }
}

/// RULE: a version belongs to the package that holds it, so a listing reports that
/// package's versions and no other's — and the order is by the CREATION instant, which an
/// update does not move.
#[tokio::test]
async fn a_listing_reports_one_package_in_creation_order() {
    let case = "two packages holding versions of the same names";
    let emulator = Emulator::new();
    for package in ["left.package", "right.package"] {
        assert_eq!(
            emulator.create_package(package).await.status,
            200,
            "case {case:?}: {package:?} must be creatable"
        );
    }
    // Interleaved creates under both packages, under names each package shares with the
    // other: what separates the two listings is the package, not the names or the clock.
    for version in ["1.0.0", "2.0.0", "3.0.0"] {
        for package in ["left.package", "right.package"] {
            assert_eq!(
                emulator
                    .create_version(package, version, &json!({}))
                    .await
                    .status,
                200,
                "case {case:?}: {package:?}/{version:?} must be creatable"
            );
        }
    }
    for package in ["left.package", "right.package"] {
        let listed = list(&emulator, package, "").await;
        assert_eq!(
            listed_names(case, &listed),
            newest_first(&["1.0.0", "2.0.0", "3.0.0"]),
            "case {case:?}: expected only {package:?}'s versions, newest first, got {:?}",
            listed.body
        );
        for summary in summaries(case, &listed) {
            assert_eq!(
                summary.get("packageName").and_then(Value::as_str),
                Some(package),
                "case {case:?}: every summary names the package listed, got {summary:?}"
            );
        }
    }

    // `UpdatePackageVersion` is "The date when the package version was last updated", so
    // it moves `lastModifiedDate` and leaves `creationDate` where the create put it: the
    // OLDEST version, updated, stays last in the listing.
    let oldest = "1.0.0";
    assign_status(case, &emulator, "left.package", oldest, "PUBLISHED").await;
    let after = list(&emulator, "left.package", "").await;
    assert_eq!(
        listed_names(case, &after),
        newest_first(&["1.0.0", "2.0.0", "3.0.0"]),
        "case {case:?}: an update must not move a version in a creation-ordered listing, got \
         {:?}",
        after.body
    );
    let updated = summaries(case, &after)
        .into_iter()
        .find(|summary| summary.get("versionName").and_then(Value::as_str) == Some(oldest))
        .unwrap_or_else(|| panic!("case {case:?}: the updated version is still listed"));
    let creation = updated
        .get("creationDate")
        .and_then(Value::as_f64)
        .expect("a summary carries its creationDate");
    let modified = updated
        .get("lastModifiedDate")
        .and_then(Value::as_f64)
        .expect("a summary carries its lastModifiedDate");
    assert!(
        modified >= creation,
        "case {case:?}: the update must stamp lastModifiedDate at or after creationDate, got \
         {updated:?}"
    );
}

/// RULE: `status` selects exactly the versions holding that status, so the three modelled
/// statuses partition the unfiltered listing and each filtered listing keeps its order.
#[tokio::test]
async fn the_status_filter_selects_exactly_the_versions_holding_that_status() {
    let case = "five versions across the lifecycle";
    let created = ["s1", "s2", "s3", "s4", "s5"];
    let package = "status.package";
    let emulator = seeded(case, package, &created).await;

    // One version left where the create put it, two published, two deprecated: every
    // modelled status selects something, so no filter passes vacuously.
    let wanted: [(&str, &str); 5] = [
        ("s1", "DRAFT"),
        ("s2", "PUBLISHED"),
        ("s3", "DEPRECATED"),
        ("s4", "PUBLISHED"),
        ("s5", "DEPRECATED"),
    ];
    for (version, status) in wanted {
        assign_status(case, &emulator, package, version, status).await;
    }

    let mut partitioned: Vec<String> = Vec::new();
    for status in PACKAGE_VERSION_STATUSES {
        let case = format!("{case}, status={status}");
        let filtered = list(&emulator, package, &format!("status={status}")).await;
        assert_eq!(
            filtered.status, 200,
            "case {case:?}: expected 200, got {filtered:?}"
        );
        let expected: Vec<String> = wanted
            .iter()
            .rev()
            .filter(|(_, held)| *held == status)
            .map(|(version, _)| (*version).to_string())
            .collect();
        let names = listed_names(&case, &filtered);
        assert_eq!(
            names, expected,
            "case {case:?}: expected exactly the versions holding that status, newest first, \
             got {:?}",
            filtered.body
        );
        for summary in summaries(&case, &filtered) {
            assert_eq!(
                summary.get("status").and_then(Value::as_str),
                Some(status),
                "case {case:?}: a selected version holds the status filtered on, got {summary:?}"
            );
        }
        partitioned.extend(names);
    }
    partitioned.sort();
    let mut all = listed_names(case, &list(&emulator, package, "").await);
    all.sort();
    assert_eq!(
        partitioned, all,
        "case {case:?}: the statuses must partition the unfiltered listing, got {partitioned:?}"
    );

    // A filtered listing pages like any other, and its token belongs to IT.
    let (walked, pages) =
        walk_pages(case, &emulator, package, "status=PUBLISHED", 1, created.len()).await;
    assert_eq!(
        walked,
        vec!["s4".to_string(), "s2".to_string()],
        "case {case:?}: a filtered listing pages through every selected version, got {walked:?}"
    );
    assert_eq!(
        pages, 2,
        "case {case:?}: expected one page per selected version, got {pages}"
    );
}

/// RULE: `maxResults` bounds one page and `nextToken` resumes the same listing where the
/// page stopped, so the token chain reports every version exactly once, in the listing's
/// order, and ends without a token.
#[tokio::test]
async fn the_token_chain_walks_the_listing_once_whatever_the_page_size() {
    let case = "seven versions paged";
    let created = ["p1", "p2", "p3", "p4", "p5", "p6", "p7"];
    let package = "paged.package";
    let emulator = seeded(case, package, &created).await;
    let expected = newest_first(&created);

    // The page sizes span the class: the modelled minimum, sizes that divide the set and
    // sizes that do not, the count itself, one past it, and the modelled maximum.
    for page_size in [1usize, 2, 3, 7, 8, 100] {
        let case = format!("{case}, maxResults={page_size}");
        let (walked, pages) =
            walk_pages(&case, &emulator, package, "", page_size, expected.len()).await;
        assert_eq!(
            walked, expected,
            "case {case:?}: expected the whole listing once, newest first, got {walked:?}"
        );
        let wanted_pages = expected.len().div_ceil(page_size).max(1);
        assert_eq!(
            pages, wanted_pages,
            "case {case:?}: expected {wanted_pages} page(s) of {page_size}, got {pages}"
        );
    }

    // A page that stops short of the end carries the token, and the version it resumes
    // after is the last one it reported.
    let first = list(&emulator, package, "maxResults=3").await;
    let token = first
        .text("nextToken")
        .unwrap_or_else(|| panic!("a truncated listing carries a token, got {:?}", first.body));
    let rest = list(&emulator, package, &format!("maxResults=100&nextToken={token}")).await;
    assert_eq!(
        listed_names("resumed after the third version", &rest),
        expected[3..].to_vec(),
        "case \"resumed after the third version\": expected the versions past the first page, \
         got {:?}",
        rest.body
    );
    assert_eq!(
        rest.text("nextToken"),
        None,
        "case \"resumed after the third version\": the last page carries no token, got {:?}",
        rest.body
    );
}

/// RULE: a request the listing does not admit — a `nextToken` it never issued, a token
/// issued for another package or another filter, a `maxResults` outside the modelled range
/// — is refused and lists nothing; both published bounds of the range are served.
#[tokio::test]
async fn a_request_the_listing_does_not_admit_is_refused() {
    let case = "four versions, refused requests";
    let created = ["r1", "r2", "r3", "r4"];
    let package = "refusal.package";
    let emulator = seeded(case, package, &created).await;
    let sibling = "refusal.sibling";
    assert_eq!(
        emulator.create_package(sibling).await.status,
        200,
        "case {case:?}: the sibling package must be creatable"
    );
    for version in created {
        assert_eq!(
            emulator
                .create_version(sibling, version, &json!({}))
                .await
                .status,
            200,
            "case {case:?}: {sibling:?}/{version:?} must be creatable"
        );
    }

    // A token is a position in ONE listing: a token this package's listing issued names no
    // position of another package's, nor of a filtered listing of the same package.
    let unfiltered = list(&emulator, package, "maxResults=1").await;
    let own_token = unfiltered
        .text("nextToken")
        .unwrap_or_else(|| panic!("a truncated listing carries a token, got {:?}", unfiltered.body));
    let filtered = list(&emulator, package, "status=DRAFT&maxResults=1").await;
    let filtered_token = filtered
        .text("nextToken")
        .unwrap_or_else(|| panic!("a truncated listing carries a token, got {:?}", filtered.body));

    let refusals: [(String, String, String); 5] = [
        (
            "a nextToken that is not a token of any listing".to_string(),
            package.to_string(),
            "nextToken=not-a-token".to_string(),
        ),
        (
            "a nextToken that is hex but names no position".to_string(),
            package.to_string(),
            format!("nextToken={}", hex::encode("nonsense")),
        ),
        (
            "a nextToken whose position is no creation instant".to_string(),
            package.to_string(),
            format!(
                "nextToken={}",
                hex::encode(format!("{package}\u{1e}\u{1f}not-a-number\u{1e}r4"))
            ),
        ),
        (
            "a token one package's listing issued, replayed on another's".to_string(),
            sibling.to_string(),
            format!("nextToken={own_token}"),
        ),
        (
            "a token a filtered listing issued, replayed on the unfiltered one".to_string(),
            package.to_string(),
            format!("nextToken={filtered_token}"),
        ),
    ];

    for (label, listed_package, query) in &refusals {
        let refused = list(&emulator, listed_package, query).await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (400, Some("ValidationException")),
            "case {label:?}: expected 400 ValidationException, got {refused:?}"
        );
        assert_eq!(
            refused.text("message").as_deref(),
            Some(REFUSED_TOKEN_MESSAGE),
            "case {label:?}: expected the token refusal's wording, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.body.get("packageVersionSummaries"),
            None,
            "case {label:?}: a refused request lists nothing, got {:?}",
            refused.body
        );
    }

    // The issuing listing still accepts its own token: refusing a token is about the
    // listing it belongs to, not about tokens.
    let resumed = list(&emulator, package, &format!("maxResults=1&nextToken={own_token}")).await;
    assert_eq!(
        resumed.status, 200,
        "case \"the issuing listing replays its own token\": expected 200, got {resumed:?}"
    );

    // `PackageCatalogMaxResults` is `range 1..100`: both published bounds are served, and
    // a value outside the range is refused before the listing runs.
    for served in ["1", "100"] {
        let listed = list(&emulator, package, &format!("maxResults={served}")).await;
        assert_eq!(
            listed.status, 200,
            "case maxResults={served}: the modelled bound must be served, got {listed:?}"
        );
    }
    for out_of_range in ["0", "-1", "101"] {
        let refused = list(&emulator, package, &format!("maxResults={out_of_range}")).await;
        assert_eq!(
            refused.status, 400,
            "case maxResults={out_of_range}: must be refused 400, got {refused:?}"
        );
        assert_eq!(
            refused.body.get("packageVersionSummaries"),
            None,
            "case maxResults={out_of_range}: a refused request lists nothing, got {:?}",
            refused.body
        );
    }
}

/// RULE: the versions are the package's own, so a `packageName` the catalog does not hold
/// names no version set to page through and the request is refused — an empty page would
/// state that the package exists and holds nothing.
#[tokio::test]
async fn a_listing_of_a_package_the_catalog_does_not_hold_is_refused() {
    let emulator = Emulator::new();

    // Names of every legal form, none of them created.
    let absent = [
        "a".to_string(),
        "never-created".to_string(),
        "Never_Created.1".to_string(),
        "n".repeat(128),
    ];
    for package in &absent {
        let refused = list(&emulator, package, "").await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (400, Some("ValidationException")),
            "case {package:?}: an uncreated package must be refused 400 ValidationException, \
             got {refused:?}"
        );
        assert_eq!(
            refused.text("message"),
            Some(format!("Package {package} not found")),
            "case {package:?}: expected the catalog's not-found wording, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.body.get("packageVersionSummaries"),
            None,
            "case {package:?}: a refused request lists nothing, got {:?}",
            refused.body
        );
    }

    // Creating ONE of them makes exactly that one listable — holding nothing yet — and
    // leaves the rest refused.
    assert_eq!(
        emulator.create_package(&absent[1]).await.status,
        200,
        "the package must be creatable"
    );
    for package in &absent {
        let listed = list(&emulator, package, "").await;
        let expected = if package == &absent[1] { 200 } else { 400 };
        assert_eq!(
            listed.status, expected,
            "case {package:?}: expected {expected}, got {listed:?}"
        );
        if expected == 200 {
            assert_eq!(
                listed_names(package, &listed),
                Vec::<String>::new(),
                "case {package:?}: a package with no version yet lists none, got {:?}",
                listed.body
            );
        }
    }
}

/// RULE: the listing projects the store as it IS, so a version a delete removed is listed
/// no more while every other version of that package still is.
#[tokio::test]
async fn a_version_a_delete_removed_is_listed_no_more() {
    let case = "one of three versions deleted";
    let created = ["d1", "d2", "d3"];
    let package = "deletion.package";
    let emulator = seeded(case, package, &created).await;

    let before = list(&emulator, package, "").await;
    assert_eq!(
        listed_names(case, &before),
        newest_first(&created),
        "case {case:?}: expected every version before the delete, got {:?}",
        before.body
    );

    let deleted = emulator
        .call(
            "DELETE",
            &format!("/packages/{package}/versions/d2"),
            None,
            REGION,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the delete must be served, got {deleted:?}"
    );

    let after = list(&emulator, package, "").await;
    assert_eq!(
        listed_names(case, &after),
        newest_first(&["d1", "d3"]),
        "case {case:?}: expected the deleted version gone and its neighbours kept, got {:?}",
        after.body
    );
    let read = emulator.get_version(package, "d2").await;
    assert_eq!(
        read.status, 404,
        "case {case:?}: the deleted version must be unreadable too, got {read:?}"
    );
}
