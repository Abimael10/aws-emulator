// SPDX-License-Identifier: CC0-1.0

//! WHAT one page of a paginated Parameter Store read carries, and WHERE the next one picks
//! up, end to end over the front door.
//!
//! RULE under test: **a page of a filtered, size-limited read carries the entries that
//! satisfy ALL of the request's filters, in the store's own total order (ascending
//! parameter name; ascending version for a history), cut at `MaxResults`; and a page
//! fetched with the `NextToken` the previous one handed out continues that same listing
//! STRICTLY AFTER the last entry that page served — never from the head of the store, and
//! never past an entry the store gained at a position after that cut.** A read that
//! resumed at a position fixed while the previous page was assembled steps over a
//! parameter created in between, so a client paging its whole Parameter Store silently
//! never sees it; one that restarted the listing would report the head of the store
//! again and never reach its tail.
//!
//! Every case goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol"), through the crate's one test
//! harness (`all/parameter_store.rs`). Every read follows the writes that created the
//! parameters, so a page that is not computed from persisted state cannot pass.
//!
//! The first suite is a table whose rows differ along what the rule generalises over: the
//! filter key, its comparison option and its values, which filter member carries it
//! (`ParameterFilters` or the deprecated `Filters`), how many parameters match, the page
//! size (the published minimum, sizes inside the listing, the match count exactly, one
//! more than it and the published maximum) and the shape of the store the listing is cut
//! from (hierarchy depth, both name spellings, every type, both tiers, tagged and
//! untagged). The parameters are created in an order that is NOT their name order, so a
//! page that reported them as they were written could not pass. The second suite covers
//! the resume cut for each of the three paginated parameter reads.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, router, ACCOUNT, REGION};

/// The published maximum of `DescribeParameters`'s `MaxResults`
/// (`smithy_query(query="operation", operation="DescribeParameters")`: range 1..50), which
/// is also the page size a request naming none is served up to.
const MAX_PAGE_SIZE: i64 = 50;

/// The published minimum of the same member.
const MIN_PAGE_SIZE: i64 = 1;

/// One parameter the suites below write, with the metadata a describe must report for it.
struct Seeded {
    /// The name as the put writes it, which is the name every read reports.
    name: &'static str,
    /// A `ParameterType` value.
    type_: &'static str,
    /// A `ParameterTier` value.
    tier: &'static str,
    /// A `Key=Value` tag the put attaches, or `None` for an untagged parameter.
    tag: Option<(&'static str, &'static str)>,
}

/// One case of the resume suite: a read, the first page of it, what the store gains
/// between the two calls, and the page its token must then serve.
struct Resume {
    case: &'static str,
    operation: &'static str,
    /// The request for the read's first page.
    first: Value,
    /// The names that first page reports, in order.
    first_names: Vec<&'static str>,
    /// The `PutParameter` body written between the two pages.
    written: Value,
    /// The names the page fetched with the first page's token must report, in order.
    second_names: Vec<&'static str>,
}

/// The store every case of the first suite is cut from: two hierarchies of different
/// depths under one prefix, a parameter outside it, both spellings of a top-level name,
/// both `ParameterType`s a plaintext parameter can carry, both tiers, and tagged beside
/// untagged parameters.
///
/// Held in the order the puts happen, which is deliberately NOT the order a listing
/// reports them in.
const SEEDED: &[Seeded] = &[
    Seeded { name: "/Paged/gamma", type_: "String", tier: "Standard", tag: None },
    Seeded { name: "/Paged/web/port", type_: "String", tier: "Advanced", tag: None },
    Seeded {
        name: "/Paged/db/user",
        type_: "String",
        tier: "Standard",
        tag: Some(("Env", "dev")),
    },
    Seeded {
        name: "/Paged/alpha",
        type_: "String",
        tier: "Standard",
        tag: Some(("Env", "prod")),
    },
    Seeded {
        name: "/Unpaged/one",
        type_: "String",
        tier: "Standard",
        tag: Some(("Env", "prod")),
    },
    Seeded {
        name: "/Paged/beta",
        type_: "StringList",
        tier: "Advanced",
        tag: Some(("Env", "dev")),
    },
    Seeded {
        name: "/Paged/db/password",
        type_: "String",
        tier: "Standard",
        tag: Some(("Env", "prod")),
    },
    Seeded {
        name: "/Paged/delta",
        type_: "String",
        tier: "Standard",
        tag: Some(("Env", "prod")),
    },
    Seeded { name: "Unpaged-top", type_: "String", tier: "Standard", tag: None },
    Seeded {
        name: "/Paged/web/host",
        type_: "StringList",
        tier: "Standard",
        tag: Some(("Env", "prod")),
    },
];

/// Every seeded parameter in the order a listing reports it: ascending canonical name (the
/// name without its leading separator, which is how the store is keyed), so `Unpaged-top`
/// sorts beside `/Unpaged/one` while each is reported under the name its put wrote.
const EVERY_NAME: &[&str] = &[
    "/Paged/alpha",
    "/Paged/beta",
    "/Paged/db/password",
    "/Paged/db/user",
    "/Paged/delta",
    "/Paged/gamma",
    "/Paged/web/host",
    "/Paged/web/port",
    "Unpaged-top",
    "/Unpaged/one",
];

/// The eight parameters of the `/Paged` prefix, in listing order.
const PAGED_NAMES: &[&str] = &[
    "/Paged/alpha",
    "/Paged/beta",
    "/Paged/db/password",
    "/Paged/db/user",
    "/Paged/delta",
    "/Paged/gamma",
    "/Paged/web/host",
    "/Paged/web/port",
];

/// Each page of a filtered, size-limited describe is the next slice of the filtered
/// listing: the entries that satisfy every filter, in the store's own order, cut at
/// `MaxResults` and resumed strictly after the previous page's last entry.
#[tokio::test]
async fn each_page_of_a_filtered_describe_is_the_next_slice_of_the_filtered_listing() {
    let router = router();
    seed(&router).await;

    // (case, the filter members of the request, the names the filtered listing holds, in
    // listing order)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        ("no filter at all", json!({}), EVERY_NAME.to_vec()),
        (
            "Name BeginsWith the common prefix",
            json!({ "ParameterFilters": [{ "Key": "Name", "Option": "BeginsWith", "Values": ["/Paged"] }] }),
            PAGED_NAMES.to_vec(),
        ),
        (
            "Path Recursive over the whole prefix",
            json!({ "ParameterFilters": [{ "Key": "Path", "Option": "Recursive", "Values": ["/Paged"] }] }),
            PAGED_NAMES.to_vec(),
        ),
        (
            "Path OneLevel, which reaches only the level below the path",
            json!({ "ParameterFilters": [{ "Key": "Path", "Option": "OneLevel", "Values": ["/Paged"] }] }),
            vec![
                "/Paged/alpha",
                "/Paged/beta",
                "/Paged/delta",
                "/Paged/gamma",
            ],
        ),
        (
            "Name Contains one level of the hierarchy",
            json!({ "ParameterFilters": [{ "Key": "Name", "Option": "Contains", "Values": ["/db/"] }] }),
            vec!["/Paged/db/password", "/Paged/db/user"],
        ),
        (
            "Type Equals one of the published types",
            json!({ "ParameterFilters": [{ "Key": "Type", "Values": ["String"] }] }),
            vec![
                "/Paged/alpha",
                "/Paged/db/password",
                "/Paged/db/user",
                "/Paged/delta",
                "/Paged/gamma",
                "/Paged/web/port",
                "Unpaged-top",
                "/Unpaged/one",
            ],
        ),
        (
            "the deprecated Filters member carrying the same narrowing",
            json!({ "Filters": [{ "Key": "Type", "Values": ["StringList"] }] }),
            vec!["/Paged/beta", "/Paged/web/host"],
        ),
        (
            "Tier Equals the advanced tier",
            json!({ "ParameterFilters": [{ "Key": "Tier", "Values": ["Advanced"] }] }),
            vec!["/Paged/beta", "/Paged/web/port"],
        ),
        (
            "a tag filter on the value one tag carries",
            json!({ "ParameterFilters": [{ "Key": "tag:Env", "Values": ["prod"] }] }),
            vec![
                "/Paged/alpha",
                "/Paged/db/password",
                "/Paged/delta",
                "/Paged/web/host",
                "/Unpaged/one",
            ],
        ),
        (
            "two filters, which narrow to what satisfies both",
            json!({
                "ParameterFilters": [
                    { "Key": "Path", "Option": "Recursive", "Values": ["/Paged"] },
                    { "Key": "Type", "Values": ["String"] },
                ],
            }),
            vec![
                "/Paged/alpha",
                "/Paged/db/password",
                "/Paged/db/user",
                "/Paged/delta",
                "/Paged/gamma",
                "/Paged/web/port",
            ],
        ),
        (
            "a filter in each member, which narrow together",
            json!({
                "ParameterFilters": [{ "Key": "DataType", "Values": ["text"] }],
                "Filters": [{ "Key": "Name", "Values": ["/Paged/alpha"] }],
            }),
            vec!["/Paged/alpha"],
        ),
        (
            "a filter nothing satisfies",
            json!({ "ParameterFilters": [{ "Key": "Name", "Option": "BeginsWith", "Values": ["/NothingHere"] }] }),
            vec![],
        ),
    ];

    for (case, filters, expected) in cases {
        // The whole filtered listing, unpaged: the page sizes below are cut from exactly
        // this, so a row whose expectation is wrong fails here first.
        let (status, listing) = call(&router, "DescribeParameters", &filters).await;
        assert_eq!(
            (status, reported_names(&listing)),
            (200, owned(&expected)),
            "case {case:?}: expected the filtered listing {expected:?}, got {status} {listing}"
        );

        // Every page size the rule generalises over: the published bounds, sizes inside the
        // listing, and the match count exactly and one either side of it.
        let sizes: Vec<i64> = [
            MIN_PAGE_SIZE,
            2,
            3,
            expected.len() as i64,
            expected.len() as i64 + 1,
            MAX_PAGE_SIZE,
        ]
        .into_iter()
        .filter(|size| (MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(size))
        .collect();

        for page_size in sizes {
            let case = format!("{case} at page size {page_size}");
            let mut walked: Vec<String> = Vec::new();
            let mut token: Option<String> = None;
            // One request per page, plus one: a token that never advanced would loop.
            for page_number in 0..=expected.len() + 1 {
                let mut body = filters.clone();
                body["MaxResults"] = json!(page_size);
                if let Some(token) = &token {
                    body["NextToken"] = json!(token);
                }
                let (status, page) = call(&router, "DescribeParameters", &body).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: page {page_number} must be served, got {status} {page}"
                );
                let names = reported_names(&page);
                assert!(
                    names.len() as i64 <= page_size,
                    "case {case:?}: page {page_number} carries at most {page_size} entries, \
                     got {names:?}"
                );
                // The cut this page continues after is the last entry the previous one
                // served, so this page's first entry sorts strictly after it.
                if let (Some(last), Some(first)) = (walked.last(), names.first()) {
                    assert!(
                        canonical(first) > canonical(last),
                        "case {case:?}: page {page_number} must continue strictly after \
                         {last:?}, got {names:?}"
                    );
                }
                // Each reported entry carries the metadata its put persisted, so a page is
                // the described parameters and not a list of names.
                for name in &names {
                    assert_metadata(&case, &page, name);
                }
                walked.extend(names);
                match page["NextToken"].as_str() {
                    Some(next) => token = Some(next.to_string()),
                    None => break,
                }
            }
            assert_eq!(
                walked,
                owned(&expected),
                "case {case:?}: paging must walk the filtered listing in order, got {walked:?}"
            );
        }
    }
}

/// A resume continues strictly after the last entry the previous page served, so a record
/// the store gains at a position after that cut is still reported by the walk.
#[tokio::test]
async fn a_resume_continues_after_the_last_entry_the_previous_page_served() {
    let router = router();
    for leaf in ["a", "b", "d", "e"] {
        put(&router, &json!({ "Name": format!("/Cut/{leaf}"), "Type": "String", "Value": "v" }))
            .await;
    }
    put(&router, &json!({ "Name": "/Cut/history", "Type": "String", "Value": "v1" })).await;
    for value in ["v2", "v3"] {
        put(
            &router,
            &json!({ "Name": "/Cut/history", "Type": "String", "Value": value, "Overwrite": true }),
        )
        .await;
    }

    // (case, the operation, the request for its first page, the first page's entries, what
    // to write between the two pages, the second page's entries)
    //
    // The write between the pages lands at a position AFTER the first page's last entry
    // and BEFORE the entry that was waiting behind it, which is exactly the record a
    // resume fixed on the waiting entry would step over.
    let cases: Vec<Resume> = vec![
        Resume {
            case: "the metadata describe",
            operation: "DescribeParameters",
            first: json!({
                "MaxResults": 2,
                "ParameterFilters": [{ "Key": "Path", "Option": "Recursive", "Values": ["/Cut"] }],
            }),
            first_names: vec!["/Cut/a", "/Cut/b"],
            written: json!({ "Name": "/Cut/c", "Type": "String", "Value": "v" }),
            second_names: vec!["/Cut/c", "/Cut/d"],
        },
        Resume {
            case: "the hierarchy read",
            operation: "GetParametersByPath",
            first: json!({ "Path": "/Cut", "Recursive": true, "MaxResults": 2 }),
            first_names: vec!["/Cut/a", "/Cut/b"],
            written: json!({ "Name": "/Cut/bb", "Type": "String", "Value": "v" }),
            second_names: vec!["/Cut/bb", "/Cut/c"],
        },
    ];

    for Resume {
        case,
        operation,
        first,
        first_names,
        written,
        second_names,
    } in cases
    {
        let (status, page) = call(&router, operation, &first).await;
        assert_eq!(
            (status, reported_names(&page)),
            (200, owned(&first_names)),
            "case {case:?}: expected the first page {first_names:?}, got {status} {page}"
        );
        let token = page["NextToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a token, got {page}"))
            .to_string();

        put(&router, &written).await;

        let mut body = first.clone();
        body["NextToken"] = json!(token);
        let (status, page) = call(&router, operation, &body).await;
        assert_eq!(
            (status, reported_names(&page)),
            (200, owned(&second_names)),
            "case {case:?}: the resume must continue after {:?} and report what the store \
             gained after it, got {status} {page}",
            first_names.last()
        );
    }

    // The version history resumes the same way: after the newest version the page served,
    // so a version a put appends between the two calls is reported rather than displacing
    // the resume.
    let history = json!({ "Name": "/Cut/history", "MaxResults": 2 });
    let (status, page) = call(&router, "GetParameterHistory", &history).await;
    assert_eq!(
        (status, reported_versions(&page)),
        (200, vec![1, 2]),
        "expected the first two versions of the history, got {status} {page}"
    );
    let token = page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a token for the history, got {page}"))
        .to_string();
    put(
        &router,
        &json!({ "Name": "/Cut/history", "Type": "String", "Value": "v4", "Overwrite": true }),
    )
    .await;
    let mut body = history.clone();
    body["NextToken"] = json!(token);
    let (status, page) = call(&router, "GetParameterHistory", &body).await;
    assert_eq!(
        (status, reported_versions(&page)),
        (200, vec![3, 4]),
        "the history must resume after version 2, got {status} {page}"
    );
}

/// The `Name` of every entry a page reported, in the order it reported them.
fn reported_names(page: &Value) -> Vec<String> {
    page["Parameters"]
        .as_array()
        .unwrap_or_else(|| panic!("a served page carries a Parameters list, got {page}"))
        .iter()
        .map(|entry| {
            entry["Name"]
                .as_str()
                .unwrap_or_else(|| panic!("every entry carries a Name, got {entry}"))
                .to_string()
        })
        .collect()
}

/// The `Version` of every entry a history page reported, in order.
fn reported_versions(page: &Value) -> Vec<i64> {
    page["Parameters"]
        .as_array()
        .unwrap_or_else(|| panic!("a served page carries a Parameters list, got {page}"))
        .iter()
        .map(|entry| {
            entry["Version"]
                .as_i64()
                .unwrap_or_else(|| panic!("every history entry carries a Version, got {entry}"))
        })
        .collect()
}

/// The entry of `page` reported under `name`, with the metadata the put that wrote it
/// persisted: the members a metadata listing reports beside the name.
fn assert_metadata(case: &str, page: &Value, name: &str) {
    let seeded = SEEDED
        .iter()
        .find(|seeded| seeded.name == name)
        .unwrap_or_else(|| panic!("case {case:?}: no parameter named {name} was seeded"));
    let entry = page["Parameters"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["Name"].as_str() == Some(name))
        })
        .unwrap_or_else(|| panic!("case {case:?}: {name} must be on the page, got {page}"));
    assert_eq!(
        (
            entry["ARN"].as_str(),
            entry["Type"].as_str(),
            entry["Tier"].as_str(),
            entry["DataType"].as_str(),
            entry["Version"].as_i64(),
            entry["Policies"].as_array().map(Vec::len),
        ),
        (
            Some(
                format!(
                    "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}",
                    canonical(name)
                )
                .as_str()
            ),
            Some(seeded.type_),
            Some(seeded.tier),
            Some("text"),
            Some(1),
            Some(0),
        ),
        "case {case:?}: expected the metadata persisted for {name}, got {entry}"
    );
}

/// A name as the store keys it: without the leading separator, which is not part of a
/// parameter's identity, so two spellings of one top-level name sort as one name.
fn canonical(name: &str) -> &str {
    name.strip_prefix('/').unwrap_or(name)
}

/// `expected` as owned strings, to compare against what a page reported.
fn owned(expected: &[&str]) -> Vec<String> {
    expected.iter().map(|name| (*name).to_string()).collect()
}

/// Write the parameters of [`SEEDED`], in the order it holds them.
async fn seed(router: &MultiServiceRouter) {
    for seeded in SEEDED {
        let mut body = json!({
            "Name": seeded.name,
            "Type": seeded.type_,
            "Value": "seeded",
            "Tier": seeded.tier,
        });
        if let Some((key, value)) = seeded.tag {
            body["Tags"] = json!([{ "Key": key, "Value": value }]);
        }
        put(router, &body).await;
    }
}

/// One `PutParameter` through the front door.
async fn put(router: &MultiServiceRouter, body: &Value) {
    let (status, written) = call(router, "PutParameter", body).await;
    assert_eq!(
        status, 200,
        "expected {body} to be written, got {status} {written}"
    );
}
