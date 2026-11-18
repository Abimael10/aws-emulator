// SPDX-License-Identifier: CC0-1.0

//! A DynamoDB table tagged through DynamoDB's OWN tag plane is a row of the Resource
//! Groups Tagging index.
//!
//! The rule under test: **a tag the service that OWNS a resource holds for it — here a
//! table's tag set, seeded by `CreateTable$Tags` or written by DynamoDB's own
//! `TagResource` — is what the tagging index reports for that resource in the Region the
//! resource lives in, so `GetResources` selects the table by its `TagFilters` (AND-ed
//! across filters, each filter's `Values` OR-ed, a filter with no values matching the key
//! with any value), optionally narrowed by `ResourceTypeFilters` in the
//! `service[:resourceType]` spelling the table's ARN carries, and `GetTagKeys` /
//! `GetTagValues` summarise those same tags.** A table carrying no tag is in no answer
//! ("GetResources does not return untagged resources", that operation's documentation),
//! and a table of another Region is in no answer here ("Returns all the tagged or
//! previously tagged resources that are located in the specified Amazon Web Services
//! Region for the account").
//!
//! Two crates have to agree for that to hold — DynamoDB owns and stores the table's tag
//! set, the tagging API projects it — so the suite lives here, on the assembled router
//! with both services mounted on ONE engine, which is how the emulator runs.
//!
//! The tagging crate's own `tests/all/dynamodb_tagged_resource_index.rs` reads that
//! service as an owner of this API from the other side — a write and an untag through
//! each of the two tag planes, an unresolvable ARN, a deleted table. What this suite adds
//! is the SELECTION the index answers with once those writes have landed: every
//! `TagFilters` form over one estate, the Region the answer is scoped to, and the two
//! summary operations that read the same owner-held tags.
//!
//! The rows span what the rule generalises over: how the tag got onto the table (a
//! create's `Tags`, a later `TagResource`, a `TagResource` overwriting a key the create
//! carried), how many keys the table ends up with, an empty tag value, a table with no
//! tag at all, and a table in another Region; and the queries span the filter forms —
//! key only, key and value, several values, several filters, the bare and the qualified
//! resource type, a type no table answers to, `ResourceARNList`, and no filter at all.
//! The single-key case the issue cites is one row.
//!
//! A DynamoDB tag change is ASYNCHRONOUS ("The application or removal of tags using
//! TagResource and UntagResource APIs is eventually consistent", `TagResource`'s
//! documentation), so the suite waits for the CONDITION — the whole estate reporting the
//! tag sets the writes left — in a poll loop, and never for a fixed time.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810 as owner_dynamodb;
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging_api;

/// The Region the estate is created in, and the Region the index is queried in.
const REGION: &str = "us-east-1";

/// A second Region, to pin the Region scope of the answer: a table here is reported by
/// this Region's endpoint and by no other.
const OTHER_REGION: &str = "us-west-2";

/// The account segment every ARN this emulator mints carries.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The SigV4 signing names the router routes a request to a service by.
const TAGGING: &str = "tagging";
const DYNAMODB: &str = "dynamodb";

/// The `service:resourceType` key a table's ARN carries, and the bare service token that
/// selects every resource of that service.
const TABLE_TYPE: &str = "dynamodb:table";
const BARE_TYPE: &str = "dynamodb";

/// How long a poll loop keeps looking before it gives up. Generous on purpose: it bounds
/// a condition owed one short propagation span, so a loaded or slow host still settles
/// well inside it.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks — short relative to the span it waits out, so
/// the loop observes the change rather than the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The router carrying DynamoDB and the tagging API over ONE engine, the arrangement
/// `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises handler behaviour, not SigV4: the `Authorization` header below
    // is present because its credential scope is what routes a request to a signing name
    // and names the Region, not to be verified.
    router.set_request_authentication(false);
    router.register_service(owner_dynamodb::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(tagging_api::EmulatedService::new(engine));
    router
}

/// Call one operation of one service, signed for `region`, and return its HTTP status and
/// parsed body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=not-verified"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{target} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, parsed)
}

/// One tagging-API operation in `region`, which must answer 200.
async fn tagging_ok(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, parsed) = call(
        router,
        region,
        TAGGING,
        "application/x-amz-json-1.1",
        &format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} in {region} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// One DynamoDB operation in `region`, which must answer 200.
async fn ddb_ok(router: &MultiServiceRouter, region: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = call(
        router,
        region,
        DYNAMODB,
        "application/x-amz-json-1.0",
        &format!("DynamoDB_20120810.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} in {region} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// `(Key, Value)` records as the modelled DynamoDB `TagList` carries them.
fn tag_list(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect(),
    )
}

/// A declared tag set as the map the assertions compare against.
fn tag_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// Create a table in `region` — `PAY_PER_REQUEST`, one `HASH` key — carrying `tags`, and
/// return the ARN its own `TableDescription` reports, which is the ARN this API has to
/// address it by.
async fn create_table(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut request = json!({
        "TableName": name,
        "KeySchema": [{ "AttributeName": "Id", "KeyType": "HASH" }],
        "AttributeDefinitions": [{ "AttributeName": "Id", "AttributeType": "S" }],
        "BillingMode": "PAY_PER_REQUEST",
    });
    if !tags.is_empty() {
        request["Tags"] = tag_list(tags);
    }
    let created = ddb_ok(router, region, "CreateTable", request).await;
    let minted = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("a CreateTable body with no TableArn: {created}"))
        .to_string();
    let expected = format!("arn:aws:dynamodb:{region}:{ACCOUNT_ID}:table/{name}");
    assert_eq!(
        minted, expected,
        "table {name:?}: expected CreateTable in {region} to mint {expected}, got {minted}"
    );
    minted
}

/// The `(ARN -> tags)` map one `GetResources` page reports.
fn page_estate(body: &Value) -> BTreeMap<String, BTreeMap<String, String>> {
    body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("a GetResources body with no ResourceTagMappingList: {body}"))
        .iter()
        .map(|mapping| {
            let arn = mapping["ResourceARN"]
                .as_str()
                .unwrap_or_else(|| panic!("a mapping with no ResourceARN: {mapping}"))
                .to_string();
            let tags = mapping["Tags"]
                .as_array()
                .unwrap_or_else(|| panic!("a mapping with no Tags: {mapping}"))
                .iter()
                .map(|tag| {
                    (
                        tag["Key"]
                            .as_str()
                            .unwrap_or_else(|| panic!("a tag with no Key: {tag}"))
                            .to_string(),
                        tag["Value"]
                            .as_str()
                            .unwrap_or_else(|| panic!("a tag with no Value: {tag}"))
                            .to_string(),
                    )
                })
                .collect();
            (arn, tags)
        })
        .collect()
}

/// The ARNs one `GetResources` request selects, sorted, with the answer for a message.
async fn selected(
    router: &MultiServiceRouter,
    region: &str,
    request: Value,
) -> (Vec<String>, Value) {
    let body = tagging_ok(router, region, "GetResources", request).await;
    let mut arns: Vec<String> = page_estate(&body).into_keys().collect();
    arns.sort();
    (arns, body)
}

/// Every string of a list member of an answer, sorted.
fn string_list(body: &Value, member: &str) -> Vec<String> {
    let mut values: Vec<String> = body[member]
        .as_array()
        .unwrap_or_else(|| panic!("an answer with no {member}: {body}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("a non-string entry of {member}: {body}"))
                .to_string()
        })
        .collect();
    values.sort();
    values
}

/// Look again until `settled` answers `Ok`, or [`POLL_DEADLINE`] passes.
///
/// A DynamoDB tag change is eventually consistent, so the condition is owed rather than
/// immediate: the loop waits for the CONDITION and reports the last answer it saw when it
/// runs out of patience, which is what names the failure.
async fn poll_until<T, F, Fut>(case: &str, what: &str, mut settled: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let started = Instant::now();
    loop {
        let last = match settled().await {
            Ok(value) => return value,
            Err(seen) => seen,
        };
        if started.elapsed() >= POLL_DEADLINE {
            panic!("case {case:?}: {what} never settled; the last answer was {last}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One table of the estate: how its tags got onto it, and the set it carries once both
/// writes have landed.
struct Row {
    case: &'static str,
    table: &'static str,
    /// The `Tags` its `CreateTable` declares.
    seeded: &'static [(&'static str, &'static str)],
    /// The pairs a later DynamoDB `TagResource` writes.
    written: &'static [(&'static str, &'static str)],
    /// The set the table carries afterwards, computed by the rule the `Tag$Key`
    /// documentation states ("If you try to add an existing tag (same key), the existing
    /// tag value will be updated to the new value"), not read back out of the answer.
    settled: &'static [(&'static str, &'static str)],
}

/// The estate of [`REGION`]: a create's single tag (the case the issue cites), a create
/// carrying two keys, a tag written only by `TagResource`, a `TagResource` that overwrites
/// a key the create carried and leaves another one an empty value (`TagValueString` is
/// `len 0..256`, so an empty value is a value and not an absence), and a table nothing
/// ever tagged.
const ROWS: &[Row] = &[
    Row {
        case: "one tag a create seeded",
        table: "native-create-one",
        seeded: &[("Project", "phoenix")],
        written: &[],
        settled: &[("Project", "phoenix")],
    },
    Row {
        case: "two tags a create seeded",
        table: "native-create-two",
        seeded: &[("Project", "phoenix"), ("Environment", "prod")],
        written: &[],
        settled: &[("Environment", "prod"), ("Project", "phoenix")],
    },
    Row {
        case: "a tag written onto a table created untagged",
        table: "native-tagged-later",
        seeded: &[],
        written: &[("Project", "atlas")],
        settled: &[("Project", "atlas")],
    },
    Row {
        case: "a write that overwrites a seeded key and empties another",
        table: "Native.Table_4",
        seeded: &[("Project", "phoenix"), ("Owner", "platform")],
        written: &[("Project", "atlas"), ("Owner", "")],
        settled: &[("Owner", ""), ("Project", "atlas")],
    },
    Row {
        case: "a table nothing ever tagged",
        table: "native-untagged",
        seeded: &[],
        written: &[],
        settled: &[],
    },
];

/// The tag set the table of [`OTHER_REGION`] carries, keyed the same way as the estate of
/// [`REGION`] so that a Region leak would show up as a selection.
const OTHER_REGION_TAGS: &[(&str, &str)] = &[("Project", "phoenix")];
const OTHER_REGION_TABLE: &str = "native-other-region";

/// The estate once every write has landed: the ARN of each row by table name, and the
/// `(ARN -> tags)` map the index has to report for the Region, which is every row that
/// ends up carrying at least one tag.
struct Estate {
    arns: BTreeMap<&'static str, String>,
    tagged: BTreeMap<String, BTreeMap<String, String>>,
}

/// Create the estate of [`REGION`] and the one table of [`OTHER_REGION`], apply each
/// row's `TagResource`, and wait until the index reports exactly the tag sets the writes
/// left in each Region.
async fn settled_estate(router: &MultiServiceRouter) -> (Estate, String) {
    let mut arns = BTreeMap::new();
    let mut tagged = BTreeMap::new();
    for row in ROWS {
        let arn = create_table(router, REGION, row.table, row.seeded).await;
        if !row.written.is_empty() {
            ddb_ok(
                router,
                REGION,
                "TagResource",
                json!({ "ResourceArn": &arn, "Tags": tag_list(row.written) }),
            )
            .await;
        }
        if !row.settled.is_empty() {
            tagged.insert(arn.clone(), tag_map(row.settled));
        }
        arns.insert(row.table, arn);
    }
    let elsewhere = create_table(
        router,
        OTHER_REGION,
        OTHER_REGION_TABLE,
        OTHER_REGION_TAGS,
    )
    .await;

    let want = tagged.clone();
    poll_until(
        "the whole estate",
        "the index entries of the tables this Region holds",
        || async {
            let body = tagging_ok(router, REGION, "GetResources", json!({})).await;
            let reported = page_estate(&body);
            if reported == want {
                Ok(())
            } else {
                Err(format!("{reported:?}, expected {want:?}"))
            }
        },
    )
    .await;

    let want_elsewhere: BTreeMap<String, BTreeMap<String, String>> =
        [(elsewhere.clone(), tag_map(OTHER_REGION_TAGS))]
            .into_iter()
            .collect();
    poll_until(
        "the table of the other Region",
        "the index entry of that Region's own endpoint",
        || async {
            let body = tagging_ok(router, OTHER_REGION, "GetResources", json!({})).await;
            let reported = page_estate(&body);
            if reported == want_elsewhere {
                Ok(())
            } else {
                Err(format!("{reported:?}, expected {want_elsewhere:?}"))
            }
        },
    )
    .await;

    (Estate { arns, tagged }, elsewhere)
}

/// Every filter form, over the settled estate: `GetResources` selects the tables whose
/// OWN tag set — the one DynamoDB holds — satisfies the request, and reports that set as
/// the entry's `Tags`.
#[tokio::test]
async fn get_resources_selects_tables_by_the_tags_dynamodb_holds_for_them() {
    let router = emulator();
    let (estate, elsewhere) = settled_estate(&router).await;
    let arn = |table: &str| estate.arns[table].clone();

    // The tag set each selected entry reports is the table's own, pair for pair: the
    // issue's row is the first one, and the entry it asks for carries the key and value
    // that was set.
    let body = tagging_ok(&router, REGION, "GetResources", json!({})).await;
    let reported = page_estate(&body);
    for row in ROWS {
        let case = row.case;
        let table_arn = arn(row.table);
        match reported.get(&table_arn) {
            Some(tags) => {
                let want = tag_map(row.settled);
                assert_eq!(
                    *tags, want,
                    "case {case:?}: expected the index entry for {table_arn} to carry exactly \
                     {want:?}, got {tags:?}"
                );
            }
            None => assert!(
                row.settled.is_empty(),
                "case {case:?}: expected {table_arn} to be reported, the page holds \
                 {reported:?}"
            ),
        }
    }

    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "one key and value, narrowed by the qualified resource type",
            json!({
                "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }],
                "ResourceTypeFilters": [TABLE_TYPE],
            }),
            vec![arn("native-create-one"), arn("native-create-two")],
        ),
        (
            "the same key and value with no type narrowing",
            json!({ "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }] }),
            vec![arn("native-create-one"), arn("native-create-two")],
        ),
        (
            "a filter with no values matches the key with any value",
            json!({ "TagFilters": [{ "Key": "Project" }] }),
            vec![
                arn("Native.Table_4"),
                arn("native-create-one"),
                arn("native-create-two"),
                arn("native-tagged-later"),
            ],
        ),
        (
            "several values of one filter are OR-ed",
            json!({ "TagFilters": [{ "Key": "Project", "Values": ["atlas", "phoenix"] }] }),
            vec![
                arn("Native.Table_4"),
                arn("native-create-one"),
                arn("native-create-two"),
                arn("native-tagged-later"),
            ],
        ),
        (
            "two filters are AND-ed",
            json!({
                "TagFilters": [
                    { "Key": "Project", "Values": ["phoenix"] },
                    { "Key": "Environment", "Values": ["prod"] },
                ],
            }),
            vec![arn("native-create-two")],
        ),
        (
            "an AND no table satisfies",
            json!({
                "TagFilters": [
                    { "Key": "Project", "Values": ["atlas"] },
                    { "Key": "Environment", "Values": ["prod"] },
                ],
            }),
            Vec::new(),
        ),
        (
            "an empty tag value is a value a filter selects by",
            json!({ "TagFilters": [{ "Key": "Owner", "Values": [""] }] }),
            vec![arn("Native.Table_4")],
        ),
        (
            "the bare service token selects every tagged table of the service",
            json!({
                "TagFilters": [{ "Key": "Project" }],
                "ResourceTypeFilters": [BARE_TYPE],
            }),
            vec![
                arn("Native.Table_4"),
                arn("native-create-one"),
                arn("native-create-two"),
                arn("native-tagged-later"),
            ],
        ),
        (
            "a qualified type no tagged resource answers to",
            json!({
                "TagFilters": [{ "Key": "Project" }],
                "ResourceTypeFilters": ["dynamodb:index"],
            }),
            Vec::new(),
        ),
        (
            "a value no table carries",
            json!({ "TagFilters": [{ "Key": "Project", "Values": ["no-such-project"] }] }),
            Vec::new(),
        ),
        (
            "no filter at all reports every tagged table of the Region",
            json!({}),
            estate.tagged.keys().cloned().collect(),
        ),
        // "If a resource specified by this parameter doesn't exist, it doesn't generate
        // an error; it simply isn't included in the response" (`ResourceARNList`'s own
        // documentation) and "GetResources does not return untagged resources" (the
        // operation's): naming the untagged table is neither an error nor an entry.
        (
            "ResourceARNList naming every table of the Region, tagged or not",
            json!({
                "ResourceARNList": ROWS
                    .iter()
                    .map(|row| arn(row.table))
                    .collect::<Vec<String>>(),
            }),
            estate.tagged.keys().cloned().collect(),
        ),
        (
            "ResourceARNList naming the table of another Region",
            json!({ "ResourceARNList": [elsewhere.clone()] }),
            Vec::new(),
        ),
    ];

    for (case, request, mut want) in cases {
        want.sort();
        let (got, body) = selected(&router, REGION, request.clone()).await;
        assert_eq!(
            got, want,
            "case {case:?}: expected {request} in {REGION} to select {want:?}, got {got:?} \
             from {body}"
        );
    }

    // The other Region's endpoint answers for its own table and for no table of
    // [`REGION`], under the same filter that selects two of them there.
    let case = "the Region scope of the answer";
    let request = json!({ "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }] });
    let (got, body) = selected(&router, OTHER_REGION, request.clone()).await;
    assert_eq!(
        got,
        vec![elsewhere.clone()],
        "case {case:?}: expected {request} in {OTHER_REGION} to select {elsewhere} alone, got \
         {got:?} from {body}"
    );
}

/// The index SUMMARISES the same owner-held tags: `GetTagKeys` reports every distinct key
/// the Region's tagged tables carry and `GetTagValues` every distinct value under one key
/// — each once, however many tables carry it — and neither reports a key or value that
/// only a table of another Region carries.
#[tokio::test]
async fn get_tag_keys_and_values_summarise_the_tags_dynamodb_holds() {
    let router = emulator();
    let (estate, _) = settled_estate(&router).await;

    // What the estate's own declarations say the summary is, computed from the rows
    // rather than read back out of the answer.
    let mut want_keys: Vec<String> = Vec::new();
    let mut want_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for tags in estate.tagged.values() {
        for (key, value) in tags {
            if !want_keys.contains(key) {
                want_keys.push(key.clone());
            }
            let values = want_values.entry(key.clone()).or_default();
            if !values.contains(value) {
                values.push(value.clone());
            }
        }
    }
    want_keys.sort();

    let case = "every key the Region's tagged tables carry";
    let body = tagging_ok(&router, REGION, "GetTagKeys", json!({})).await;
    let got = string_list(&body, "TagKeys");
    assert_eq!(
        got, want_keys,
        "case {case:?}: expected GetTagKeys in {REGION} to report {want_keys:?}, got {got:?}"
    );

    for (key, mut want) in want_values {
        want.sort();
        let body = tagging_ok(&router, REGION, "GetTagValues", json!({ "Key": &key })).await;
        let got = string_list(&body, "TagValues");
        assert_eq!(
            got, want,
            "case {key:?}: expected GetTagValues({key:?}) in {REGION} to report {want:?}, got \
             {got:?}"
        );
    }

    // A key no table of this Region carries is summarised as nothing, rather than as the
    // keys of some other Region's tables.
    let case = "a key nothing carries";
    let body = tagging_ok(
        &router,
        REGION,
        "GetTagValues",
        json!({ "Key": "no-such-key" }),
    )
    .await;
    let got = string_list(&body, "TagValues");
    assert!(
        got.is_empty(),
        "case {case:?}: expected GetTagValues of a key nothing carries to report no value, got \
         {got:?}"
    );

    // The other Region's endpoint summarises its own table's tags only: the one key it
    // carries, under the one value it carries there.
    let case = "the Region scope of the summary";
    let body = tagging_ok(&router, OTHER_REGION, "GetTagKeys", json!({})).await;
    let got = string_list(&body, "TagKeys");
    let want: Vec<String> = OTHER_REGION_TAGS
        .iter()
        .map(|(key, _)| (*key).to_string())
        .collect();
    assert_eq!(
        got, want,
        "case {case:?}: expected GetTagKeys in {OTHER_REGION} to report {want:?}, got {got:?}"
    );
    for (key, value) in OTHER_REGION_TAGS {
        let body = tagging_ok(&router, OTHER_REGION, "GetTagValues", json!({ "Key": key })).await;
        let got = string_list(&body, "TagValues");
        assert_eq!(
            got,
            vec![(*value).to_string()],
            "case {key:?}: expected GetTagValues({key:?}) in {OTHER_REGION} to report \
             {value:?} alone, got {got:?}"
        );
    }
}
