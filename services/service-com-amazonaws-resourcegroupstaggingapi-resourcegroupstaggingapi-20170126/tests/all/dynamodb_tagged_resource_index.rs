// SPDX-License-Identifier: CC0-1.0

//! Amazon DynamoDB as an OWNING service of this API: a table has ONE tag set, held by
//! DynamoDB, so a tag written through DynamoDB's own `TagResource` — or seeded by
//! `CreateTable$Tags` — is discoverable through `GetResources`, an `UntagResource`
//! withdraws the entry, a tag written through `TagResources` here is the tag that
//! service's own `ListTagsOfResource` reports, and a table deleted through DynamoDB
//! leaves this API's answers with no untag.
//!
//! Every test drives the real wire path — the multi-service router, `POST /` with an
//! `X-Amz-Target` and an awsJson body — over ONE engine carrying both services, which is
//! how the emulator runs.
//!
//! A DynamoDB tag change is ASYNCHRONOUS ("The application or removal of tags using
//! TagResource and UntagResource APIs is eventually consistent", that operation's doc),
//! so every read below is taken in a poll loop that looks again until the condition
//! holds: the tests wait for the CONDITION, never for a fixed time.
//!
//! The tables span what the rule generalises over — how the tags got onto the table (a
//! create's `Tags`, a later `TagResource`, a write through this API), how many keys a
//! write carries, a key the create already carried being overwritten, an empty tag
//! value, and a table carrying no tag at all — plus the ARN forms in this namespace that
//! name no taggable table. The single-key case the issue cites is one row.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_dynamodb_dynamodb_20120810 as owner_dynamodb;
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and therefore the one both endpoints
/// serve.
const REGION: &str = "us-east-1";

/// The account segment every emulated ARN carries.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The SigV4 signing names the router routes on.
const TAGGING: &str = "tagging";
const DYNAMODB: &str = "dynamodb";

/// How long a poll loop keeps looking before it gives up. Generous on purpose: it bounds
/// a condition that is owed one short propagation span, so a loaded or slow host still
/// settles well inside it.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks. Short relative to the span it is waiting
/// out, so the loop observes the change rather than the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A router carrying the tagging API and DynamoDB over ONE engine — the arrangement
/// `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The tests exercise handler behaviour, not SigV4: the `Authorization` header below
    // is present because it is what routes a request to a signing name, not to be
    // verified.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(owner_dynamodb::EmulatedService::new(engine));
    router
}

/// Call one operation of one service and return its HTTP status and parsed body.
async fn call(
    router: &MultiServiceRouter,
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
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("build the request");

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

/// One tagging-API operation.
async fn tagging(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    call(
        router,
        TAGGING,
        "application/x-amz-json-1.1",
        &format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
        body,
    )
    .await
}

/// One tagging-API operation that must answer 200.
async fn tagging_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = tagging(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// One DynamoDB operation.
async fn ddb(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    call(
        router,
        DYNAMODB,
        "application/x-amz-json-1.0",
        &format!("DynamoDB_20120810.{operation}"),
        body,
    )
    .await
}

/// One DynamoDB operation that must answer 200.
async fn ddb_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = ddb(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with body {parsed}"
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

/// A tag map as the expected `BTreeMap`.
fn expected(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The ARN form DynamoDB mints for a table of the serving Region and account.
fn table_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT_ID}:table/{name}")
}

/// Create a table — `PAY_PER_REQUEST`, one `HASH` key — carrying `tags`, and return the
/// ARN its own `TableDescription` reports, which must be the one this API addresses it
/// by.
async fn create_table(router: &MultiServiceRouter, name: &str, tags: &[(&str, &str)]) -> String {
    let mut request = json!({
        "TableName": name,
        "KeySchema": [{ "AttributeName": "Id", "KeyType": "HASH" }],
        "AttributeDefinitions": [{ "AttributeName": "Id", "AttributeType": "S" }],
        "BillingMode": "PAY_PER_REQUEST",
    });
    if !tags.is_empty() {
        request["Tags"] = tag_list(tags);
    }
    let created = ddb_ok(router, "CreateTable", request).await;
    let minted = created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("a CreateTable body with no TableArn: {created}"))
        .to_string();
    assert_eq!(
        minted,
        table_arn(name),
        "expected CreateTable to mint {}, got {minted}",
        table_arn(name)
    );
    minted
}

/// The ARNs of a `GetResources` page, in the order the service returned them.
fn page_arns(body: &Value) -> Vec<String> {
    body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("a GetResources body with no ResourceTagMappingList: {body}"))
        .iter()
        .map(|mapping| {
            mapping["ResourceARN"]
                .as_str()
                .unwrap_or_else(|| panic!("a mapping with no ResourceARN: {mapping}"))
                .to_string()
        })
        .collect()
}

/// The tag map a `GetResources` page reports for one ARN.
fn page_tags(body: &Value, arn: &str) -> BTreeMap<String, String> {
    let mapping = body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("a GetResources body with no ResourceTagMappingList: {body}"))
        .iter()
        .find(|mapping| mapping["ResourceARN"] == json!(arn))
        .unwrap_or_else(|| panic!("{arn} is not in the page: {body}"));
    mapping["Tags"]
        .as_array()
        .unwrap_or_else(|| panic!("a mapping with no Tags: {mapping}"))
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().expect("Tag.Key").to_string(),
                tag["Value"].as_str().expect("Tag.Value").to_string(),
            )
        })
        .collect()
}

/// `GetResources` filtered the way a client discovers its estate: by the resource type
/// and by one tag key and value.
async fn resources_filtered_by(
    router: &MultiServiceRouter,
    resource_type: &str,
    key: &str,
    value: &str,
) -> Value {
    tagging_ok(
        router,
        "GetResources",
        json!({
            "ResourceTypeFilters": [resource_type],
            "TagFilters": [{ "Key": key, "Values": [value] }],
        }),
    )
    .await
}

/// `GetResources` restricted to one ARN.
async fn resources_for(router: &MultiServiceRouter, arn: &str) -> Value {
    tagging_ok(router, "GetResources", json!({ "ResourceARNList": [arn] })).await
}

/// The tag map DynamoDB's own `ListTagsOfResource` reports for one ARN.
async fn listed_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let body = ddb_ok(router, "ListTagsOfResource", json!({ "ResourceArn": arn })).await;
    body["Tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().expect("Tag.Key").to_string(),
                tag["Value"].as_str().expect("Tag.Value").to_string(),
            )
        })
        .collect()
}

/// Look again until `settled` answers `Some`, or [`POLL_DEADLINE`] passes.
///
/// A DynamoDB tag change is eventually consistent, so the condition is owed rather than
/// immediate: the loop waits for the CONDITION and reports the LAST answer it saw when it
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
            panic!("case {case}: {what} never settled; the last answer was {last}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One tagging path onto a table: the tags its create seeded, the tags a later
/// `TagResource` writes, and the set the table carries once both have landed.
struct Row {
    case: &'static str,
    table: &'static str,
    seeded: &'static [(&'static str, &'static str)],
    written: &'static [(&'static str, &'static str)],
    settled: &'static [(&'static str, &'static str)],
    /// The key and value a client filters `GetResources` by to find this table, which is
    /// one of the pairs the write carried.
    filter: (&'static str, &'static str),
}

/// The rows: a single key written onto an untagged table (the case the issue cites), a
/// batch of three, a key the create already carried being overwritten, an empty tag value
/// (`TagValueString` is `len 0..256`, so it is a value and not an absence), and a write
/// onto a table the create already tagged.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one key on a table created with none",
            table: "native-one-key",
            seeded: &[],
            written: &[("k", "v")],
            settled: &[("k", "v")],
            filter: ("k", "v"),
        },
        Row {
            case: "three keys at once",
            table: "native-three-keys",
            seeded: &[],
            written: &[
                ("Environment", "prod"),
                ("Project", "project1"),
                ("owner", "billing"),
            ],
            settled: &[
                ("Environment", "prod"),
                ("Project", "project1"),
                ("owner", "billing"),
            ],
            filter: ("Project", "project1"),
        },
        Row {
            case: "a key the create carried, overwritten",
            table: "native-overwrite",
            seeded: &[("Environment", "staging"), ("team", "core")],
            written: &[("Environment", "prod")],
            settled: &[("Environment", "prod"), ("team", "core")],
            filter: ("Environment", "prod"),
        },
        Row {
            case: "an empty tag value",
            table: "native-empty-value",
            seeded: &[],
            written: &[("owner", "")],
            settled: &[("owner", "")],
            filter: ("owner", ""),
        },
        Row {
            case: "a key added beside the ones the create seeded",
            table: "Native.Table_4",
            seeded: &[("Environment", "dev")],
            written: &[("CostCentre", "cc-42")],
            settled: &[("CostCentre", "cc-42"), ("Environment", "dev")],
            filter: ("CostCentre", "cc-42"),
        },
    ]
}

/// A tag written with DynamoDB's own `TagResource` is discoverable through
/// `GetResources`: the filtered query answers exactly one mapping, whose `ResourceARN` is
/// the table's own `TableArn` and whose `Tags` carry every pair the table holds — and
/// DynamoDB's own `ListTagsOfResource` reports the same pairs, because there is ONE tag
/// set and both operations read it. A later `UntagResource` withdraws the entry, so the
/// same filtered query answers nothing.
#[tokio::test]
async fn natively_written_tags_are_discoverable_through_get_resources() {
    let router = emulator();
    for row in rows() {
        let arn = create_table(&router, row.table, row.seeded).await;
        let (key, value) = row.filter;
        ddb_ok(
            &router,
            "TagResource",
            json!({ "ResourceArn": &arn, "Tags": tag_list(row.written) }),
        )
        .await;

        let want = expected(row.settled);
        let page = poll_until(row.case, "the index entry for the native tag", || async {
            let page = resources_filtered_by(&router, "dynamodb:table", key, value).await;
            let arns = page_arns(&page);
            if arns == vec![arn.clone()] {
                Ok(page)
            } else {
                Err(format!("{arns:?}"))
            }
        })
        .await;
        let got = page_tags(&page, &arn);
        assert_eq!(
            got, want,
            "case {}: expected GetResources to report {want:?} for {arn}, got {got:?}",
            row.case
        );

        let echoed = listed_tags(&router, &arn).await;
        assert_eq!(
            echoed, want,
            "case {}: expected ListTagsOfResource to report {want:?} for {arn}, got \
             {echoed:?}",
            row.case
        );

        let keys: Vec<&str> = row.written.iter().map(|(key, _)| *key).collect();
        ddb_ok(
            &router,
            "UntagResource",
            json!({ "ResourceArn": &arn, "TagKeys": keys }),
        )
        .await;
        poll_until(row.case, "the withdrawal of the index entry", || async {
            let page = resources_filtered_by(&router, "dynamodb:table", key, value).await;
            let arns = page_arns(&page);
            if arns.is_empty() {
                Ok(())
            } else {
                Err(format!("{arns:?}"))
            }
        })
        .await;

        // The keys the untag did not name are untouched, on the one tag set both
        // surfaces read.
        let left: BTreeMap<String, String> = want
            .into_iter()
            .filter(|(key, _)| !keys.iter().any(|removed| removed == key))
            .collect();
        let echoed = listed_tags(&router, &arn).await;
        assert_eq!(
            echoed, left,
            "case {}: expected the untag to leave {left:?} on {arn}, got {echoed:?}",
            row.case
        );
    }
}

/// `ResourceTypeFilters` select DynamoDB tables by the `service[:resourceType]` key their
/// ARN carries: the bare service token selects every tagged table and the qualified token
/// only tables. A table carrying no tag is in no answer ("GetResources does not return
/// untagged resources"), and a type nothing answers to is an empty page rather than an
/// error.
#[tokio::test]
async fn resource_type_filters_select_tagged_dynamodb_tables() {
    let router = emulator();
    let tagged = create_table(&router, "filtered-tagged", &[("Scope", "selected")]).await;
    let untagged = create_table(&router, "filtered-untagged", &[]).await;

    // The create's own tag set propagates like any other tag change, so the row is owed
    // rather than immediate.
    poll_until("the create's tag set", "the index entry", || async {
        let page = tagging_ok(
            &router,
            "GetResources",
            json!({ "ResourceTypeFilters": ["dynamodb:table"] }),
        )
        .await;
        let arns = page_arns(&page);
        if arns == vec![tagged.clone()] {
            Ok(())
        } else {
            Err(format!("{arns:?}"))
        }
    })
    .await;

    let cases: Vec<(&str, Vec<&str>, Vec<String>)> = vec![
        ("the bare service token", vec!["dynamodb"], vec![tagged.clone()]),
        ("the qualified type", vec!["dynamodb:table"], vec![tagged.clone()]),
        (
            "both spellings, whose selections are the union of each",
            vec!["dynamodb", "dynamodb:table"],
            vec![tagged.clone()],
        ),
        // An index is not a resource DynamoDB holds a tag set for, so nothing answers to
        // this type.
        ("a type nothing tagged", vec!["dynamodb:index"], Vec::new()),
    ];
    for (case, filters, want) in cases {
        let page = tagging_ok(
            &router,
            "GetResources",
            json!({ "ResourceTypeFilters": filters }),
        )
        .await;
        let mut got = page_arns(&page);
        got.sort();
        assert_eq!(
            got, want,
            "case {case}: expected ResourceTypeFilters={filters:?} to select {want:?}, got \
             {got:?}"
        );
    }

    let page = resources_for(&router, &untagged).await;
    let arns = page_arns(&page);
    assert!(
        arns.is_empty(),
        "case an untagged table: expected no mapping for {untagged}, got {arns:?}"
    );
}

/// A table's tags have ONE home — DynamoDB's own record — so a tag written through THIS
/// API is the table's own tag, which `ListTagsOfResource` reports, and an untag here
/// removes it from that same record.
#[tokio::test]
async fn a_tag_written_through_this_api_is_the_tables_own_tag() {
    let router = emulator();
    let arn = create_table(&router, "cross-written", &[("Environment", "dev")]).await;

    let body = tagging_ok(
        &router,
        "TagResources",
        json!({ "ResourceARNList": [&arn], "Tags": { "CostCentre": "cc-42" } }),
    )
    .await;
    assert_eq!(
        body["FailedResourcesMap"],
        json!({}),
        "case a write through this API: expected no failed resource for {arn}, got {}",
        body["FailedResourcesMap"]
    );

    let want = expected(&[("CostCentre", "cc-42"), ("Environment", "dev")]);
    poll_until("a write through this API", "the table's own tag set", || async {
        let got = listed_tags(&router, &arn).await;
        if got == want {
            Ok(())
        } else {
            Err(format!("{got:?}"))
        }
    })
    .await;

    let body = tagging_ok(
        &router,
        "UntagResources",
        json!({ "ResourceARNList": [&arn], "TagKeys": ["CostCentre"] }),
    )
    .await;
    assert_eq!(
        body["FailedResourcesMap"],
        json!({}),
        "case an untag through this API: expected no failed resource for {arn}, got {}",
        body["FailedResourcesMap"]
    );

    let left = expected(&[("Environment", "dev")]);
    poll_until("an untag through this API", "the table's own tag set", || async {
        let got = listed_tags(&router, &arn).await;
        if got == left {
            Ok(())
        } else {
            Err(format!("{got:?}"))
        }
    })
    .await;
}

/// A value in the `dynamodb` namespace that names no taggable table is answered per
/// RESOURCE, with DynamoDB's own refusal: a table ARN naming nothing earns its
/// `ResourceNotFoundException`, and a sub-resource ARN — one it will not resolve to a
/// table at all — earns its `ValidationException`. Both are `StatusCode` 400, and the
/// live ARN in the same batch is tagged.
#[tokio::test]
async fn an_unresolvable_dynamodb_arn_fails_with_that_services_refusal() {
    let router = emulator();
    let present = create_table(&router, "batch-present", &[]).await;

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "a table that was never created",
            table_arn("ghost"),
            "ResourceNotFoundException",
        ),
        (
            "an index of a table, which holds no tag set of its own",
            format!("{}/index/by-owner", table_arn("batch-present")),
            "ValidationException",
        ),
        (
            "a stream of a table",
            format!(
                "{}/stream/2026-01-01T00:00:00.000",
                table_arn("batch-present")
            ),
            "ValidationException",
        ),
    ];
    for (case, arn, code) in cases {
        let body = tagging_ok(
            &router,
            "TagResources",
            json!({
                "ResourceARNList": [&arn, &present],
                "Tags": { "Project": "project1" },
            }),
        )
        .await;
        let failure = &body["FailedResourcesMap"][arn.as_str()];
        assert_eq!(
            failure["ErrorCode"],
            json!(code),
            "case {case}: expected {code} for {arn}, got {}",
            body["FailedResourcesMap"]
        );
        assert_eq!(
            failure["StatusCode"],
            json!(400),
            "case {case}: expected StatusCode 400 for {arn}, got {failure}"
        );
        assert_eq!(
            body["FailedResourcesMap"]
                .as_object()
                .map(serde_json::Map::len),
            Some(1),
            "case {case}: the live ARN of the batch must be tagged, got {}",
            body["FailedResourcesMap"]
        );
    }

    // The live ARN really was tagged, on the table's own record.
    let want = expected(&[("Project", "project1")]);
    poll_until("the live ARN of the batch", "the table's own tag set", || async {
        let got = listed_tags(&router, &present).await;
        if got == want {
            Ok(())
        } else {
            Err(format!("{got:?}"))
        }
    })
    .await;
}

/// A table deleted through DynamoDB is gone from the owning service, so this API stops
/// reporting it — with no untag anywhere — while the table beside it keeps its row.
#[tokio::test]
async fn a_table_deleted_through_dynamodb_leaves_the_index() {
    let router = emulator();
    let doomed = create_table(&router, "deleted-table", &[("Scope", "doomed")]).await;
    let kept = create_table(&router, "kept-table", &[("Scope", "kept")]).await;

    for (case, arn) in [("the doomed table", &doomed), ("the kept table", &kept)] {
        poll_until(case, "the create's index entry", || async {
            let page = resources_for(&router, arn).await;
            let arns = page_arns(&page);
            if arns == vec![arn.clone()] {
                Ok(())
            } else {
                Err(format!("{arns:?}"))
            }
        })
        .await;
    }

    ddb_ok(
        &router,
        "DeleteTable",
        json!({ "TableName": "deleted-table" }),
    )
    .await;

    poll_until("a deleted table", "the withdrawal of its index entry", || async {
        let page = resources_for(&router, &doomed).await;
        let arns = page_arns(&page);
        if arns.is_empty() {
            Ok(())
        } else {
            Err(format!("{arns:?}"))
        }
    })
    .await;

    let page = resources_for(&router, &kept).await;
    let arns = page_arns(&page);
    assert_eq!(
        arns,
        vec![kept.clone()],
        "case the kept table: expected {kept} to be untouched, got {arns:?}"
    );
}
