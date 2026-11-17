// SPDX-License-Identifier: CC0-1.0

//! How `ListAccessPoints` SCOPES a listing to one bucket, driven end to end
//! over HTTP.
//!
//! The rule under test: the `bucket` query parameter is a FILTER over the
//! access points of the account the request names — the listing answers 200
//! with the page of the access points fronting that bucket, and that page is
//! EMPTY when none of them does — so the only scope this service refuses is one
//! naming a bucket the store that owns buckets does not hold, because then
//! there is no bucket whose access points could be listed.
//!
//! The rows differ along what the rule generalises over: how many access points
//! front the bucket (two, one, none), how a bucket came to have none (never
//! fronted, or fronted by one that was since deleted), the length of the
//! bucket's name at both ends of `ListAccessPointsRequest$Bucket`'s modelled
//! 3..255 bounds and one past each, whether an absent name is a prefix, an
//! extension or another case of a bucket that IS held, and the page size the
//! scoped listing asks for. One suite beside them pins the ORDER the two
//! decisions are made in: the account a request names is authorized before the
//! bucket the scope names is resolved, so every scope earns one refusal in
//! another account's control plane.
//!
//! The harness is the lifecycle suite's: the same router, request builder and
//! XML readers a client's request meets.

use std::collections::BTreeSet;

use crate::access_point_lifecycle::{
    assert_foreign_account_refused, xml_escape, Emulator, Reply, ACCOUNT, NO_CONTENT,
    OTHER_ACCOUNT, XMLNS,
};

/// The wire `<Code>` this service refuses a request it will not carry out with.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The sentence this service answers a scope naming a bucket it does not hold
/// with. Written out here rather than imported from the implementation, so this
/// suite states the wording a client reads independently of the code that
/// builds it.
const NO_SUCH_BUCKET_TO_SCOPE_BY: &str = "No access point attached to this bucket";

/// `ListAccessPointsRequest$Bucket`'s modelled length bounds
/// (`smithy_query(query="operation", operation="ListAccessPoints")`: `Bucket`
/// targets `BucketName`, `length 3..255`), which the rows below drive at both
/// ends and one character past each.
const BUCKET_NAME_LEN: ::std::ops::RangeInclusive<usize> = 3..=255;

/// Create one access point named `name` over `bucket`, recording the bucket in
/// the store that owns it first — an access point is created only over a bucket
/// that exists.
async fn create(emulator: &Emulator, name: &str, bucket: &str) {
    emulator.provision_bucket(name, bucket);
    let body = format!(
        "<CreateAccessPointRequest xmlns=\"{XMLNS}\"><Bucket>{}</Bucket>\
         </CreateAccessPointRequest>",
        xml_escape(bucket)
    );
    let created = emulator
        .call(
            "PUT",
            &format!("/v20180820/accesspoint/{name}"),
            ACCOUNT,
            &body,
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: expected the access point to be created over {bucket:?}, got {} with \
         body {}",
        created.status, created.body
    );
}

/// Delete the access point named `name`.
async fn delete(emulator: &Emulator, name: &str) {
    let deleted = emulator
        .call(
            "DELETE",
            &format!("/v20180820/accesspoint/{name}"),
            ACCOUNT,
            "",
        )
        .await;
    assert_eq!(
        deleted.status, NO_CONTENT,
        "case {name:?}: expected the access point to be deleted, got {} with body {}",
        deleted.status, deleted.body
    );
}

/// `ListAccessPoints` scoped to `bucket`, with `query` appended for the rows
/// that also ask for a page size or resume a scan.
async fn scoped(emulator: &Emulator, bucket: &str, query: &str) -> Reply {
    let uri = match query.is_empty() {
        true => format!("/v20180820/accesspoint?bucket={bucket}"),
        false => format!("/v20180820/accesspoint?bucket={bucket}&{query}"),
    };
    emulator.call("GET", &uri, ACCOUNT, "").await
}

/// The access point names a listing reported.
fn names(reply: &Reply) -> BTreeSet<String> {
    reply.texts("Name").into_iter().collect()
}

/// One row of the scope table: the bucket a listing is scoped to, and what that
/// scope selects — `Some(names)` for a page this service answers (empty when
/// the filter selects none of the account's access points), `None` for the one
/// scope it refuses.
struct Row {
    case: &'static str,
    bucket: String,
    selected: Option<Vec<&'static str>>,
}

impl Row {
    fn page(case: &'static str, bucket: impl Into<String>, selected: &[&'static str]) -> Self {
        Self {
            case,
            bucket: bucket.into(),
            selected: Some(selected.to_vec()),
        }
    }

    fn refused(case: &'static str, bucket: impl Into<String>) -> Self {
        Self {
            case,
            bucket: bucket.into(),
            selected: None,
        }
    }
}

/// A bucket name beginning with `stem` and exactly `length` characters long,
/// so a row can name a bucket at a length the member bounds without the name
/// itself carrying any other meaning.
fn bucket_named(stem: &str, length: usize) -> String {
    match length.checked_sub(stem.len()) {
        Some(padding) => format!("{stem}{}", "b".repeat(padding)),
        None => stem[..length].to_string(),
    }
}

#[tokio::test]
async fn a_bucket_scope_selects_that_buckets_access_points_and_may_select_none() {
    let emulator = Emulator::start();
    let shortest = bucket_named("held", *BUCKET_NAME_LEN.start());
    let longest = bucket_named("held-", *BUCKET_NAME_LEN.end());

    // Buckets access points front: one with two, one with one.
    create(&emulator, "reports-ap", "reports").await;
    create(&emulator, "reports-audit-ap", "reports").await;
    create(&emulator, "finance-ap", "finance").await;
    // A bucket whose only access point was DELETED: it is a bucket with none
    // attached that once had one, so the answer is about the bucket now rather
    // than about its history.
    create(&emulator, "retired-ap", "retired").await;
    delete(&emulator, "retired-ap").await;
    // Buckets the store holds that no access point ever fronted, at the two
    // ends of the member's modelled length.
    for bucket in ["archive", shortest.as_str(), longest.as_str()] {
        emulator.provision_bucket("a bucket with no access point", bucket);
    }

    let rows = [
        Row::page(
            "a bucket two access points front",
            "reports",
            &["reports-ap", "reports-audit-ap"],
        ),
        Row::page("a bucket one access point fronts", "finance", &["finance-ap"]),
        Row::page("a bucket that is held with none attached", "archive", &[]),
        Row::page(
            "a bucket whose only access point was deleted",
            "retired",
            &[],
        ),
        Row::page(
            "a held bucket named at the shortest admitted length",
            shortest.clone(),
            &[],
        ),
        Row::page(
            "a held bucket named at the longest admitted length",
            longest.clone(),
            &[],
        ),
        Row::refused("a bucket the store does not hold", "ghost"),
        Row::refused("a prefix of a bucket that is held", "report"),
        Row::refused("an extension of a bucket that is held", "reportsx"),
        Row::refused("a held bucket's name in another case", "Reports"),
        Row::refused(
            "an absent bucket named at the longest admitted length",
            bucket_named("gone-", *BUCKET_NAME_LEN.end()),
        ),
    ];

    for row in &rows {
        let listed = scoped(&emulator, &row.bucket, "").await;
        let Some(selected) = row.selected.as_ref() else {
            assert_eq!(
                (
                    listed.status,
                    listed.code().as_str(),
                    listed.text("Message").as_deref()
                ),
                (400, INVALID_REQUEST, Some(NO_SUCH_BUCKET_TO_SCOPE_BY)),
                "case {:?}: expected a scope naming a bucket that is not there to be refused, \
                 got {} with body {}",
                row.case,
                listed.status,
                listed.body
            );
            assert!(
                !listed.body.contains("ListAccessPointsResult"),
                "case {:?}: expected no listing beside the refusal, got body {}",
                row.case,
                listed.body
            );
            continue;
        };

        let expected: BTreeSet<String> =
            selected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            listed.status, 200,
            "case {:?}: expected the scoped listing to answer 200, got {} with body {}",
            row.case, listed.status, listed.body
        );
        assert_eq!(
            names(&listed),
            expected,
            "case {:?}: expected exactly the access points fronting that bucket, got body {}",
            row.case,
            listed.body
        );
        assert!(
            listed.body.contains("AccessPointList"),
            "case {:?}: expected the answer to carry the listing member, got body {}",
            row.case,
            listed.body
        );
        assert!(
            listed.text("NextToken").is_none(),
            "case {:?}: expected no continuation token when the page holds the whole scope, \
             got body {}",
            row.case,
            listed.body
        );

        // A read may not change what a later reader sees: the same scope
        // answers the same page again.
        let again = scoped(&emulator, &row.bucket, "").await;
        assert_eq!(
            (again.status, names(&again)),
            (listed.status, names(&listed)),
            "case {:?}: expected a repeated scoped listing to answer identically, got body {}",
            row.case,
            again.body
        );
    }

    // None of the refusals and none of the empty pages changed the account's
    // access points.
    let all = emulator.call("GET", "/v20180820/accesspoint", ACCOUNT, "").await;
    assert_eq!(
        (all.status, names(&all)),
        (
            200,
            ["finance-ap", "reports-ap", "reports-audit-ap"]
                .into_iter()
                .map(str::to_string)
                .collect()
        ),
        "expected the account's own listing to be untouched by the scoped reads, got body {}",
        all.body
    );
}

#[tokio::test]
async fn a_scope_that_selects_nothing_pages_as_an_empty_page_at_every_page_size() {
    let emulator = Emulator::start();
    create(&emulator, "reports-ap", "reports").await;
    create(&emulator, "reports-audit-ap", "reports").await;
    emulator.provision_bucket("a bucket with no access point", "archive");

    // One row per page size the member admits: the default (`MaxResults` 0,
    // which names no page size), one, as many as the account holds, and the
    // modelled maximum. A scope that selects nothing is an empty FIRST page at
    // every one of them — never a refusal, and never a continuation token,
    // since no record follows.
    for page_size in [0usize, 1, 2, 1000] {
        let empty = scoped(&emulator, "archive", &format!("maxResults={page_size}")).await;
        assert_eq!(
            (empty.status, names(&empty)),
            (200, BTreeSet::new()),
            "case page size {page_size}: expected an empty page for a bucket with no access \
             point, got {} with body {}",
            empty.status,
            empty.body
        );
        assert!(
            empty.text("NextToken").is_none(),
            "case page size {page_size}: expected no continuation token on an empty page, got \
             body {}",
            empty.body
        );
    }

    // The same page size over a scope that DOES select access points still
    // pages through them, so the empty page above is the filter selecting
    // nothing rather than the scope being ignored or the paging being broken.
    let first = scoped(&emulator, "reports", "maxResults=1").await;
    assert_eq!(
        first.texts("Name").len(),
        1,
        "expected one entry on a one-entry page, got body {}",
        first.body
    );
    let token = first
        .text("NextToken")
        .expect("expected a continuation token while a second access point remains");
    let second = scoped(&emulator, "reports", &format!("maxResults=1&nextToken={token}")).await;
    assert_eq!(
        second.status, 200,
        "expected the resumed page to answer 200, got {} with body {}",
        second.status, second.body
    );
    let walked: BTreeSet<String> = names(&first).union(&names(&second)).cloned().collect();
    assert_eq!(
        walked,
        ["reports-ap", "reports-audit-ap"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<String>>(),
        "expected paging a scoped listing to cover exactly that bucket's access points, got \
         {walked:?}"
    );
    assert!(
        second.text("NextToken").is_none(),
        "expected no continuation token once the scope is exhausted, got body {}",
        second.body
    );
}

#[tokio::test]
async fn the_account_is_authorized_ahead_of_the_bucket_the_scope_names() {
    let emulator = Emulator::start();
    create(&emulator, "reports-ap", "reports").await;
    emulator.provision_bucket("a bucket with no access point", "archive");

    // The account a request names says whose control plane the listing is for,
    // so it is authorized BEFORE the scope is resolved: every scope earns the
    // same refusal in another account's control plane — the bucket that is
    // fronted, the bucket that is held with none attached, and the bucket that
    // is not there — so neither the bucket's existence nor its access points
    // are disclosed.
    for (case, bucket) in [
        ("a bucket an access point fronts", "reports"),
        ("a bucket held with none attached", "archive"),
        ("a bucket the store does not hold", "ghost"),
    ] {
        let refused = emulator
            .call(
                "GET",
                &format!("/v20180820/accesspoint?bucket={bucket}"),
                OTHER_ACCOUNT,
                "",
            )
            .await;
        assert_foreign_account_refused(case, OTHER_ACCOUNT, &refused);
        assert!(
            refused.texts("Name").is_empty() && refused.text("Bucket").is_none(),
            "case {case:?}: expected nothing of the account's access points beside the \
             refusal, got body {}",
            refused.body
        );
    }
}

#[tokio::test]
async fn a_bucket_name_outside_the_modelled_length_is_not_answered_as_a_listing() {
    let emulator = Emulator::start();
    create(&emulator, "reports-ap", "reports").await;

    // One character short of the modelled minimum and one past the maximum: a
    // value the member does not admit is refused as the shape violation it is —
    // the boundary's own refusal, under the code it carries — never served as a
    // page, and never as the scope refusal above, which is about a bucket name
    // the member DOES admit.
    for length in [*BUCKET_NAME_LEN.start() - 1, *BUCKET_NAME_LEN.end() + 1] {
        let bucket = bucket_named("ab", length);
        let refused = scoped(&emulator, &bucket, "").await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (400, "ValidationException"),
            "case length {length}: expected a bucket name outside the modelled bounds to be \
             refused as a shape violation, got {} with body {}",
            refused.status,
            refused.body
        );
        assert!(
            refused.texts("Name").is_empty(),
            "case length {length}: expected no listing beside the refusal, got body {}",
            refused.body
        );
    }
}
