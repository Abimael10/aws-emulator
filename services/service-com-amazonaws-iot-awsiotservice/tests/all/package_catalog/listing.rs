// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListPackages`: the software packages this account's
//! package catalog holds, as one page of summaries read off the very records
//! `CreatePackage` persisted and `UpdatePackage` maintains.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /packages` signed for the `iot` signing name — against the `CreatePackage` /
//! `GetPackage` helpers of the parent suite, so a listing is compared with what the
//! single-package reader reports for the same name.
//!
//! The rules under test:
//!
//! * the listing reports every package the account holds, ordered by the `packageName`
//!   that identifies it, each summarised as the members `PackageSummary` declares and
//!   nothing beside them, with every member equal to what `GetPackage` reports for that
//!   package; an account holding none is answered an empty list rather than an absent one
//!   or a refusal;
//! * `maxResults` bounds one page and `nextToken` resumes the SAME listing exactly where
//!   the page stopped, so walking the token chain visits every package once, in the
//!   listing's order, and the last page carries no token;
//! * a request the listing does not admit — a `nextToken` it never issued, a token
//!   belonging to another listing, a `maxResults` outside the modelled range — is refused
//!   and lists nothing, while both published bounds of the range are served;
//! * the listing projects the catalog as it IS and changes nothing: a default version
//!   `UpdatePackage` promoted is reported, a package `DeletePackage` removed is listed no
//!   more, and two identical reads answer identically.
//!
//! The tables span what the rules generalise over: how many packages the catalog holds
//! (none, one, several), in which name order they were created (never the sorted one, so
//! "in name order" cannot hold by accident), the forms and lengths `PackageName` admits,
//! and which page size the request asks for — both modelled bounds included, one past
//! each, sizes that divide the set and sizes that do not.

use super::*;

/// The `nextToken` refusal this listing answers a token it did not issue with.
const REFUSED_TOKEN_MESSAGE: &str = "nextToken is not a continuation token of this listing";

/// The modelled bounds of `PackageCatalogMaxResults` (`range 1..100`,
/// `smithy_query(query="operation",operation="ListPackages")`).
const MIN_PAGE_SIZE: usize = 1;
const MAX_PAGE_SIZE: usize = 100;

/// Every member `PackageSummary` declares
/// (`smithy_query(query="shape",shape="PackageSummary")`): a summary carries these and
/// nothing beside them.
const PACKAGE_SUMMARY_MEMBERS: [&str; 4] = [
    "packageName",
    "defaultVersionName",
    "creationDate",
    "lastModifiedDate",
];

/// One `GET /packages` with `query` appended (no leading `?`), the listing's modelled HTTP
/// binding (`smithy_query(query="operation",operation="ListPackages")`).
async fn list(emulator: &Emulator, query: &str) -> Reply {
    let path = if query.is_empty() {
        "/packages".to_string()
    } else {
        format!("/packages?{query}")
    };
    emulator.call("GET", &path, None, REGION).await
}

/// `UpdatePackage`: `PATCH /packages/{packageName}`.
async fn update_package(emulator: &Emulator, name: &str, body: &Value) -> Reply {
    emulator
        .call("PATCH", &format!("/packages/{name}"), Some(body), REGION)
        .await
}

/// `DeletePackage`: `DELETE /packages/{packageName}`.
async fn delete_package(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call("DELETE", &format!("/packages/{name}"), None, REGION)
        .await
}

/// `CreatePackageVersion`: `PUT /packages/{packageName}/versions/{versionName}` — a
/// version has to exist before `UpdatePackage` can promote it as the default.
async fn create_version(emulator: &Emulator, package: &str, version: &str) -> Reply {
    emulator
        .call(
            "PUT",
            &format!("/packages/{package}/versions/{version}"),
            Some(&json!({})),
            REGION,
        )
        .await
}

/// The `packageSummaries` member of a listing, which a served listing always carries.
fn summaries(case: &str, reply: &Reply) -> Vec<Value> {
    let listed = reply.body.get("packageSummaries").unwrap_or_else(|| {
        panic!(
            "case {case:?}: a served listing carries packageSummaries, got {:?}",
            reply.body
        )
    });
    listed
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: packageSummaries is a list, got {:?}",
                reply.body
            )
        })
        .clone()
}

/// The package names a listing reported, in the order it reported them.
fn listed_names(case: &str, reply: &Reply) -> Vec<String> {
    summaries(case, reply)
        .iter()
        .map(|summary| {
            summary
                .get("packageName")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a summary names its package, got {summary:?}")
                })
                .to_string()
        })
        .collect()
}

/// A fresh emulator holding one package per entry of `names`, created in the order given
/// — which is never the order the listing must report them in.
async fn seeded(case: &str, names: &[&str]) -> Emulator {
    let emulator = Emulator::new();
    for name in names {
        let created = emulator.create(name, &json!({})).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: {name:?} must be creatable, got {created:?}"
        );
    }
    emulator
}

/// The order the catalog's packages must be reported in: by the `packageName` that
/// identifies them.
fn in_name_order(names: &[&str]) -> Vec<String> {
    let mut ordered: Vec<String> = names.iter().map(|name| name.to_string()).collect();
    ordered.sort();
    ordered
}

/// Walk the whole token chain at `page_size`, answering the names it reported in order and
/// how many pages it took. Every page is checked for the page bound as it is walked.
async fn walk_pages(
    case: &str,
    emulator: &Emulator,
    page_size: usize,
    total: usize,
) -> (Vec<String>, usize) {
    let mut walked: Vec<String> = Vec::new();
    let mut pages = 0usize;
    let mut token: Option<String> = None;
    // One page per package is the most a correct listing can need; a chain that outruns
    // that is reporting the same page for ever rather than advancing.
    let ceiling = total + 2;
    loop {
        let mut request = format!("maxResults={page_size}");
        if let Some(token) = &token {
            request = format!("{request}&nextToken={token}");
        }
        let page = list(emulator, &request).await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: page {pages} must be served, got {page:?}"
        );
        let names = listed_names(case, &page);
        assert!(
            names.len() <= page_size,
            "case {case:?}: page {pages} must carry at most {page_size} summaries, got {names:?}"
        );
        walked.extend(names);
        pages += 1;
        assert!(
            pages <= ceiling,
            "case {case:?}: the token chain must end within {ceiling} pages, got {walked:?}"
        );
        match page.text("nextToken") {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    (walked, pages)
}

/// The names the table spans: the shortest legal `PackageName`, the longest, one using
/// every character class its pattern allows, and ordinary ones — modelled
/// `length 1..128, pattern ^[a-zA-Z0-9-_.]+$`
/// (`smithy_query(query="shape",shape="PackageName")`). They are created in an order that
/// is NOT their sorted order, so "in name order" cannot hold by accident.
fn name_table() -> Vec<String> {
    vec![
        "zulu-package".to_string(),
        "a".to_string(),
        "Pkg_01-aZ.9".to_string(),
        "middle.package".to_string(),
        "p".repeat(128),
        "0-leading-digit".to_string(),
        "_underscore".to_string(),
    ]
}

/// RULE: the listing reports every package the catalog holds, ordered by the name that
/// identifies it, each summarised as the members `PackageSummary` declares and nothing
/// beside them — and every member equal to what the single-package reader reports. An
/// account holding none is answered an EMPTY list.
#[tokio::test]
async fn list_packages_reports_every_package_in_name_order() {
    let emulator = Emulator::new();

    // Nothing created yet: an account holding no package is answered an empty list rather
    // than an absent member or a refusal.
    let empty = list(&emulator, "").await;
    assert_eq!(
        empty.status, 200,
        "case \"an empty catalog\": expected 200, got {empty:?}"
    );
    assert_eq!(
        summaries("an empty catalog", &empty),
        Vec::<Value>::new(),
        "case \"an empty catalog\": expected an empty list, got {:?}",
        empty.body
    );
    assert_eq!(
        empty.text("nextToken"),
        None,
        "case \"an empty catalog\": there is no next set, got {:?}",
        empty.body
    );

    // One create at a time: after each, the listing is exactly the packages created so
    // far, in name order — so the listing follows the catalog rather than one fixed set.
    let mut created: Vec<&str> = Vec::new();
    let table = name_table();
    for name in &table {
        let create = emulator.create(name, &json!({})).await;
        assert_eq!(
            create.status, 200,
            "case {name:?}: the create must succeed, got {create:?}"
        );
        created.push(name);

        let case = format!("{} package(s) created, last {name:?}", created.len());
        let listed = list(&emulator, "").await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {listed:?}"
        );
        assert_eq!(
            listed_names(&case, &listed),
            in_name_order(&created),
            "case {case:?}: expected the catalog in name order, got {:?}",
            listed.body
        );
        assert_eq!(
            listed.text("nextToken"),
            None,
            "case {case:?}: an unbounded listing reports everything and issues no token, \
             got {:?}",
            listed.body
        );
    }

    // Every summary agrees with `GetPackage` member for member, and carries no member
    // `PackageSummary` does not declare.
    let case = "the whole catalog";
    let listed = list(&emulator, "").await;
    for summary in summaries(case, &listed) {
        let object = summary
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: a summary is an object, got {summary:?}"));
        for member in object.keys() {
            assert!(
                PACKAGE_SUMMARY_MEMBERS.contains(&member.as_str()),
                "case {case:?}: {member:?} is no member of PackageSummary, got {summary:?}"
            );
        }
        let name = summary
            .get("packageName")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: a summary names its package"));
        let read = emulator.get(name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: a listed package must be readable, got {read:?}"
        );
        for member in ["packageName", "defaultVersionName"] {
            assert_eq!(
                summary.get(member),
                read.body.get(member),
                "case {case:?}: {name:?}'s {member} must be the reader's, got {summary:?} \
                 against {:?}",
                read.body
            );
        }
        for member in ["creationDate", "lastModifiedDate"] {
            assert_eq!(
                summary.get(member).and_then(Value::as_f64),
                read.epoch(member),
                "case {case:?}: {name:?}'s {member} must be the reader's, got {summary:?} \
                 against {:?}",
                read.body
            );
        }
        // A package nobody promoted a version for has no default version, so the summary
        // carries the member not at all rather than a null.
        assert_eq!(
            summary.get("defaultVersionName"),
            None,
            "case {case:?}: {name:?} has no default version yet, got {summary:?}"
        );
    }

    // Reading does not change what a later caller sees.
    let again = list(&emulator, "").await;
    assert_eq!(
        again.body, listed.body,
        "case {case:?}: a second read must report the same catalog, got {again:?}"
    );
}

/// RULE: `maxResults` bounds one page and `nextToken` resumes the same listing where the
/// page stopped, so the token chain reports every package exactly once, in the listing's
/// order, and ends without a token.
#[tokio::test]
async fn the_token_chain_walks_the_catalog_once_whatever_the_page_size() {
    let case = "seven packages paged";
    let created = [
        "pager-g", "pager-a", "pager-f", "pager-b", "pager-e", "pager-c", "pager-d",
    ];
    let emulator = seeded(case, &created).await;
    let expected = in_name_order(&created);

    // The page sizes span the class: the modelled minimum, sizes that divide the set and
    // sizes that do not, the count itself, one past it, and the modelled maximum.
    for page_size in [MIN_PAGE_SIZE, 2, 3, 7, 8, MAX_PAGE_SIZE] {
        let case = format!("{case}, maxResults={page_size}");
        let (walked, pages) = walk_pages(&case, &emulator, page_size, expected.len()).await;
        assert_eq!(
            walked, expected,
            "case {case:?}: expected the whole catalog once, in name order, got {walked:?}"
        );
        let wanted_pages = expected.len().div_ceil(page_size).max(1);
        assert_eq!(
            pages, wanted_pages,
            "case {case:?}: expected {wanted_pages} page(s) of {page_size}, got {pages}"
        );
    }

    // A page that stops short of the end carries the token, and the package it resumes
    // after is the last one it reported.
    let first = list(&emulator, "maxResults=3").await;
    let token = first
        .text("nextToken")
        .unwrap_or_else(|| panic!("a truncated listing carries a token, got {:?}", first.body));
    let rest = list(&emulator, &format!("maxResults=100&nextToken={token}")).await;
    assert_eq!(
        listed_names("resumed after the third package", &rest),
        expected[3..].to_vec(),
        "case \"resumed after the third package\": expected the packages past the first \
         page, got {:?}",
        rest.body
    );
    assert_eq!(
        rest.text("nextToken"),
        None,
        "case \"resumed after the third package\": the last page carries no token, got {:?}",
        rest.body
    );

    // An empty catalog paged at any size is one empty page and no token: a page bound
    // does not turn "nothing to list" into a next set.
    let empty = Emulator::new();
    for page_size in [MIN_PAGE_SIZE, MAX_PAGE_SIZE] {
        let case = format!("an empty catalog at maxResults={page_size}");
        let (walked, pages) = walk_pages(&case, &empty, page_size, 0).await;
        assert_eq!(
            (walked.as_slice(), pages),
            ([].as_slice(), 1),
            "case {case:?}: expected one empty page, got {walked:?} over {pages} page(s)"
        );
    }
}

/// RULE: a request the listing does not admit — a `nextToken` it never issued, a token
/// belonging to another listing, a `maxResults` outside the modelled range — is refused
/// and lists nothing; both published bounds of the range are served.
#[tokio::test]
async fn a_request_the_listing_does_not_admit_is_refused() {
    let case = "four packages, refused requests";
    let created = ["refusal-a", "refusal-b", "refusal-c", "refusal-d"];
    let emulator = seeded(case, &created).await;

    // A token issued by this listing, to pin that what the refusals below reject is the
    // token and not the resuming.
    let truncated = list(&emulator, "maxResults=1").await;
    let own_token = truncated.text("nextToken").unwrap_or_else(|| {
        panic!(
            "a truncated listing carries a token, got {:?}",
            truncated.body
        )
    });

    // A token is a position in ONE listing: anything else names no position of it. The
    // rows span the ways a token can fail to be one — not hex at all, hex of something
    // else, a token of ANOTHER listing of this service (`ListPackageVersions`), and a
    // position outside what `PackageName` admits at either end.
    let refusals: [(&str, String); 7] = [
        ("a token that is not a token of any listing", "not-a-token".to_string()),
        ("a token that is hex but names no position", hex::encode("nonsense")),
        (
            "a token of another listing of this service",
            hex::encode("refusal.package\u{1e}\u{1f}0\u{1e}v1"),
        ),
        (
            "a token whose marker names another listing",
            hex::encode("things\u{1f}refusal-a"),
        ),
        (
            "a token resuming after a name no package may carry",
            hex::encode("packages\u{1f}not a package name"),
        ),
        (
            "a token resuming after an empty name",
            hex::encode("packages\u{1f}"),
        ),
        (
            "a token resuming after a name past the modelled length",
            hex::encode(format!("packages\u{1f}{}", "p".repeat(129))),
        ),
    ];

    for (label, token) in &refusals {
        let refused = list(&emulator, &format!("nextToken={token}")).await;
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
            refused.body.get("packageSummaries"),
            None,
            "case {label:?}: a refused request lists nothing, got {:?}",
            refused.body
        );
    }

    // The issuing listing still accepts its own token: refusing a token is about the
    // listing it belongs to, not about tokens.
    let resumed = list(&emulator, &format!("maxResults=1&nextToken={own_token}")).await;
    assert_eq!(
        resumed.status, 200,
        "case \"the issuing listing replays its own token\": expected 200, got {resumed:?}"
    );
    assert_eq!(
        listed_names("the issuing listing replays its own token", &resumed),
        vec![created[1].to_string()],
        "case \"the issuing listing replays its own token\": expected the package after \
         the first, got {:?}",
        resumed.body
    );

    // `PackageCatalogMaxResults` is `range 1..100`: both published bounds are served, and
    // a value outside the range is refused before the listing runs.
    for served in [MIN_PAGE_SIZE, MAX_PAGE_SIZE] {
        let listed = list(&emulator, &format!("maxResults={served}")).await;
        assert_eq!(
            listed.status, 200,
            "case maxResults={served}: the modelled bound must be served, got {listed:?}"
        );
    }
    for out_of_range in ["0", "-1", "101"] {
        let refused = list(&emulator, &format!("maxResults={out_of_range}")).await;
        assert_eq!(
            refused.status, 400,
            "case maxResults={out_of_range}: must be refused 400, got {refused:?}"
        );
        assert_eq!(
            refused.body.get("packageSummaries"),
            None,
            "case maxResults={out_of_range}: a refused request lists nothing, got {:?}",
            refused.body
        );
    }
}

/// RULE: the listing projects the catalog as it IS — a default version `UpdatePackage`
/// promoted is reported, a package `DeletePackage` removed is listed no more — and it
/// changes nothing a later caller sees.
#[tokio::test]
async fn the_listing_projects_the_catalog_as_it_is() {
    let case = "the catalog as it is";
    let created = ["project-c", "project-a", "project-b"];
    let emulator = seeded(case, &created).await;

    // Promoting a default version is a change to the record, so the summary reports it —
    // for that package and no other.
    let promoted = created[1];
    let version = "1.0.0";
    let version_created = create_version(&emulator, promoted, version).await;
    assert_eq!(
        version_created.status, 200,
        "case {case:?}: a version of {promoted:?} must be creatable, got {version_created:?}"
    );
    let updated = update_package(
        &emulator,
        promoted,
        &json!({ "defaultVersionName": version }),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: promoting {version:?} must succeed, got {updated:?}"
    );

    let listed = list(&emulator, "").await;
    for summary in summaries(case, &listed) {
        let name = summary
            .get("packageName")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: a summary names its package"))
            .to_string();
        let expected_default = if name == promoted { Some(version) } else { None };
        assert_eq!(
            summary.get("defaultVersionName").and_then(Value::as_str),
            expected_default,
            "case {case:?}: {name:?} expected defaultVersionName {expected_default:?}, \
             got {summary:?}"
        );
        // An update moves `lastModifiedDate` and leaves `creationDate` where the create
        // put it, so the promoted package is the one whose two stamps differ.
        let creation = summary
            .get("creationDate")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: {name:?} reports a creationDate"));
        let modified = summary
            .get("lastModifiedDate")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: {name:?} reports a lastModifiedDate"));
        assert!(
            modified >= creation,
            "case {case:?}: {name:?} was last modified at {modified}, before its creation \
             at {creation}"
        );
        if name != promoted {
            assert!(
                (modified - creation).abs() < f64::EPSILON,
                "case {case:?}: {name:?} was never updated, got {modified} against {creation}"
            );
        }
    }

    // A listing is a read: the single-package reader answers the same record before and
    // after it.
    let before = emulator.get(promoted).await;
    let _ = list(&emulator, "maxResults=1").await;
    let after = emulator.get(promoted).await;
    assert_eq!(
        before.body, after.body,
        "case {case:?}: a listing must change nothing a later caller sees, got {after:?} \
         against {before:?}"
    );

    // A package the catalog no longer holds is listed no more, and the rest are left
    // exactly as they were.
    let removed = created[0];
    let deleted = delete_package(&emulator, removed).await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: deleting {removed:?} must succeed, got {deleted:?}"
    );
    let after_delete = list(&emulator, "").await;
    let remaining: Vec<&str> = created
        .iter()
        .copied()
        .filter(|name| *name != removed)
        .collect();
    assert_eq!(
        listed_names(case, &after_delete),
        in_name_order(&remaining),
        "case {case:?}: a deleted package is listed no more, got {:?}",
        after_delete.body
    );
}
