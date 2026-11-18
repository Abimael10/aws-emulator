// SPDX-License-Identifier: CC0-1.0

//! A Resource Groups group whose definition is a `TAG_FILTERS_1_0` resource query
//! really holds the resources that query selects out of the ACCOUNT — resources
//! other services own, an Amazon SQS queue and an AWS KMS key here — asserted over
//! the production service set (`aws_emulator_router::build_router` on one engine,
//! exactly as the binary assembles it).
//!
//! The RULE pinned here: the membership of a query-defined group is DERIVED by
//! running the group's stored `ResourceQuery` over the account's resources in the
//! group's Region every time it is read — a tag query selecting the resources whose
//! type its `ResourceTypeFilters` admits and whose tags satisfy every one of its
//! `TagFilters`, each reported with the resource type its ARN names — so a resource
//! the query selects is listed however it came to carry those tags and stops being
//! listed as soon as it no longer does, and a request's own `resource-type`
//! `Filters` NARROWS that derived set rather than replacing it.
//! `ListGroupResources` names the resource-tagging read among its required
//! permissions ("resource-groups:ListGroupResources … tag:GetResources"), which is
//! that account-wide view.
//!
//! Two crates have to agree for this to hold — the service that OWNS the tagged
//! resource and Resource Groups, which resolves membership from it — so the suite
//! lives here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: which service owns the
//! resource and which type it is, how many tag keys the query names, how many values
//! one key carries, a key named with no value, which types the query admits and how
//! many, a supported type nothing is, and the case of a tag value. Each test runs on
//! a FRESH emulator, and nothing waits on the wall clock.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and so the Region the resources
/// and the groups live in.
const REGION: &str = "us-west-2";

/// The URIs the model binds the operations this suite drives to
/// (`smithy_query(query="operation",operation="CreateGroup,ListGroupResources")`).
const CREATE_GROUP: &str = "/groups";
const LIST: &str = "/list-group-resources";

/// The one filter name `ListGroupResources` accepts
/// (`smithy_query(query="shape",shape="ResourceFilterName")`).
const RESOURCE_TYPE_FILTER: &str = "resource-type";

/// The FIXED text a `resource-type` filter value the addressed group's query does
/// not admit is refused with, before the values that failed the validation
/// (`smithy_query(query="doc",shape="ListGroupResourcesInput",member="Filters")`).
const INCOMPATIBLE_FILTERS_MESSAGE: &str = "Filters not valid: The resource types specified as \
     filters in the request are incompatible with the group query:";

/// The CloudFormation-style types the resources below are of, from the AWS
/// CloudFormation resource and property types reference — the vocabulary
/// `ResourceIdentifier$ResourceType` is documented in
/// (`smithy_query(query="shape",shape="ResourceIdentifier")`) — plus a supported
/// type nothing in the account is, and the type the request filter names that the
/// fixture group's query does not admit.
const QUEUE_TYPE: &str = "AWS::SQS::Queue";
const KEY_TYPE: &str = "AWS::KMS::Key";
const TABLE_TYPE: &str = "AWS::DynamoDB::Table";
const INSTANCE_TYPE: &str = "AWS::EC2::Instance";

/// The whole emulator over one engine, exactly as the binary assembles it, so which
/// service claims each call is decided by the production mount.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern: the `Authorization`
    // header below is what routes a request and tells a handler its Region.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`].
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One request, as its status and parsed body.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    path: &str,
    target: Option<&str>,
    content_type: &str,
    body: &Value,
) -> (u16, Value) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let (status, parsed) = dispatch(
        router,
        "sqs",
        "/",
        Some(&target),
        "application/x-amz-json-1.0",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case Amazon SQS {operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// One AWS KMS call (`awsJson1_1`, `TrentService.<Operation>`) that must answer 200.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("TrentService.{operation}");
    let (status, parsed) = dispatch(
        router,
        "kms",
        "/",
        Some(&target),
        "application/x-amz-json-1.1",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case AWS KMS {operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// One Resource Groups call over its restJson1 wire, as its status and parsed body.
async fn resource_groups(router: &MultiServiceRouter, path: &str, body: &Value) -> (u16, Value) {
    dispatch(
        router,
        "resource-groups",
        path,
        None,
        "application/json",
        body,
    )
    .await
}

/// One Resource Groups call that must answer the modelled 200.
async fn resource_groups_ok(
    router: &MultiServiceRouter,
    case: &str,
    path: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = resource_groups(router, path, body).await;
    assert_eq!(
        status, 200,
        "case {case}: expected HTTP 200 from POST {path}, got {status} with {parsed}"
    );
    parsed
}

/// A queue created through Amazon SQS's own API carrying `tags`, as the ARN that
/// service minted for it beside the URL its tags are addressed by.
async fn queue(router: &MultiServiceRouter, name: &str, tags: &Value) -> (String, String) {
    let created = sqs(
        router,
        "CreateQueue",
        &json!({ "QueueName": name, "tags": tags }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case CreateQueue {name}: expected a QueueUrl, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: expected a QueueArn, got {attributes}"))
        .to_string();
    (arn, url)
}

/// A key created through AWS KMS's own API carrying `tags`, as the ARN that service
/// minted for it.
async fn key(router: &MultiServiceRouter, tags: &Value) -> String {
    let created = kms(router, "CreateKey", &json!({ "Tags": tags })).await;
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case CreateKey: expected a KeyMetadata.Arn, got {created}"))
        .to_string()
}

/// Create the group `name` defined by the tag query `document`. The model carries a
/// `ResourceQuery`'s `Query` as a JSON STRING inside the structure
/// (`smithy_query(query="shape",shape="ResourceQuery")`).
async fn tag_query_group(router: &MultiServiceRouter, name: &str, document: &Value) {
    resource_groups_ok(
        router,
        name,
        CREATE_GROUP,
        &json!({
            "Name": name,
            "ResourceQuery": { "Type": "TAG_FILTERS_1_0", "Query": document.to_string() }
        }),
    )
    .await;
}

/// The `(ARN, ResourceType)` pairs a page reports, in page order — read off
/// `Resources`, asserting that the deprecated `ResourceIdentifiers` reports the same
/// membership.
fn page_members(case: &str, page: &Value) -> Vec<(String, Option<String>)> {
    let identifiers: Vec<Value> = page["Resources"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected Resources, got {page}"))
        .iter()
        .map(|item| item["Identifier"].clone())
        .collect();
    assert_eq!(
        page["ResourceIdentifiers"].as_array(),
        Some(&identifiers),
        "case {case}: expected ResourceIdentifiers to mirror Resources, got {page}"
    );
    identifiers
        .iter()
        .map(|identifier| {
            (
                identifier["ResourceArn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: expected a ResourceArn, got {page}"))
                    .to_string(),
                identifier["ResourceType"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// The whole membership of `group`, walking every page the listing hands out, with
/// `max_results` entries per page when the case gives one.
async fn membership(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    filters: &[&str],
    max_results: Option<u32>,
) -> Vec<String> {
    let mut arns: Vec<String> = Vec::new();
    let mut next: Option<String> = None;
    // One page per resource the account holds, with room to spare: a listing that
    // never clears its `NextToken` fails here instead of looping.
    for _ in 0..32 {
        let mut request = json!({ "Group": group });
        if !filters.is_empty() {
            request["Filters"] =
                json!([{ "Name": RESOURCE_TYPE_FILTER, "Values": filters.to_vec() }]);
        }
        if let Some(max_results) = max_results {
            request["MaxResults"] = json!(max_results);
        }
        if let Some(token) = &next {
            request["NextToken"] = json!(token);
        }
        let page = resource_groups_ok(router, case, LIST, &request).await;
        arns.extend(page_members(case, &page).into_iter().map(|(arn, _)| arn));
        next = page["NextToken"].as_str().map(str::to_string);
        if next.is_none() {
            return arns;
        }
    }
    panic!("case {case}: the listing of {group} never cleared its NextToken");
}

/// `arns` in ARN order, which is the order a membership is listed in.
fn sorted<const N: usize>(arns: [&String; N]) -> Vec<String> {
    let mut sorted: Vec<String> = arns.into_iter().cloned().collect();
    sorted.sort();
    sorted
}

/// The account every case runs over: two queues and a key, owned by two other
/// services, carrying the tags the queries select on.
struct Account {
    router: MultiServiceRouter,
    /// The queue tagged `marker=one, Stage=Test`.
    queue_one: String,
    /// That queue's URL, which its owning service addresses its tags by.
    queue_one_url: String,
    /// The queue tagged `marker=two`.
    queue_two: String,
    /// The key tagged `marker=one`, so one tag filter spans two owning services.
    key_one: String,
}

impl Account {
    async fn new() -> Self {
        let router = router();
        let (queue_one, queue_one_url) = queue(
            &router,
            "rg-derived-one",
            &json!({ "marker": "one", "Stage": "Test" }),
        )
        .await;
        let (queue_two, _) = queue(&router, "rg-derived-two", &json!({ "marker": "two" })).await;
        let key_one = key(&router, &json!([{ "TagKey": "marker", "TagValue": "one" }])).await;
        Self {
            router,
            queue_one,
            queue_one_url,
            queue_two,
            key_one,
        }
    }
}

/// A group defined by a tag query lists the resources of the ACCOUNT that query
/// selects, whichever service owns them — over a table of whole query documents, and
/// through every page size, since the same derived set is what a page is cut from.
#[tokio::test]
async fn a_tag_query_group_lists_the_tagged_resources_the_query_selects() {
    for row in 0..QUERY_ROWS {
        let account = Account::new().await;
        let document = query_document(row);
        let expected = query_expectation(row, &account);
        let group = format!("derived-{row}");
        tag_query_group(&account.router, &group, &document).await;

        let case = format!("{group} {document}");
        for max_results in [None, Some(1), Some(50)] {
            let listed = membership(&account.router, &case, &group, &[], max_results).await;
            assert_eq!(
                listed, expected,
                "case {case} MaxResults={max_results:?}: expected the derived membership \
                 {expected:?}, got {listed:?}"
            );
        }
    }
}

/// How many rows the query table holds.
const QUERY_ROWS: usize = 7;

/// The `Query` document of row `index`.
fn query_document(index: usize) -> Value {
    match index {
        // The documented shape of such a query: one type, one tag key, one value.
        0 => json!({
            "ResourceTypeFilters": [QUEUE_TYPE],
            "TagFilters": [{ "Key": "marker", "Values": ["one"] }]
        }),
        // One key, several values: the values of one key widen.
        1 => json!({
            "ResourceTypeFilters": [QUEUE_TYPE],
            "TagFilters": [{ "Key": "marker", "Values": ["one", "two"] }]
        }),
        // Several keys narrow: a resource must carry all of them.
        2 => json!({
            "ResourceTypeFilters": [QUEUE_TYPE],
            "TagFilters": [
                { "Key": "marker", "Values": ["one"] },
                { "Key": "Stage", "Values": ["Test"] }
            ]
        }),
        // A filter naming a key with no value is satisfied by carrying the key, and
        // with no type limit at all the selection spans every owning service.
        3 => json!({ "TagFilters": [{ "Key": "Stage" }] }),
        4 => json!({ "TagFilters": [{ "Key": "marker", "Values": ["one"] }] }),
        // Another owning service's type, named beside the queue type.
        5 => json!({
            "ResourceTypeFilters": [KEY_TYPE, TABLE_TYPE],
            "TagFilters": [{ "Key": "marker", "Values": ["one"] }]
        }),
        // Tag values are case-sensitive, so this selects nothing.
        6 => json!({
            "ResourceTypeFilters": [QUEUE_TYPE],
            "TagFilters": [{ "Key": "marker", "Values": ["ONE"] }]
        }),
        other => panic!("the query table has no row {other}"),
    }
}

/// The ARNs row `index` must select, in the ARN order a listing reports.
fn query_expectation(index: usize, account: &Account) -> Vec<String> {
    match index {
        0 | 2 | 3 => vec![account.queue_one.clone()],
        1 => sorted([&account.queue_one, &account.queue_two]),
        4 => sorted([&account.queue_one, &account.key_one]),
        5 => vec![account.key_one.clone()],
        6 => Vec::new(),
        other => panic!("the query table has no row {other}"),
    }
}

/// Every derived member carries the type its ARN names and no `Status` (a query
/// selects NOW), and a request's `resource-type` filter narrows the derived set to
/// the types it names, while a value the group's own query does not admit is refused
/// `BadRequestException` (400) with the service's fixed sentence followed by the
/// values that failed the validation.
#[tokio::test]
async fn a_resource_type_filter_narrows_the_derived_membership_or_is_refused() {
    let account = Account::new().await;
    let both = json!({
        "ResourceTypeFilters": [QUEUE_TYPE, KEY_TYPE],
        "TagFilters": [{ "Key": "marker" }]
    });
    tag_query_group(&account.router, "derived-filtered", &both).await;

    let page = resource_groups_ok(
        &account.router,
        "the derived types",
        LIST,
        &json!({ "Group": "derived-filtered" }),
    )
    .await;
    let reported = page_members("the derived types", &page);
    let expected: Vec<(String, Option<String>)> =
        sorted([&account.queue_one, &account.queue_two, &account.key_one])
            .into_iter()
            .map(|arn| {
                let resource_type = if arn == account.key_one {
                    KEY_TYPE
                } else {
                    QUEUE_TYPE
                };
                (arn, Some(resource_type.to_string()))
            })
            .collect();
    assert_eq!(
        reported, expected,
        "case the derived types: expected {expected:?}, got {reported:?}"
    );
    for item in page["Resources"].as_array().expect("Resources is a list") {
        assert!(
            item.get("Status").is_none_or(Value::is_null),
            "case the derived types: a query selects NOW, so a member carries no \
             Status: {item}"
        );
    }

    // (the filter values, the ARNs they leave)
    let rows: Vec<(Vec<&str>, Vec<String>)> = vec![
        (
            vec![QUEUE_TYPE],
            sorted([&account.queue_one, &account.queue_two]),
        ),
        (vec![KEY_TYPE], vec![account.key_one.clone()]),
        (
            vec![QUEUE_TYPE, KEY_TYPE],
            sorted([&account.queue_one, &account.queue_two, &account.key_one]),
        ),
    ];
    for (filters, expected) in rows {
        let case = format!("Filters={filters:?}");
        let listed = membership(&account.router, &case, "derived-filtered", &filters, None).await;
        assert_eq!(
            listed, expected,
            "case {case}: expected the filter to narrow the derived membership to \
             {expected:?}, got {listed:?}"
        );
    }

    // A group whose query admits ONE type: the unadmitted filter is the modelled
    // refusal, naming exactly the values that failed.
    let queues_only = json!({
        "ResourceTypeFilters": [QUEUE_TYPE],
        "TagFilters": [{ "Key": "marker", "Values": ["one"] }]
    });
    tag_query_group(&account.router, "derived-queues", &queues_only).await;
    for unadmitted in [vec![INSTANCE_TYPE], vec![KEY_TYPE, INSTANCE_TYPE]] {
        let case = format!("unadmitted Filters={unadmitted:?}");
        let (status, body) = resource_groups(
            &account.router,
            LIST,
            &json!({
                "Group": "derived-queues",
                "Filters": [{ "Name": RESOURCE_TYPE_FILTER, "Values": unadmitted }]
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} with {body}"
        );
        let expected = format!("{INCOMPATIBLE_FILTERS_MESSAGE} {}", unadmitted.join(", "));
        assert_eq!(
            body["Message"].as_str(),
            Some(expected.as_str()),
            "case {case}: expected the message {expected:?}, got {body}"
        );
    }
}

/// The membership is DERIVED, so it follows the owning service's own tags and
/// resources: untagging the resource there takes it out of the group, re-tagging it
/// puts it back, and deleting it there takes it out with nothing to keep in step.
#[tokio::test]
async fn a_derived_membership_follows_the_owning_services_resource() {
    let account = Account::new().await;
    let group = "derived-following";
    tag_query_group(
        &account.router,
        group,
        &json!({
            "ResourceTypeFilters": [QUEUE_TYPE],
            "TagFilters": [{ "Key": "marker", "Values": ["one"] }]
        }),
    )
    .await;

    // (the case, the call made to the OWNING service, the membership it leaves)
    /// One row: the case, the owning service's call (its operation and body) when the
    /// row makes one, and the membership the listing must answer afterwards.
    type Row = (&'static str, Option<(&'static str, Value)>, Vec<String>);
    let rows: Vec<Row> = vec![
        ("tagged", None, vec![account.queue_one.clone()]),
        (
            "untagged",
            Some((
                "UntagQueue",
                json!({ "QueueUrl": &account.queue_one_url, "TagKeys": ["marker"] }),
            )),
            Vec::new(),
        ),
        (
            "re-tagged",
            Some((
                "TagQueue",
                json!({ "QueueUrl": &account.queue_one_url, "Tags": { "marker": "one" } }),
            )),
            vec![account.queue_one.clone()],
        ),
        (
            "deleted",
            Some((
                "DeleteQueue",
                json!({ "QueueUrl": &account.queue_one_url }),
            )),
            Vec::new(),
        ),
    ];
    for (case, call, expected) in rows {
        if let Some((operation, body)) = call {
            sqs(&account.router, operation, &body).await;
        }
        let listed = membership(&account.router, case, group, &[], None).await;
        assert_eq!(
            listed, expected,
            "case {case}: expected the derived membership {expected:?}, got {listed:?}"
        );
    }
}
