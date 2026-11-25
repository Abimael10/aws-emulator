// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for ONE rule about an AWS-official lens's catalog metadata: it is
//! PUBLISHED data about the lens, not data about the run that serves it.
//!
//! A lens Amazon Web Services publishes carries its own `LensName`, `Description`,
//! `LensVersion` and the instants it was created and last updated. Those members describe
//! the lens, so the service reads them from that lens's one catalog row
//! (`state::OFFICIAL_LENS_CATALOG`): none of them is minted while a request is served, none
//! is shared with another lens's row, and none varies with how the catalog is asked for.
//! The rules that follow, each pinned over a table of inputs:
//!
//! 1. Every read reports the same published data for the same lens: a second `ListLenses`,
//!    after writes that change what the store holds, repeats the first one's `Description`,
//!    `LensVersion`, `CreatedAt`, `UpdatedAt`, `Owner` and `LensStatus` for every lens. An
//!    instant taken from the serving process's clock, or a description or version derived
//!    from the store, could not survive this.
//! 2. The published data does not vary with the request: whatever Region a caller signs in
//!    and however small a page it asks for, each lens is served exactly once and carries
//!    the data the single-page read in another Region reported — the AWS-official lens ARN
//!    included, which names no Region.
//! 3. `GetLens` reports the same published data as the lens's `ListLenses` summary, for
//!    EVERY lens the catalog carries rather than for a chosen few: two independent
//!    responses about one lens cannot disagree about its name, description, version, ARN or
//!    owner.
//! 4. `CreatedAt` and `UpdatedAt` are each lens's OWN published instants, held as that row's
//!    data: independent of the day its `LensVersion` names in both directions, so a lens AWS
//!    revised after publishing the version it carries reports a later `UpdatedAt`.
//!
//! The rows are the catalog's own lenses, which differ in name, in where the version they
//! report comes from and in the question set behind them; rule 2's rows differ in signed
//! Region and in page size. Every assertion compares two INDEPENDENT responses, or a
//! response against the modelled bounds, rather than re-stating a value the catalog itself
//! carries — the catalog's published values are pinned by `super::official_lens_catalog`
//! and `super::official_lens_catalog_metadata`.
//!
//! Requests go through the real `MultiServiceRouter`, so routing, restJson1
//! (de)serialization, boundary constraint validation and the handler all run as they do in
//! production. The client and the resource builders are the shared `super::workload_harness`.

use std::collections::BTreeMap;

use serde_json::Value;

use service_com_amazonaws_wellarchitected_wellarchitectedapiservicelambda::state;

use super::workload_harness::{
    created_id, encode_path_segment, minimal_create, review_template, Client, SIGNED_REGION,
};

/// The `LensSummary` members that carry a lens's published identity and catalog text, each
/// modelled as a string: `LensArn`, `LensAlias`, `LensName`, `LensType`, `Description`,
/// `LensVersion`, `Owner` and `LensStatus`
/// (`smithy_query(query="shape", shape="LensSummary")`).
const PUBLISHED_TEXT_MEMBERS: [&str; 8] = [
    "LensArn",
    "LensAlias",
    "LensName",
    "LensType",
    "Description",
    "LensVersion",
    "Owner",
    "LensStatus",
];

/// The two `LensSummary` members that carry an instant — "The date and time when the lens
/// was created" and "… last updated" (`smithy_query(query="shape", shape="LensSummary")`),
/// which restJson1 carries as epoch seconds.
const PUBLISHED_INSTANT_MEMBERS: [&str; 2] = ["CreatedAt", "UpdatedAt"];

/// `LensDescription`'s modelled `@length(min: 1, max: 1024)` and `LensVersion`'s
/// `@length(min: 1, max: 32)` (`smithy_query(query="shape", shape="LensSummary")`), as the
/// (member, min, max) rows a summary is checked against.
const MODELLED_TEXT_LENGTHS: [(&str, usize, usize); 4] = [
    ("Description", 1, 1024),
    ("LensVersion", 1, 32),
    ("LensName", 1, 128),
    ("LensAlias", 1, 128),
];

/// One member of a JSON object as text, empty when the object carries none.
fn text(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One lens's published catalog data as a response carries it, keyed by member name: every
/// text member beside the two instants, so two responses about the same lens can be
/// compared member by member.
///
/// Each member is checked for presence here, so a comparison between two responses that
/// both omit one cannot pass as agreement.
fn published_data(summary: &Value, case: &str) -> BTreeMap<String, String> {
    let mut data = BTreeMap::new();
    for member in PUBLISHED_TEXT_MEMBERS {
        let value = text(summary, member);
        assert!(
            !value.is_empty(),
            "case {case:?}: a published lens reports {member}, got {summary:?}"
        );
        data.insert(member.to_string(), value);
    }
    for member in PUBLISHED_INSTANT_MEMBERS {
        let at = summary.get(member).and_then(Value::as_f64);
        let at = at.filter(|at| *at > 0.0).unwrap_or_else(|| {
            panic!("case {case:?}: a published lens reports {member} as an instant, got {summary:?}")
        });
        data.insert(member.to_string(), format!("{at}"));
    }
    for (member, min, max) in MODELLED_TEXT_LENGTHS {
        let length = text(summary, member).chars().count();
        assert!(
            length >= min && length <= max,
            "case {case:?}: {member} is @length({min}, {max}), got {length} characters in \
             {summary:?}"
        );
    }
    data
}

/// The AWS-official half of the lens catalog as one Region's endpoint serves it at page
/// size `page_size`, keyed by `LensAlias`: the pages are followed to the end, so a case
/// that asks for one lens per page reports the same catalog as one that asks for all of it.
async fn official_catalog(
    client: &Client,
    region: &str,
    page_size: usize,
    case: &str,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let expected = state::OFFICIAL_LENS_CATALOG.len();
    let mut catalog: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut token: Option<String> = None;
    // One page per lens at worst, plus the page that ends the walk: a token that did not
    // advance would otherwise spin here instead of failing.
    for _ in 0..=expected {
        let mut uri = format!("/lenses?LensType=AWS_OFFICIAL&MaxResults={page_size}");
        if let Some(resume) = &token {
            uri.push_str(&format!("&NextToken={}", encode_path_segment(resume)));
        }
        let answer = client.send_in(region, "GET", &uri, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected ListLenses 200, got {answer:?}"
        );
        let summaries = answer
            .body
            .get("LensSummaries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // A page holds at most what was asked for; an empty page is legal as the one that
        // ends the walk, which the token assertion below is what pins.
        assert!(
            summaries.len() <= page_size,
            "case {case:?}: a page holds at most {page_size} lenses, got {answer:?}"
        );
        for summary in &summaries {
            let alias = text(summary, "LensAlias");
            let data = published_data(summary, case);
            assert!(
                catalog.insert(alias.clone(), data).is_none(),
                "case {case:?}: {alias:?} must be served once over the walk, got it twice"
            );
        }
        token = answer
            .body
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        token, None,
        "case {case:?}: the walk over the catalog must end, got a token after \
         {} lenses",
        catalog.len()
    );
    assert_eq!(
        catalog.len(),
        expected,
        "case {case:?}: expected one summary per catalog entry, got {:?}",
        catalog.keys().collect::<Vec<_>>()
    );
    catalog
}

/// Writes that change what the store holds, so a second read of the catalog is served by a
/// process whose state has moved on: a workload reviewing two lenses, and a review template
/// applying one.
async fn change_the_store(client: &Client, case: &str) {
    let mut request = minimal_create(&format!("catalog-reader-{case}"), &format!("tok-{case}"));
    request["Lenses"] = Value::Array(vec![
        Value::String("wellarchitected".to_string()),
        Value::String("serverless".to_string()),
    ]);
    let workload_id = created_id(&client.create(request).await, case);
    assert!(
        !workload_id.is_empty(),
        "case {case:?}: the workload the second read is served beside must exist"
    );
    review_template(client, &format!("catalog-template-{case}"), &["iot"]).await;
}

/// Rule 1: the published data of every lens survives a second read across writes.
#[tokio::test]
async fn a_lens_reports_the_same_published_data_to_every_read() {
    let client = Client::new();
    let first = official_catalog(&client, SIGNED_REGION, 50, "first-read").await;

    change_the_store(&client, "between-reads").await;

    let second = official_catalog(&client, SIGNED_REGION, 50, "second-read").await;
    for (alias, published) in &first {
        let again = second.get(alias).unwrap_or_else(|| {
            panic!("case {alias:?}: the lens must still be listed, got {second:?}")
        });
        assert_eq!(
            published, again,
            "case {alias:?}: a lens's published data is the same on every read, expected \
             {published:?}, got {again:?}"
        );
    }
    assert_eq!(
        first.len(),
        second.len(),
        "case \"catalog\": the catalog a read reports does not grow with the store, expected \
         {:?}, got {:?}",
        first.keys().collect::<Vec<_>>(),
        second.keys().collect::<Vec<_>>()
    );
}

/// Rule 2: the same published data whatever Region is signed and however the catalog is
/// paged.
#[tokio::test]
async fn published_data_does_not_vary_by_region_or_page_size() {
    let client = Client::new();
    let baseline = official_catalog(&client, SIGNED_REGION, 50, "baseline").await;

    // (case, signed Region, MaxResults) — rows that differ in the Region the caller signs
    // in and in how many lenses a page may hold, down to one lens per page.
    let rows: [(&str, &str, usize); 4] = [
        ("one-lens-per-page", SIGNED_REGION, 1),
        ("small-page-another-region", "us-east-1", 3),
        ("full-page-another-region", "eu-central-1", 50),
        ("page-wider-than-the-catalog", "ap-southeast-2", 50),
    ];
    for (case, region, page_size) in rows {
        let catalog = official_catalog(&client, region, page_size, case).await;
        for (alias, published) in &baseline {
            let reported = catalog.get(alias).unwrap_or_else(|| {
                panic!("case {case:?}/{alias:?}: the lens must be listed, got {catalog:?}")
            });
            assert_eq!(
                published, reported,
                "case {case:?}/{alias:?}: a lens's published data does not vary with the \
                 request, expected {published:?}, got {reported:?}"
            );
        }
        assert_eq!(
            catalog.len(),
            baseline.len(),
            "case {case:?}: the walk reports the whole catalog, got {:?}",
            catalog.keys().collect::<Vec<_>>()
        );
    }
}

/// Rule 3: `GetLens` and the lens's `ListLenses` summary agree, for every catalog lens.
#[tokio::test]
async fn get_lens_reports_the_same_published_data_as_the_listing() {
    let client = Client::new();
    let catalog = official_catalog(&client, SIGNED_REGION, 50, "catalog").await;

    for official in state::OFFICIAL_LENS_CATALOG {
        let case = official.alias;
        let listed = catalog.get(official.alias).unwrap_or_else(|| {
            panic!("case {case:?}: the lens must be listed, got {catalog:?}")
        });

        let answer = client
            .send(
                "GET",
                &format!("/lenses/{}", encode_path_segment(official.alias)),
                None,
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected GetLens 200, got {answer:?}"
        );
        let lens = answer.body.get("Lens").cloned().unwrap_or(Value::Null);

        // `GetLens` names the lens in `Lens$Name`; a summary in `LensSummary$LensName`.
        // Every other member is spelled the same on both shapes.
        let read: [(&str, String, String); 5] = [
            ("LensName", text(&lens, "Name"), listed["LensName"].clone()),
            (
                "Description",
                text(&lens, "Description"),
                listed["Description"].clone(),
            ),
            (
                "LensVersion",
                text(&lens, "LensVersion"),
                listed["LensVersion"].clone(),
            ),
            ("LensArn", text(&lens, "LensArn"), listed["LensArn"].clone()),
            ("Owner", text(&lens, "Owner"), listed["Owner"].clone()),
        ];
        for (member, reported, expected) in read {
            assert!(
                !reported.is_empty(),
                "case {case:?}: GetLens reports {member} for a lens the catalog carries, got \
                 {lens:?}"
            );
            assert_eq!(
                reported, expected,
                "case {case:?}: GetLens and ListLenses must agree about {member}, expected \
                 {expected:?}, got {reported:?}"
            );
        }
    }
}

/// The instant `stamp` names, in epoch seconds — how a restJson1 `Timestamp` travels.
fn epoch_seconds(stamp: &str) -> f64 {
    ::aws_smithy_types::DateTime::from_str(stamp, ::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| panic!("a published instant must parse: {stamp:?} ({error})"))
        .as_secs_f64()
}

/// Rule 4: `ListLenses` reports each AWS-official lens's OWN published creation and
/// last-revision instants, held as that row's data rather than read off the day its
/// `LensVersion` names.
///
/// The rows span the three cases the catalog holds: a lens AWS has revised SINCE the version
/// it carries was published (so `UpdatedAt` falls AFTER that day), a lens whose published
/// instant falls BEFORE the day its version names (so neither instant is derivable from the
/// version, in either direction), and a lens this catalog carries no published instant for,
/// which reports the start of the day its version names.
///
/// SOURCE of the quoted instants: the instants AWS's lens catalog publishes for those
/// lenses, as `state::OFFICIAL_LENS_CATALOG` carries them per row.
#[tokio::test]
async fn list_lenses_reports_each_lenss_own_published_instants() {
    // (LensAlias, CreatedAt, UpdatedAt, whether UpdatedAt falls after the version's day)
    let rows: [(&str, &str, &str, bool); 3] = [
        (
            "wellarchitected",
            "2019-09-13T20:13:30Z",
            "2026-10-01T11:23:33Z",
            true,
        ),
        (
            "connectedmobility",
            "2024-03-21T19:50:10Z",
            "2024-03-21T19:50:10Z",
            false,
        ),
        // No published instant of its own: the start of the day its version names.
        ("sap", "2025-01-15T00:00:00Z", "2025-01-15T00:00:00Z", false),
    ];

    let client = Client::new();
    let catalog = official_catalog(&client, SIGNED_REGION, 50, "instants").await;
    // A listing is a REPORT of what the catalog holds, so a second one answers the same
    // instants: neither is minted while the request is served.
    let again = official_catalog(&client, SIGNED_REGION, 50, "instants-again").await;

    for (alias, created, updated, revised_after_version_day) in rows {
        let case = alias;
        let official = state::official_lens(alias)
            .unwrap_or_else(|| panic!("case {case:?}: the catalog must carry the lens"));
        let listed = catalog
            .get(alias)
            .unwrap_or_else(|| panic!("case {case:?}: the lens must be listed, got {catalog:?}"));

        let reported_created = listed["CreatedAt"].clone();
        let reported_updated = listed["UpdatedAt"].clone();
        assert_eq!(
            reported_created,
            format!("{}", epoch_seconds(created)),
            "case {case:?}: expected CreatedAt {created:?}, got {listed:?}"
        );
        assert_eq!(
            reported_updated,
            format!("{}", epoch_seconds(updated)),
            "case {case:?}: expected UpdatedAt {updated:?}, got {listed:?}"
        );
        assert!(
            epoch_seconds(created) <= epoch_seconds(updated),
            "case {case:?}: a lens is created no later than it is revised, got {listed:?}"
        );
        assert_eq!(
            again.get(alias).map(|row| row["UpdatedAt"].clone()),
            Some(reported_updated),
            "case {case:?}: reading the catalog must not move the lens's instants"
        );

        // The pair is a fact about the LENS: the revision instant of a lens AWS revised
        // after publishing this version falls after that version's day, while another
        // lens's falls before its own version day — so neither can be read off the version
        // string, in either direction.
        let version_day = epoch_seconds(&format!("{}T00:00:00Z", official.version));
        assert_eq!(
            epoch_seconds(updated) > version_day,
            revised_after_version_day,
            "case {case:?}: expected UpdatedAt {} the day the version names, got {updated:?} \
             against {:?}",
            if revised_after_version_day {
                "after"
            } else {
                "not after"
            },
            official.version
        );
        if official.revised_at.is_some() {
            assert_ne!(
                epoch_seconds(updated),
                version_day,
                "case {case:?}: a published instant is not the start of the version day, got \
                 {updated:?}"
            );
        }
    }
}
