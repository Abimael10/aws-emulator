// SPDX-License-Identifier: CC0-1.0

//! A CloudWatch Logs resource has ONE tag set, whichever tagging plane writes it, and an
//! ARN that service will not take is tagged through neither.
//!
//! Two rules are under test here.
//!
//! **One tag set per resource.** `TagResources` "appl[ies] one or more tags to the
//! specified resources", and the resources belong to the service whose namespace their ARN
//! names, so the set this API writes is the set that service itself holds: a log group
//! tagged through this API reports those pairs from CloudWatch Logs' own
//! `ListTagsForResource`, a pair written through CloudWatch Logs' own `TagResource` (or
//! seeded by `CreateLogGroup$tags`) is what `GetResources` discovers the group by, and
//! `UntagResources` removes exactly the keys it names from that same set.
//!
//! **The owning service decides which ARNs it will tag.** Each ARN of `ResourceARNList` is
//! resolved against that service, and one it refuses is reported in `FailedResourcesMap`
//! for that ARN alone — carrying that service's own answer, with HTTP 200 overall — while
//! every other ARN of the same batch is written. A value the member CloudWatch Logs takes
//! an ARN in does not admit at all is refused as the value it is: its `resourceArn` targets
//! `AmazonResourceName`, whose `@pattern` holds no `*`, so the wildcard-suffixed log-group
//! ARN form `DescribeLogGroups` reports (the group together with everything under it) is
//! not a value that plane takes, and nothing in the index ever answers to it.
//!
//! Two crates have to agree for either rule to hold — CloudWatch Logs owns and stores the
//! resource's tag set, the tagging API writes and projects it — so the suite lives here, on
//! the assembled router with both services mounted on ONE engine, which is how the emulator
//! runs.
//!
//! The rows of the first test span what that rule generalises over: which plane the tag
//! came from (a create's `tags`, CloudWatch Logs' own `TagResource`, this API's
//! `TagResources`, and the two together), how many keys the group ends up with, an empty
//! tag value, a key this API's `UntagResources` removes, the punctuation a log group name
//! carries, and a group nothing ever tagged. The rows of the second span what that service
//! refuses: three wildcard forms (the trailing `:*` of a group that exists, a wildcard
//! standing for every group, a wildcard inside a name), a value its member DOES admit
//! naming a kind it holds no tag set for, and one naming a taggable kind the account holds
//! no resource of — the last two earning their own refusals, which is what places the
//! boundary.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatchlogs_logs_20140328 as owner_logs;
use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging_api;

/// The Region the estate is created in, and the Region every call is signed for.
const REGION: &str = "us-east-1";

/// The account segment every ARN this emulator mints carries.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The SigV4 signing names the router routes a request to a service by.
const TAGGING: &str = "tagging";
const LOGS: &str = "logs";

/// The `service:resourceType` key a log group's ARN carries, and the bare service token
/// that selects every resource of that service.
const LOG_GROUP_TYPE: &str = "logs:log-group";
const BARE_TYPE: &str = "logs";

/// The router carrying CloudWatch Logs and the tagging API over ONE engine, the
/// arrangement `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises handler behaviour, not SigV4: the `Authorization` header below is
    // present because its credential scope is what routes a request to a signing name and
    // names the Region, not to be verified.
    router.set_request_authentication(false);
    router.register_service(owner_logs::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(tagging_api::EmulatedService::new(engine));
    router
}

/// Call one operation of one service, signed for [`REGION`], and return its HTTP status and
/// parsed body.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
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

/// One tagging-API operation, which must answer 200.
async fn tagging_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = call(
        router,
        TAGGING,
        &format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// One CloudWatch Logs operation: its status and parsed body.
async fn logs_call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    call(router, LOGS, &format!("Logs_20140328.{operation}"), body).await
}

/// One CloudWatch Logs operation, which must answer 200.
async fn logs_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = logs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with body {parsed}"
    );
    parsed
}

/// A declared tag set as the map the assertions compare against.
fn tag_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// A declared tag set as the `TagMap` both services model a tag write with.
fn tag_object(pairs: &[(&str, &str)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::String((*value).to_string())))
            .collect(),
    )
}

/// The tag set CloudWatch Logs' OWN reader reports for `arn` — a different operation than
/// the one that wrote it.
async fn native_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let body = logs_ok(
        router,
        "ListTagsForResource",
        json!({ "resourceArn": arn }),
    )
    .await;
    body["tags"]
        .as_object()
        .unwrap_or_else(|| panic!("a ListTagsForResource body with no tags: {body}"))
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("a non-string tag value: {body}"))
                    .to_string(),
            )
        })
        .collect()
}

/// The two ARN forms CloudWatch Logs reports for one log group: `logGroupArn`, the group
/// itself, and `arn`, the group together with everything under it (the `:*` form).
struct GroupArns {
    bare: String,
    starred: String,
}

/// Create a log group carrying `seeded`, and return the two ARN forms the service's own
/// `DescribeLogGroups` reports for it.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    seeded: &[(&str, &str)],
) -> GroupArns {
    let mut request = json!({ "logGroupName": name });
    if !seeded.is_empty() {
        request["tags"] = tag_object(seeded);
    }
    logs_ok(router, "CreateLogGroup", request).await;

    let described = logs_ok(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": name }),
    )
    .await;
    let group = described["logGroups"]
        .as_array()
        .unwrap_or_else(|| panic!("a DescribeLogGroups body with no logGroups: {described}"))
        .iter()
        .find(|group| group["logGroupName"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("group {name:?} is not in {described}"));
    let bare = group["logGroupArn"]
        .as_str()
        .unwrap_or_else(|| panic!("group {name:?} reports no logGroupArn: {described}"))
        .to_string();
    let starred = group["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("group {name:?} reports no arn: {described}"))
        .to_string();
    assert_eq!(
        bare,
        format!("arn:aws:logs:{REGION}:{ACCOUNT_ID}:log-group:{name}"),
        "group {name:?}: expected the group's own ARN form, got {bare}"
    );
    assert_eq!(
        starred,
        format!("{bare}:*"),
        "group {name:?}: expected the reported arn to be the bare form with the trailing \
         wildcard, got {starred}"
    );
    GroupArns { bare, starred }
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
async fn selected(router: &MultiServiceRouter, request: Value) -> (Vec<String>, Value) {
    let body = tagging_ok(router, "GetResources", request).await;
    let mut arns: Vec<String> = page_estate(&body).into_keys().collect();
    arns.sort();
    (arns, body)
}

/// One `FailedResourcesMap` entry: the three members `FailureInfo` models.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    status_code: i64,
    error_code: String,
    error_message: String,
}

/// The `FailedResourcesMap` of a batch answer, keyed by ARN.
fn failures(body: &Value) -> BTreeMap<String, Failure> {
    body["FailedResourcesMap"]
        .as_object()
        .unwrap_or_else(|| panic!("a batch answer with no FailedResourcesMap: {body}"))
        .iter()
        .map(|(arn, failure)| {
            (
                arn.clone(),
                Failure {
                    status_code: failure["StatusCode"].as_i64().unwrap_or_default(),
                    error_code: failure["ErrorCode"].as_str().unwrap_or_default().to_string(),
                    error_message: failure["ErrorMessage"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                },
            )
        })
        .collect()
}

/// One log group of the estate: which plane each of its tags came from, and the set it
/// carries once every write has landed.
struct Row {
    case: &'static str,
    group: &'static str,
    /// The pairs its `CreateLogGroup$tags` declares.
    seeded: &'static [(&'static str, &'static str)],
    /// The pairs CloudWatch Logs' own `TagResource` writes.
    native: &'static [(&'static str, &'static str)],
    /// The pairs this API's `TagResources` writes.
    through_tagging_api: &'static [(&'static str, &'static str)],
    /// The keys this API's `UntagResources` then removes.
    untagged: &'static [&'static str],
    /// The set the group carries afterwards, computed from the merge rule both planes
    /// state ("If you specify a tag key that is already associated [...] the new tag value
    /// that you specify replaces the previous value"), not read back out of an answer.
    settled: &'static [(&'static str, &'static str)],
}

/// The estate: every combination of the planes a tag can arrive through, the key counts
/// either plane leaves, an empty value, a removal, a name carrying log-group punctuation,
/// and a group nothing ever tagged. The case the issue cites is the first row.
const ROWS: &[Row] = &[
    Row {
        case: "one tag written through the tagging API",
        group: "tagging-api-one",
        seeded: &[],
        native: &[],
        through_tagging_api: &[("Project", "phoenix")],
        untagged: &[],
        settled: &[("Project", "phoenix")],
    },
    Row {
        case: "two tags written through the tagging API",
        group: "tagging-api-two",
        seeded: &[],
        native: &[],
        through_tagging_api: &[("Project", "phoenix"), ("Environment", "prod")],
        untagged: &[],
        settled: &[("Environment", "prod"), ("Project", "phoenix")],
    },
    Row {
        case: "a tagging-API write merges into the create's tags and overwrites one key",
        group: "tagging-api-over-create",
        seeded: &[("Project", "seeded"), ("Owner", "platform")],
        native: &[],
        through_tagging_api: &[("Project", "phoenix"), ("Team", "core")],
        untagged: &[],
        settled: &[
            ("Owner", "platform"),
            ("Project", "phoenix"),
            ("Team", "core"),
        ],
    },
    Row {
        case: "both planes write into the one set",
        group: "both-planes",
        seeded: &[],
        native: &[("Native", "yes")],
        through_tagging_api: &[("Project", "phoenix")],
        untagged: &[],
        settled: &[("Native", "yes"), ("Project", "phoenix")],
    },
    Row {
        case: "a tagging-API write whose value is empty, over the owner's own write",
        group: "empty-value",
        seeded: &[],
        native: &[("Owner", "platform")],
        through_tagging_api: &[("Owner", "")],
        untagged: &[],
        settled: &[("Owner", "")],
    },
    Row {
        case: "a key the tagging API's untag removes leaves the rest of the set",
        group: "untagged-one-key",
        seeded: &[("Project", "phoenix")],
        native: &[("Owner", "platform")],
        through_tagging_api: &[("Team", "core")],
        untagged: &["Owner"],
        settled: &[("Project", "phoenix"), ("Team", "core")],
    },
    Row {
        // The `#` a log group name may also carry is left out on purpose: CloudWatch
        // Logs' own `resourceArn` is `^[\w+=/:,.@-]*$`, so an ARN carrying one is not a
        // value its tagging operations take.
        case: "a group name carrying the punctuation a log group name may hold",
        group: "/sim/group.name_1/tail-2",
        seeded: &[],
        native: &[],
        through_tagging_api: &[("Project", "phoenix")],
        untagged: &[],
        settled: &[("Project", "phoenix")],
    },
    Row {
        case: "a group nothing ever tagged",
        group: "never-tagged",
        seeded: &[],
        native: &[],
        through_tagging_api: &[],
        untagged: &[],
        settled: &[],
    },
];

/// The estate once every write has landed: each row's ARN forms by group name, and the
/// `(ARN -> tags)` map the index has to report, which is every row that ends up carrying at
/// least one tag.
struct Estate {
    arns: BTreeMap<&'static str, GroupArns>,
    tagged: BTreeMap<String, BTreeMap<String, String>>,
}

/// Create every row's group and apply its writes through the plane the row names.
async fn settled_estate(router: &MultiServiceRouter) -> Estate {
    let mut arns: BTreeMap<&'static str, GroupArns> = BTreeMap::new();
    let mut tagged = BTreeMap::new();
    for row in ROWS {
        let case = row.case;
        let group = create_group(router, row.group, row.seeded).await;
        if !row.native.is_empty() {
            logs_ok(
                router,
                "TagResource",
                json!({ "resourceArn": &group.bare, "tags": tag_object(row.native) }),
            )
            .await;
        }
        if !row.through_tagging_api.is_empty() {
            let answered = tagging_ok(
                router,
                "TagResources",
                json!({
                    "ResourceARNList": [&group.bare],
                    "Tags": tag_object(row.through_tagging_api),
                }),
            )
            .await;
            let failed = failures(&answered);
            assert!(
                failed.is_empty(),
                "case {case:?}: expected TagResources on {} to fail no ARN, got {failed:?}",
                group.bare
            );
        }
        if !row.untagged.is_empty() {
            let answered = tagging_ok(
                router,
                "UntagResources",
                json!({ "ResourceARNList": [&group.bare], "TagKeys": row.untagged }),
            )
            .await;
            let failed = failures(&answered);
            assert!(
                failed.is_empty(),
                "case {case:?}: expected UntagResources on {} to fail no ARN, got {failed:?}",
                group.bare
            );
        }
        if !row.settled.is_empty() {
            tagged.insert(group.bare.clone(), tag_map(row.settled));
        }
        arns.insert(row.group, group);
    }
    Estate { arns, tagged }
}

/// A tag the tagging API writes IS the tag CloudWatch Logs holds: the service's own
/// `ListTagsForResource` reports exactly the set the writes left, whichever plane each pair
/// arrived through, and the index reports that same set.
#[tokio::test]
async fn one_tag_set_serves_both_the_tagging_api_and_cloudwatch_logs() {
    let router = emulator();
    let estate = settled_estate(&router).await;

    // The owning service's OWN reader, which is a different operation than the one that
    // wrote the tags.
    for row in ROWS {
        let case = row.case;
        let arn = &estate.arns[row.group].bare;
        let want = tag_map(row.settled);
        let got = native_tags(&router, arn).await;
        assert_eq!(
            got, want,
            "case {case:?}: expected CloudWatch Logs' ListTagsForResource on {arn} to report \
             {want:?}, got {got:?}"
        );
    }

    // The index reports the same sets, pair for pair, and reports no untagged group:
    // "GetResources does not return untagged resources".
    let body = tagging_ok(&router, "GetResources", json!({})).await;
    let reported = page_estate(&body);
    assert_eq!(
        reported, estate.tagged,
        "expected the index of {REGION} to report {:?}, got {reported:?}",
        estate.tagged
    );

    let arn = |group: &str| estate.arns[group].bare.clone();
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "a key and value written through the tagging API",
            json!({ "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }] }),
            vec![
                arn("/sim/group.name_1/tail-2"),
                arn("both-planes"),
                arn("tagging-api-one"),
                arn("tagging-api-over-create"),
                arn("tagging-api-two"),
                arn("untagged-one-key"),
            ],
        ),
        (
            "a key and value written through CloudWatch Logs' own TagResource",
            json!({ "TagFilters": [{ "Key": "Native", "Values": ["yes"] }] }),
            vec![arn("both-planes")],
        ),
        (
            "a key a create seeded and the tagging API left alone",
            json!({ "TagFilters": [{ "Key": "Owner", "Values": ["platform"] }] }),
            vec![arn("tagging-api-over-create")],
        ),
        (
            "an empty value a tagging-API write left",
            json!({ "TagFilters": [{ "Key": "Owner", "Values": [""] }] }),
            vec![arn("empty-value")],
        ),
        (
            "a key the tagging API's untag removed",
            json!({ "TagFilters": [{ "Key": "Owner" }] }),
            vec![arn("empty-value"), arn("tagging-api-over-create")],
        ),
        (
            "the qualified resource type a log group's ARN carries",
            json!({
                "TagFilters": [{ "Key": "Project", "Values": ["phoenix"] }],
                "ResourceTypeFilters": [LOG_GROUP_TYPE],
            }),
            vec![
                arn("/sim/group.name_1/tail-2"),
                arn("both-planes"),
                arn("tagging-api-one"),
                arn("tagging-api-over-create"),
                arn("tagging-api-two"),
                arn("untagged-one-key"),
            ],
        ),
        (
            "the bare service token",
            json!({ "ResourceTypeFilters": [BARE_TYPE] }),
            estate.tagged.keys().cloned().collect(),
        ),
        (
            "a qualified type no tagged resource of the service answers to",
            json!({ "ResourceTypeFilters": ["logs:log-stream"] }),
            Vec::new(),
        ),
        (
            "ResourceARNList naming every group of the estate, tagged or not",
            json!({
                "ResourceARNList": ROWS
                    .iter()
                    .map(|row| arn(row.group))
                    .collect::<Vec<String>>(),
            }),
            estate.tagged.keys().cloned().collect(),
        ),
    ];

    for (case, request, mut want) in cases {
        want.sort();
        let (got, body) = selected(&router, request.clone()).await;
        assert_eq!(
            got, want,
            "case {case:?}: expected {request} to select {want:?}, got {got:?} from {body}"
        );
    }
}

/// A tag written through the tagging API survives the owning service's own untag, and a
/// group DELETED there leaves the index: the tag set is the resource's, not a second copy.
#[tokio::test]
async fn the_owning_services_own_writes_decide_what_the_index_reports() {
    let router = emulator();
    let group = create_group(&router, "owner-decides", &[]).await;

    tagging_ok(
        &router,
        "TagResources",
        json!({
            "ResourceARNList": [&group.bare],
            "Tags": tag_object(&[("Project", "phoenix"), ("Owner", "platform")]),
        }),
    )
    .await;

    // CloudWatch Logs' own `UntagResource` removes a key the tagging API wrote, because
    // there is one set and that is the set it edits.
    logs_ok(
        &router,
        "UntagResource",
        json!({ "resourceArn": &group.bare, "tagKeys": ["Owner"] }),
    )
    .await;
    let case = "the owner's untag";
    let want = tag_map(&[("Project", "phoenix")]);
    let got = native_tags(&router, &group.bare).await;
    assert_eq!(
        got, want,
        "case {case:?}: expected the set after the owner's untag to be {want:?}, got {got:?}"
    );
    let (selected_arns, body) =
        selected(&router, json!({ "TagFilters": [{ "Key": "Owner" }] })).await;
    assert!(
        selected_arns.is_empty(),
        "case {case:?}: expected no resource to answer to the removed key, got \
         {selected_arns:?} from {body}"
    );

    // Deleting the group there takes its tags out of the index with it: the entry was the
    // group's own tag set, so there is nothing left to report.
    logs_ok(
        &router,
        "DeleteLogGroup",
        json!({ "logGroupName": "owner-decides" }),
    )
    .await;
    let case = "the owner's delete";
    let (selected_arns, body) = selected(&router, json!({})).await;
    assert!(
        selected_arns.is_empty(),
        "case {case:?}: expected the deleted group to leave the index, got {selected_arns:?} \
         from {body}"
    );
}

/// One batch, one entry per ARN that cannot be tagged: the wildcard forms, and a
/// One batch, one entry per ARN CloudWatch Logs will not tag: the values its tagging
/// member does not admit, a kind it holds no tag set for, and a well-formed ARN naming a
/// group the account does not hold.
struct PatternCase {
    case: &'static str,
    /// The ARN as the caller writes it, `{bare}` standing for the live group's own ARN.
    arn: &'static str,
    /// The `FailureInfo$ErrorCode` the entry carries.
    error_code: &'static str,
    /// The whole `FailureInfo$ErrorMessage` the entry states.
    error_message: &'static str,
}

/// The refusal of a VALUE the owning service's tagging member does not admit: the member
/// named as that service names it, under the validation code AWS reports for it.
const INVALID_VALUE: (&str, &str) = ("ValidationException", "Invalid resourceArn");

/// The ARN forms CloudWatch Logs will not tag.
///
/// The first row is the form `DescribeLogGroups` reports a group's `arn` in, which is the
/// value a client feeds straight back in; the next two are other wildcard patterns, which
/// the same member constraint refuses. The last two are the controls that place the
/// boundary: a value the member DOES admit, naming a kind that service holds no tag set
/// for, and one naming a group of a kind it does, which the account does not hold — each
/// earning a refusal of its own.
const PATTERNS: &[PatternCase] = &[
    PatternCase {
        case: "the trailing wildcard of a group that exists",
        arn: "{bare}:*",
        error_code: INVALID_VALUE.0,
        error_message: INVALID_VALUE.1,
    },
    PatternCase {
        case: "a wildcard standing for every log group of the account",
        arn: "arn:aws:logs:{region}:{account}:log-group:*",
        error_code: INVALID_VALUE.0,
        error_message: INVALID_VALUE.1,
    },
    PatternCase {
        case: "a wildcard inside a log group name",
        arn: "arn:aws:logs:{region}:{account}:log-group:/sim/*/tail",
        error_code: INVALID_VALUE.0,
        error_message: INVALID_VALUE.1,
    },
    PatternCase {
        case: "a log stream, a kind that service holds no tag set for",
        arn: "arn:aws:logs:{region}:{account}:log-group:batch-with-patterns:log-stream:one",
        error_code: "InvalidParameterException",
        error_message: "resourceArn must name a resource that supports tagging. Currently, the \
                        only CloudWatch Logs resources that can be tagged are log groups and \
                        destinations.",
    },
    PatternCase {
        case: "a taggable kind the account holds no resource of",
        arn: "arn:aws:logs:{region}:{account}:log-group:no-such-group",
        error_code: "ResourceNotFoundException",
        error_message: "The specified log group does not exist.",
    },
];

/// The tag the batch below writes onto the one live ARN it carries.
const BATCH_TAG: (&str, &str) = ("Project", "phoenix");

/// A batch mixing the live ARN of a log group with ARNs the owning service refuses: the
/// live one is tagged, each of the others is its own `FailedResourcesMap` entry carrying
/// that service's own answer, and none of them becomes a resource the index reports.
#[tokio::test]
async fn an_arn_the_owning_service_refuses_is_a_per_resource_failure() {
    let router = emulator();
    let group = create_group(&router, "batch-with-patterns", &[]).await;
    let written = tag_map(&[BATCH_TAG]);

    let spelled = |case: &PatternCase| {
        case.arn
            .replace("{bare}", &group.bare)
            .replace("{region}", REGION)
            .replace("{account}", ACCOUNT_ID)
    };
    let mut arns: Vec<String> = vec![group.bare.clone()];
    arns.extend(PATTERNS.iter().map(spelled));

    let answered = tagging_ok(
        &router,
        "TagResources",
        json!({ "ResourceARNList": &arns, "Tags": tag_object(&[BATCH_TAG]) }),
    )
    .await;
    let reported = failures(&answered);

    // Exactly the ARNs that service refuses, and the live one is not among them.
    let want_keys: Vec<String> = PATTERNS.iter().map(spelled).collect();
    let got_keys: Vec<String> = reported.keys().cloned().collect();
    let mut want_sorted = want_keys.clone();
    want_sorted.sort();
    assert_eq!(
        got_keys, want_sorted,
        "expected FailedResourcesMap to name {want_sorted:?}, got {got_keys:?} from {answered}"
    );

    for case in PATTERNS {
        let arn = spelled(case);
        let failure = reported
            .get(&arn)
            .unwrap_or_else(|| panic!("case {:?}: {arn} is not in {answered}", case.case));
        assert_eq!(
            failure.status_code, 400,
            "case {:?}: expected the entry for {arn} to carry status 400, got {failure:?}",
            case.case
        );
        assert_eq!(
            failure.error_code, case.error_code,
            "case {:?}: expected the entry for {arn} to carry {}, got {failure:?}",
            case.case, case.error_code
        );
        assert_eq!(
            failure.error_message, case.error_message,
            "case {:?}: expected the entry for {arn} to state {:?}, got {failure:?}",
            case.case, case.error_message
        );
    }

    // The rest of the batch landed: the live group carries the tag the request wrote, in
    // the owning service's own tag set.
    let got = native_tags(&router, &group.bare).await;
    assert_eq!(
        got, written,
        "expected the live ARN of the batch to be tagged {written:?}, got {got:?}"
    );

    // No refused ARN became a resource of the index.
    let (reported_arns, body) = selected(&router, json!({})).await;
    assert_eq!(
        reported_arns,
        vec![group.bare.clone()],
        "expected the index to report the live group alone, got {reported_arns:?} from {body}"
    );

    // `UntagResources` draws the same line, for the same batch: the live ARN's key is
    // removed and every refused ARN is its own entry again.
    let answered = tagging_ok(
        &router,
        "UntagResources",
        json!({ "ResourceARNList": &arns, "TagKeys": [BATCH_TAG.0] }),
    )
    .await;
    let got_keys: Vec<String> = failures(&answered).keys().cloned().collect();
    assert_eq!(
        got_keys, want_sorted,
        "expected UntagResources to name {want_sorted:?}, got {got_keys:?} from {answered}"
    );
    let got = native_tags(&router, &group.bare).await;
    assert!(
        got.is_empty(),
        "expected the untagged key to leave the owning service's set, got {got:?}"
    );
}

/// A refused ARN is refused PER ARN, not at request level: a batch of nothing but the
/// wildcard-suffixed form still answers HTTP 200 with one entry for it, which is what tells
/// a caller which ARN form to use.
#[tokio::test]
async fn a_batch_of_refused_arns_alone_still_answers_per_resource() {
    let router = emulator();
    let group = create_group(&router, "patterns-only", &[]).await;
    let answered = tagging_ok(
        &router,
        "TagResources",
        json!({
            "ResourceARNList": [&group.starred],
            "Tags": tag_object(&[BATCH_TAG]),
        }),
    )
    .await;
    let reported = failures(&answered);
    let got_keys: Vec<String> = reported.keys().cloned().collect();
    assert_eq!(
        got_keys,
        vec![group.starred.clone()],
        "expected the wildcard-suffixed ARN alone to fail, got {got_keys:?} from {answered}"
    );
    let failure = &reported[&group.starred];
    assert_eq!(
        (
            failure.status_code,
            failure.error_code.as_str(),
            failure.error_message.as_str()
        ),
        (400, INVALID_VALUE.0, INVALID_VALUE.1),
        "expected the entry for {} to carry the owning service's refusal of the value, got \
         {failure:?}",
        group.starred
    );

    // Nothing was written: the group that ARN form names is still untagged, and the index
    // reports no resource at all.
    let got = native_tags(&router, &group.bare).await;
    assert!(
        got.is_empty(),
        "expected the group that ARN form names to stay untagged, got {got:?}"
    );
    let (reported_arns, body) = selected(&router, json!({})).await;
    assert!(
        reported_arns.is_empty(),
        "expected no resource in the index, got {reported_arns:?} from {body}"
    );
}
