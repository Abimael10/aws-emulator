// SPDX-License-Identifier: CC0-1.0

//! The METADATA read of Parameter Store over AWS's own published hierarchy, end to end over
//! the front door.
//!
//! RULE under test: the `/aws` hierarchy holds the parameters AWS itself publishes, which no
//! account can write and no store of this service holds, and it is reached by ADDRESSING it —
//! so `DescribeParameters` whose filters address that hierarchy describes the published
//! catalog beside the account's own parameters, reporting there exactly what a hierarchy read
//! of the same place reports, and a request not addressed at it describes only what this
//! account wrote.
//!
//! `all/published_parameter_metadata.rs` states that rule over the named namespaces and the
//! projection a published entry is reported through; this suite states what that one does
//! not: the whole catalog as the emulator publishes it, each namespace DISCOVERED through the
//! hierarchy read itself rather than written here, the remaining filter keys narrowing a
//! published entry, a level inside a namespace that holds nothing, the members a published
//! entry carries no value for, and each page size the model's `MaxResults` admits.
//!
//! Every case goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol"). Nothing reaches into a state struct.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::parameter_store::call;
use super::public_parameter_catalog::{
    emulator, listing, names, recursive_listing, AMAZON_LINUX_NAMESPACE, PUBLISHED_DATA_TYPE,
    PUBLISHED_TYPE,
};

/// The hierarchy AWS publishes its own parameters in, as a client writes it — the one
/// `PutParameter` refuses to write into.
const PUBLISHED_ROOT: &str = "/aws";

/// The level below [`PUBLISHED_ROOT`] the published namespaces of AWS's services live in.
const PUBLISHED_SERVICE_ROOT: &str = "/aws/service";

/// A parameter of the ACCOUNT, written before each read so the account's own half of every
/// answer is non-empty: a read that names AWS's hierarchy must not report it, and a read
/// that names nothing there must report it.
const ACCOUNT_NAME: &str = "/App/db/url";

/// Its value. Nothing asserts on it; it is here because `PutParameter` requires one.
const ACCOUNT_VALUE: &str = "postgres://db.internal:5432/app";

/// The `ParameterTier` a published parameter carries, from the modelled enum
/// (`smithy_query(query="shape", shape="ParameterTier")`): AWS publishes it in the tier every
/// account reads for free.
const PUBLISHED_TIER: &str = "Standard";

/// The published bounds of `DescribeParameters`'s `MaxResults`
/// (`smithy_query(query="operation", operation="DescribeParameters")`: range 1..50).
const MIN_PAGE_SIZE: i64 = 1;

/// See [`MIN_PAGE_SIZE`].
const MAX_PAGE_SIZE: i64 = 50;

/// How many one-level namespaces the reach suite walks. Every parent of a published name is
/// one; a spread of this many is taken so the suite spans the catalog's depth without
/// re-reading every level of it.
const ONE_LEVEL_ROWS: usize = 6;

/// Write [`ACCOUNT_NAME`] into the account's own hierarchy.
async fn put_account_parameter(router: &MultiServiceRouter, case: &str) {
    let (status, response) = call(
        router,
        "PutParameter",
        &json!({ "Name": ACCOUNT_NAME, "Value": ACCOUNT_VALUE, "Type": "String" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: PutParameter {ACCOUNT_NAME} must succeed, got {status} with {response}"
    );
}

/// Every parameter `DescribeParameters` reports for `body`, paging until the read hands out
/// no further token, the way a client's paginator walks the listing.
async fn described(router: &MultiServiceRouter, case: &str, body: &Value) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut page = body.clone();
        if let Some(resume) = &token {
            page["NextToken"] = json!(resume);
        }
        let (status, response) = call(router, "DescribeParameters", &page).await;
        assert_eq!(
            status, 200,
            "case {case}: DescribeParameters {page} must succeed, got {status} with {response}"
        );
        collected.extend(
            response["Parameters"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
        match response["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
        assert!(
            collected.len() < 5_000,
            "case {case}: DescribeParameters kept handing out tokens past {} parameters",
            collected.len()
        );
    }
    collected
}

/// The `Name` of each reported parameter, ascending — the set a row compares, independent of
/// the order one page boundary fell at.
fn sorted_names(parameters: &[Value]) -> Vec<String> {
    let mut sorted = names(parameters);
    sorted.sort();
    sorted
}

/// A `DescribeParameters` body carrying `filters` and nothing else.
fn with_filters(filters: Value) -> Value {
    json!({ "ParameterFilters": filters })
}

/// A `Path` filter over `path`, reaching the whole hierarchy under it or only the level
/// immediately below it ("For filters using the key `Path`, valid options include
/// `Recursive` and `OneLevel`", the `ParameterStringFilter$Option` doc).
fn path_filter(path: &str, recursive: bool) -> Value {
    json!([{
        "Key": "Path",
        "Option": if recursive { "Recursive" } else { "OneLevel" },
        "Values": [path],
    }])
}

/// The hierarchy one level above a published name (`/a/b/c` -> `/a/b`).
fn parent_of(name: &str) -> String {
    let (parent, _leaf) = name
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("a published name is hierarchical: {name}"));
    parent.to_string()
}

/// The namespace `levels` levels deep that `name` lies in, or `None` when the name is not
/// that deep (`/aws/service/x/y` at 3 levels -> `/aws/service/x`).
fn namespace_of(name: &str, levels: usize) -> Option<String> {
    let segments: Vec<&str> = name.trim_start_matches('/').split('/').collect();
    (segments.len() > levels).then(|| format!("/{}", segments[..levels].join("/")))
}

/// `values` deduplicated, keeping the first occurrence of each — the catalog's own ascending
/// order, so the rows a suite walks are the same on every run.
fn distinct(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in values {
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    seen
}

/// Up to `wanted` of `values`, spread evenly across them, so a sample of a long list reaches
/// its far end as well as its near one.
fn spread(values: &[String], wanted: usize) -> Vec<String> {
    if values.len() <= wanted || wanted == 0 {
        return values.to_vec();
    }
    (0..wanted)
        .map(|at| values[at * values.len() / wanted].clone())
        .collect()
}

/// RULE: a metadata read whose `Path` filter names a place in AWS's published hierarchy
/// describes exactly the parameters a hierarchy read of that place reports — at both reaches
/// of a path — and reports none of the account's own parameters, which lie outside it.
#[tokio::test]
async fn a_path_filter_naming_the_published_hierarchy_describes_what_the_hierarchy_read_serves() {
    let router = emulator();
    put_account_parameter(&router, "the account's own parameter").await;

    let catalog = names(&recursive_listing(&router, "the whole published catalog", PUBLISHED_ROOT).await);
    assert!(
        catalog.len() >= 5,
        "the emulator must publish a catalog to read: got {catalog:?}"
    );

    // Recursive rows: the published root, the service level, and every namespace the catalog
    // itself holds names in three levels down — discovered, so a namespace the emulator
    // gains is walked without a row being written for it.
    let namespaces = distinct(catalog.iter().filter_map(|name| namespace_of(name, 3)));
    let recursive_rows: Vec<String> = [PUBLISHED_ROOT.to_string(), PUBLISHED_SERVICE_ROOT.to_string()]
        .into_iter()
        .chain(namespaces)
        .collect();

    // One-level rows: a parent of a published name is the one place a `OneLevel` reach
    // reports something, so the sample is taken from those.
    let parents = distinct(catalog.iter().map(|name| parent_of(name)));
    let one_level_rows = spread(&parents, ONE_LEVEL_ROWS);

    let rows: Vec<(String, bool)> = recursive_rows
        .into_iter()
        .map(|path| (path, true))
        .chain(one_level_rows.into_iter().map(|path| (path, false)))
        .collect();

    for (path, recursive) in &rows {
        let reach = if *recursive { "Recursive" } else { "OneLevel" };
        let case = format!("DescribeParameters Path={path} Option={reach}");
        let expected = sorted_names(&listing(&router, &case, path, *recursive).await);
        let actual = sorted_names(&described(&router, &case, &with_filters(path_filter(path, *recursive))).await);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the metadata read to describe the parameters the \
             hierarchy read serves ({expected:?}), got {actual:?}"
        );
        assert!(
            !expected.is_empty(),
            "case {case:?}: the row must name a place the catalog publishes under, got nothing"
        );
        assert!(
            !actual.iter().any(|name| name == ACCOUNT_NAME),
            "case {case:?}: the account's own parameter lies outside AWS's hierarchy and must \
             not be described there, got {actual:?}"
        );
    }
}

/// RULE: a published parameter is described through the same metadata the store's own
/// parameters are described through, so every other filter of the read narrows it too: it
/// carries the attributes AWS publishes it with, and a filter asking for another value of one
/// of those attributes describes none of them.
#[tokio::test]
async fn the_remaining_filters_narrow_the_published_catalog_as_well() {
    let router = emulator();
    let published = sorted_names(
        &recursive_listing(&router, "the Amazon Linux AMI aliases", AMAZON_LINUX_NAMESPACE).await,
    );
    assert!(
        !published.is_empty(),
        "the namespace under test must publish names: got {published:?}"
    );

    // One row per attribute a published parameter carries, each stated twice: the value AWS
    // publishes it with, and another value of the same attribute. The `Type` and `Tier`
    // values come from the modelled enums (`smithy_query(query="shape",
    // shape="ParameterType,ParameterTier")`), the `DataType` values from
    // `smithy_query(query="doc", shape="PutParameterRequest", member="DataType")`.
    let rows: &[(&str, &str, bool)] = &[
        ("Type", PUBLISHED_TYPE, true),
        ("Type", "StringList", false),
        ("Type", "SecureString", false),
        ("Tier", PUBLISHED_TIER, true),
        ("Tier", "Advanced", false),
        ("Tier", "Intelligent-Tiering", false),
        ("DataType", PUBLISHED_DATA_TYPE, true),
        ("DataType", "aws:ec2:image", false),
    ];

    for (key, value, matches) in rows {
        let case = format!("Path={AMAZON_LINUX_NAMESPACE} Recursive plus {key}={value}");
        let mut filters = path_filter(AMAZON_LINUX_NAMESPACE, true);
        filters
            .as_array_mut()
            .expect("the filter list is an array")
            .push(json!({ "Key": key, "Option": "Equals", "Values": [value] }));
        let actual = sorted_names(&described(&router, &case, &with_filters(filters)).await);
        let expected: Vec<String> = if *matches {
            published.clone()
        } else {
            Vec::new()
        };
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {} described, got {actual:?}",
            if *matches { "every published alias" } else { "nothing" }
        );
    }
}

/// RULE: a described published parameter carries no last-modifying user, because no user of
/// this account wrote it, and no value, because a metadata read reports none — while the
/// instant it was published IS reported, as the instant that name last changed.
///
/// What a published entry's other members carry (its type, tier, data type, version and
/// account-less ARN) is `published_projection`'s in `all/published_parameter_metadata.rs`;
/// only the three members that suite does not state are checked here.
#[tokio::test]
async fn a_described_published_parameter_names_no_writer_and_carries_no_value() {
    let router = emulator();
    let case = "the metadata of the published catalog";
    let reported = described(
        &router,
        case,
        &with_filters(path_filter(PUBLISHED_ROOT, true)),
    )
    .await;
    assert!(
        reported.len() >= 5,
        "case {case:?}: the published catalog must be described to be checked, got {reported:?}"
    );

    for parameter in &reported {
        let name = parameter["Name"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: every described parameter carries a Name: {parameter}")
        });
        assert!(
            parameter["LastModifiedDate"].is_number(),
            "case {name:?}: expected the instant it was published, got {parameter}"
        );
        assert!(
            parameter.get("LastModifiedUser").is_none_or(Value::is_null),
            "case {name:?}: no user of this account wrote a published parameter, got {parameter}"
        );
        assert!(
            parameter.get("Value").is_none_or(Value::is_null),
            "case {name:?}: a metadata read reports no value, got {parameter}"
        );
    }
}

/// RULE: a level INSIDE a namespace AWS publishes in exists and may hold nothing there, so
/// both reads of Parameter Store report the nothing it holds rather than refusing it — and
/// they report the same nothing, because the two reads resolve the published catalog through
/// the one projection of it.
#[tokio::test]
async fn a_level_inside_a_published_namespace_that_holds_nothing_is_read_as_empty() {
    let router = emulator();
    put_account_parameter(&router, "the account's own parameter").await;

    let aliases = names(
        &recursive_listing(&router, "the Amazon Linux AMI aliases", AMAZON_LINUX_NAMESPACE).await,
    );
    assert!(
        !aliases.is_empty(),
        "the namespace under test must publish names: got {aliases:?}"
    );

    // Levels inside a namespace AWS publishes in, at the depths a caller reaches them at: a
    // sibling of a published name, and a level below a published name itself.
    let rows = [
        (
            "a leaf of the namespace AWS publishes nothing under",
            format!("{AMAZON_LINUX_NAMESPACE}/no-such-published-alias"),
        ),
        (
            "a level below a published name",
            format!("{}/deeper", aliases[0]),
        ),
    ];

    for (case, path) in &rows {
        let served = listing(&router, case, path, true).await;
        assert!(
            served.is_empty(),
            "case {case:?} ({path}): the hierarchy read reports the nothing the namespace \
             holds there, got {served:?}"
        );
        let reported = described(&router, case, &with_filters(path_filter(path, true))).await;
        assert!(
            reported.is_empty(),
            "case {case:?} ({path}): the metadata read reports the same nothing, got \
             {reported:?}"
        );
    }
}

/// RULE: the published catalog pages through the metadata read exactly as the store's own
/// parameters do, so a caller walking it at any page size the model admits sees the same set
/// it sees unpaged, and never the account's own parameter, which lies outside the hierarchy
/// the read named.
#[tokio::test]
async fn the_published_catalog_pages_at_every_size_the_model_admits() {
    let router = emulator();
    put_account_parameter(&router, "the account's own parameter").await;

    let body = with_filters(path_filter(PUBLISHED_ROOT, true));
    let unpaged = sorted_names(&described(&router, "unpaged", &body).await);
    assert!(
        unpaged.len() > MIN_PAGE_SIZE as usize,
        "the catalog must be longer than one page to page it: got {unpaged:?}"
    );
    assert!(
        !unpaged.iter().any(|name| name == ACCOUNT_NAME),
        "the account's own parameter lies outside AWS's hierarchy: got {unpaged:?}"
    );

    for size in [MIN_PAGE_SIZE, 2, 7, MAX_PAGE_SIZE] {
        let case = format!("MaxResults={size} over {PUBLISHED_ROOT}");
        let mut paged_body = body.clone();
        paged_body["MaxResults"] = json!(size);
        let paged = sorted_names(&described(&router, &case, &paged_body).await);
        assert_eq!(
            paged, unpaged,
            "case {case:?}: expected the paged walk to describe {unpaged:?}, got {paged:?}"
        );
    }
}
