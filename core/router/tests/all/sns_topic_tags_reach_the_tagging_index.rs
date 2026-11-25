// SPDX-License-Identifier: CC0-1.0

//! An Amazon SNS topic tagged through SNS's OWN tag plane is a row of the Resource Groups
//! Tagging index, and `GetResources`' mutually-exclusive-parameter refusal names the member
//! the request combined with `ResourceARNList`.
//!
//! Two rules are under test.
//!
//! **Discovery**: the tag set the service that OWNS a resource holds for it — here a
//! topic's one tag set, seeded by `CreateTopic$Tags` or written by SNS's own `TagResource` —
//! is what the tagging index reports for that resource in the Region the resource lives in,
//! keyed by the ARN that service minted, so `GetResources` selects the topic by its
//! `TagFilters` (AND-ed across filters, each filter's `Values` OR-ed, a filter with no
//! values matching the key with any value), optionally narrowed by `ResourceTypeFilters` in
//! the `service[:resourceType]` spelling the tagging API names the resource by, and SNS's
//! `UntagResource` withdraws exactly that resource from the index. A topic carrying no tag
//! is in no answer ("GetResources does not return untagged resources", that operation's
//! documentation) and a topic of another Region is in no answer here ("Returns all the
//! tagged or previously tagged resources that are located in the specified Amazon Web
//! Services Region for the account").
//!
//! **Refusal**: `ResourceARNList` is mutually exclusive with the filtering and the
//! pagination members ("You can't specify both this parameter and the ResourceTypeFilters
//! parameter … the TagFilters parameter … any of the pagination parameters (ResourcesPerPage,
//! TagsPerPage, PaginationToken)", `GetResourcesInput$ResourceARNList`), and each
//! combination is refused with HTTP 400 `InvalidParameterException` whose sentence names the
//! member that is not allowed beside a resource list — `TagFilters` being the one AWS speaks
//! of generically, as "Filters … a resource list".
//!
//! Two crates have to agree for the first rule to hold — Amazon SNS owns and stores the
//! topic's tag set, the tagging API projects it — so the suite lives here, on the assembled
//! router with both services mounted on ONE engine, which is how the emulator runs.
//!
//! The estate's rows span what the discovery rule generalises over: how the tag got onto the
//! topic (a create's `Tags`, a later `TagResource`, a `TagResource` overwriting a key the
//! create carried), how many keys the topic ends up with, an empty tag value, a FIFO topic, a
//! topic nothing ever tagged, and a topic of another Region; the queries span the filter
//! forms — key only, key and value, a value nothing carries, several values, several filters,
//! the bare and the qualified resource type, `ResourceARNList`, and no filter at all.
//!
//! Every read of the index is taken in a poll loop that looks again until the condition
//! holds: the suite waits for the CONDITION, never for a fixed time.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126 as tagging_api;
use service_com_amazonaws_sns_amazonsimplenotificationservice as owner_sns;

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

/// The Region the estate is created in, and the Region the index is queried in.
const REGION: &str = "us-east-1";

/// A second Region, to pin the Region scope of the answer: a topic here is reported by that
/// Region's endpoint and by no other.
const OTHER_REGION: &str = "us-west-2";

/// The SigV4 signing names the router routes a request to a service by.
const TAGGING: &str = "tagging";
const SNS: &str = "sns";

/// The API version every awsQuery envelope below names: the version the SNS model declares.
const SNS_VERSION: &str = "2010-03-31";

/// The `service:resourceType` key the tagging API names a topic by, and the bare service
/// token that selects every resource of that service.
const TOPIC_TYPE: &str = "sns:topic";
const BARE_TYPE: &str = "sns";

/// The tag key the whole estate carries, so one query reaches every row.
const KEY: &str = "sim2real";

/// How long a poll loop keeps looking before it gives up. Generous on purpose: it bounds a
/// condition owed at most one short span, so a loaded or slow host still settles well inside
/// it.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks — short relative to the span it waits out, so the
/// loop observes the change rather than the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The router carrying Amazon SNS and the tagging API over ONE engine, the arrangement
/// `core/router` builds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises handler behaviour, not SigV4: the `Authorization` header below is
    // present because its credential scope is what routes a request to a signing name and
    // names the Region, not to be verified.
    router.set_request_authentication(false);
    router.register_service(owner_sns::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(tagging_api::EmulatedService::new(engine));
    router
}

/// The `Authorization` header that routes a request to `signing_name` in `region`.
fn authorization(region: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One tagging-API operation in `region`, over the awsJson wire this API publishes.
async fn tagging(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let target = format!("ResourceGroupsTaggingAPI_20170126.{operation}");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", &target)
        .header("authorization", authorization(region, TAGGING))
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

/// One tagging-API operation in `region` that must answer 200.
async fn tagging_ok(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, parsed) = tagging(router, region, operation, body.clone()).await;
    assert_eq!(
        status, 200,
        "{operation} in {region} with {body} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Percent-encode one form component, so an ARN crosses the wire intact. Escaping every
/// non-alphanumeric byte is always a valid form encoding.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(char::from(*byte)),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Encode one form component the way an AWS SDK's own form wire does: urlencode leaves the
/// unreserved characters literal, writes a SPACE as `+`, and percent-escapes every other byte
/// — so a value's space reaches the service as `+` rather than as `%20`.
fn plus_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(*byte))
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// One Amazon SNS operation in `region`, with each form component encoded by `component`, so a
/// suite can post the body an SDK's own encoder would build as well as the fully escaped one.
async fn sns_encoded(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
    component: fn(&str) -> String,
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={SNS_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&component(name));
        body.push('=');
        body.push_str(&component(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(region, SNS))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon SNS operation in `region`, over the awsQuery wire that model publishes.
async fn sns(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    sns_encoded(router, region, action, params, encode).await
}

/// One Amazon SNS operation in `region` that must succeed.
async fn sns_ok(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = sns(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {action}{params:?} in {region}: expected 200 from Amazon SNS, got {status} with \
         body {body}"
    );
    body
}

/// The character data of the first element named `tag` of an awsQuery answer.
fn element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

/// The character data of the first element named `tag`, or a panic naming the body.
fn require(xml: &str, tag: &str) -> String {
    element(xml, tag).unwrap_or_else(|| panic!("no <{tag}> in {xml}"))
}

/// `Tags.member.N.Key` / `.Value` fields for an awsQuery tag list.
fn tag_params(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        fields.push((format!("Tags.member.{position}.Key"), (*key).to_string()));
        fields.push((format!("Tags.member.{position}.Value"), (*value).to_string()));
    }
    fields
}

/// `fields` as the borrowed pairs [`sns`] takes.
fn borrowed(fields: &[(String, String)]) -> Vec<(&str, &str)> {
    fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// A topic created through Amazon SNS's own `CreateTopic`, carrying `tags`, with the ARN that
/// service minted for it.
async fn topic(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut fields: Vec<(String, String)> = vec![("Name".to_string(), name.to_string())];
    if name.ends_with(".fifo") {
        // "the fifo suffix belongs to a FIFO topic and to no other": the name and the
        // `FifoTopic` attribute have to agree, so the attribute is read off the name.
        fields.push(("Attributes.entry.1.key".to_string(), "FifoTopic".to_string()));
        fields.push(("Attributes.entry.1.value".to_string(), "true".to_string()));
    }
    fields.extend(tag_params(tags));
    let created = sns_ok(router, region, "CreateTopic", &borrowed(&fields)).await;
    require(&created, "TopicArn")
}

/// Write `tags` to `arn` through Amazon SNS's own tag plane.
async fn sns_tag(router: &MultiServiceRouter, region: &str, arn: &str, tags: &[(&str, &str)]) {
    let mut fields: Vec<(String, String)> = vec![("ResourceArn".to_string(), arn.to_string())];
    fields.extend(tag_params(tags));
    let _ = sns_ok(router, region, "TagResource", &borrowed(&fields)).await;
}

/// Remove `keys` from `arn` through Amazon SNS's own tag plane.
async fn sns_untag(router: &MultiServiceRouter, region: &str, arn: &str, keys: &[&str]) {
    let mut fields: Vec<(String, String)> = vec![("ResourceArn".to_string(), arn.to_string())];
    for (index, key) in keys.iter().enumerate() {
        let position = index + 1;
        fields.push((format!("TagKeys.member.{position}"), (*key).to_string()));
    }
    let _ = sns_ok(router, region, "UntagResource", &borrowed(&fields)).await;
}

/// The tag map Amazon SNS's OWN `ListTagsForResource` reports for one ARN — the owning
/// service's own reader, so what it holds is read where it is held rather than only through
/// the index.
///
/// A list member's open tag carries the envelope's default namespace
/// (`<member xmlns="…">`), so an opening is matched up to its first `>`.
async fn sns_tags(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
) -> BTreeMap<String, String> {
    let body = sns_ok(
        router,
        region,
        "ListTagsForResource",
        &[("ResourceArn", arn)],
    )
    .await;
    let Some(block) = element(&body, "Tags") else {
        return BTreeMap::new();
    };
    let mut found = BTreeMap::new();
    let mut rest = block.as_str();
    while let Some(start) = rest.find("<member") {
        rest = &rest[start..];
        let Some(open) = rest.find('>') else { break };
        rest = &rest[open + 1..];
        let Some(end) = rest.find("</member>") else {
            break;
        };
        let member = &rest[..end];
        if let Some(key) = element(member, "Key") {
            found.insert(key, element(member, "Value").unwrap_or_default());
        }
        rest = &rest[end + "</member>".len()..];
    }
    found
}

/// The `(ARN -> tags)` map one `GetResources` page reports.
fn reported(body: &Value) -> BTreeMap<String, BTreeMap<String, String>> {
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

/// The ARNs one `GetResources` request selects in `region`, in ARN order.
async fn selected(router: &MultiServiceRouter, region: &str, request: Value) -> Vec<String> {
    let body = tagging_ok(router, region, "GetResources", request).await;
    let mut arns: Vec<String> = reported(&body).into_keys().collect();
    arns.sort();
    arns
}

/// Look again until `settled` answers `Ok`, or [`POLL_DEADLINE`] passes.
///
/// The loop waits for the CONDITION and reports the last answer it saw when it runs out of
/// patience, which is what names the failure.
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

/// One topic of the estate: how its tags got onto it, and the set it carries once both writes
/// have landed.
struct Row {
    case: &'static str,
    /// The topic's name, which its ARN's resource segment is.
    name: &'static str,
    /// The `Tags` its `CreateTopic` declares.
    seeded: &'static [(&'static str, &'static str)],
    /// The pairs a later SNS `TagResource` writes.
    written: &'static [(&'static str, &'static str)],
    /// The set the topic carries afterwards, by the rule `TagResource` states ("Add tags to
    /// the specified Amazon SNS topic"; a key already present takes the new value), computed
    /// from the declarations rather than read back out of an answer.
    settled: &'static [(&'static str, &'static str)],
}

/// The estate of [`REGION`]: a topic tagged by its own create (the case the issue cites), one
/// tagged only afterwards, one whose create's value a later write overwrites beside a key it
/// empties, a three-key topic, a FIFO topic, and a topic nothing ever tagged.
const ROWS: &[Row] = &[
    Row {
        case: "a topic tagged by its own create",
        name: "sns-index-created-tagged",
        seeded: &[(KEY, "true")],
        written: &[],
        settled: &[(KEY, "true")],
    },
    Row {
        case: "a topic tagged only after the fact",
        name: "sns-index-tagged-later",
        seeded: &[],
        written: &[(KEY, "later")],
        settled: &[(KEY, "later")],
    },
    Row {
        case: "a write that overwrites a seeded key and empties another",
        name: "sns-index-overwritten",
        seeded: &[(KEY, "true"), ("Owner", "platform")],
        written: &[(KEY, "overwritten"), ("Owner", "")],
        settled: &[("Owner", ""), (KEY, "overwritten")],
    },
    Row {
        case: "a topic carrying three keys",
        name: "sns-index-three-keys",
        seeded: &[(KEY, "true"), ("Team", "payments")],
        written: &[("Tier", "gold")],
        settled: &[(KEY, "true"), ("Team", "payments"), ("Tier", "gold")],
    },
    Row {
        case: "a FIFO topic tagged by its own create",
        name: "sns-index-fifo.fifo",
        seeded: &[(KEY, "true")],
        written: &[],
        settled: &[(KEY, "true")],
    },
    Row {
        case: "a topic nothing ever tagged",
        name: "sns-index-untagged",
        seeded: &[],
        written: &[],
        settled: &[],
    },
];

/// The topic of [`OTHER_REGION`], keyed the same way as the estate of [`REGION`] so a Region
/// leak would show up as a selection.
const OTHER_REGION_TOPIC: &str = "sns-index-other-region";

/// The estate both rules are read over, with the ARNs Amazon SNS minted.
struct Estate {
    router: MultiServiceRouter,
    /// `case -> ARN`, for every row of [`ROWS`].
    arns: BTreeMap<&'static str, String>,
    /// The topic of [`OTHER_REGION`].
    other_region: String,
}

impl Estate {
    /// Create the estate through the two services' OWN APIs, and wait until the index reports
    /// what the writes left.
    async fn create() -> Self {
        let router = emulator();
        let mut arns: BTreeMap<&'static str, String> = BTreeMap::new();
        for row in ROWS {
            let arn = topic(&router, REGION, row.name, row.seeded).await;
            if !row.written.is_empty() {
                sns_tag(&router, REGION, &arn, row.written).await;
            }
            arns.insert(row.case, arn);
        }
        let other_region = topic(&router, OTHER_REGION, OTHER_REGION_TOPIC, &[(KEY, "true")]).await;

        let estate = Self {
            router,
            arns,
            other_region,
        };
        estate.wait_for_the_settled_estate().await;
        estate
    }

    /// The ARN of the row whose case is `case`.
    fn arn(&self, case: &str) -> String {
        self.arns
            .get(case)
            .unwrap_or_else(|| panic!("the estate holds no row {case:?}"))
            .clone()
    }

    /// Every tagged ARN of [`REGION`], in ARN order — the answer a query carrying no filter
    /// at all selects.
    fn tagged_arns(&self) -> Vec<String> {
        let mut arns: Vec<String> = ROWS
            .iter()
            .filter(|row| !row.settled.is_empty())
            .map(|row| self.arn(row.case))
            .collect();
        arns.sort();
        arns
    }

    /// The ARNs of the rows named by `cases`, in ARN order.
    fn arns_of(&self, cases: &[&str]) -> Vec<String> {
        let mut arns: Vec<String> = cases.iter().map(|case| self.arn(case)).collect();
        arns.sort();
        arns
    }

    /// Wait until the index reports every row's settled tag set, so a later read is taken on a
    /// settled estate.
    async fn wait_for_the_settled_estate(&self) {
        let router = &self.router;
        poll_until("the settled estate", "the tagging index", || async {
            let body = tagging_ok(router, REGION, "GetResources", json!({})).await;
            let index = reported(&body);
            for row in ROWS {
                let arn = self.arn(row.case);
                let expected: BTreeMap<String, String> = row
                    .settled
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect();
                match (index.get(&arn), expected.is_empty()) {
                    (None, true) => {}
                    (Some(tags), false) if *tags == expected => {}
                    _ => {
                        return Err(format!(
                            "{arn} ({}) is reported as {:?}, expected {expected:?}: {body}",
                            row.case,
                            index.get(&arn)
                        ))
                    }
                }
            }
            Ok(())
        })
        .await;
    }
}

/// THE DISCOVERY RULE: every topic the owning service holds a tag for is a row of the index,
/// carrying exactly the tag set that service holds, selected by the tag key — and an untagged
/// topic and another Region's topic are in no answer.
#[tokio::test]
async fn every_tagged_sns_topic_is_a_row_of_the_tagging_index() {
    let estate = Estate::create().await;

    let body = tagging_ok(
        &estate.router,
        REGION,
        "GetResources",
        json!({ "TagFilters": [{ "Key": KEY }] }),
    )
    .await;
    let index = reported(&body);

    for row in ROWS {
        let arn = estate.arn(row.case);
        let expected: BTreeMap<String, String> = row
            .settled
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        if expected.is_empty() {
            assert!(
                !index.contains_key(&arn),
                "case {:?}: GetResources must not report the untagged {arn}, got {index:?}",
                row.case
            );
            continue;
        }
        let reported_tags = index.get(&arn).unwrap_or_else(|| {
            panic!(
                "case {:?}: the index must report {arn}, got {index:?}",
                row.case
            )
        });
        assert_eq!(
            reported_tags, &expected,
            "case {:?}: expected {arn} to carry {expected:?}, got {reported_tags:?}",
            row.case
        );
    }

    assert!(
        !index.contains_key(&estate.other_region),
        "case {:?}: a topic of {OTHER_REGION} is in no answer of {REGION}, got {index:?}",
        "another Region's topic"
    );

    // And that Region's own endpoint reports it, so the row exists and only the scope of the
    // answer keeps it out of the one above.
    let elsewhere = selected(
        &estate.router,
        OTHER_REGION,
        json!({ "TagFilters": [{ "Key": KEY }] }),
    )
    .await;
    assert_eq!(
        elsewhere,
        vec![estate.other_region.clone()],
        "case {:?}: expected {OTHER_REGION} to report exactly its own topic, got {elsewhere:?}",
        "another Region's endpoint"
    );
}

/// THE DISCOVERY RULE's selection: each filter form selects exactly the topics it names, over
/// the one estate.
#[tokio::test]
async fn the_filters_select_the_topics_they_name() {
    let estate = Estate::create().await;
    let every = estate.tagged_arns();
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        ("no filter at all", json!({}), every.clone()),
        (
            "the tag key with no value",
            json!({ "TagFilters": [{ "Key": KEY }] }),
            every.clone(),
        ),
        (
            "the tag key with the value the create seeded",
            json!({ "TagFilters": [{ "Key": KEY, "Values": ["true"] }] }),
            estate.arns_of(&[
                "a topic tagged by its own create",
                "a topic carrying three keys",
                "a FIFO topic tagged by its own create",
            ]),
        ),
        (
            "the tag key with a value no topic carries",
            json!({ "TagFilters": [{ "Key": KEY, "Values": ["never-applied"] }] }),
            Vec::new(),
        ),
        (
            "the tag key with two values",
            json!({ "TagFilters": [{ "Key": KEY, "Values": ["later", "overwritten"] }] }),
            estate.arns_of(&[
                "a topic tagged only after the fact",
                "a write that overwrites a seeded key and empties another",
            ]),
        ),
        (
            "two filters, both of which one topic carries",
            json!({ "TagFilters": [
                { "Key": KEY, "Values": ["true"] },
                { "Key": "Team", "Values": ["payments"] },
            ] }),
            estate.arns_of(&["a topic carrying three keys"]),
        ),
        (
            "two filters, one of which nothing carries",
            json!({ "TagFilters": [{ "Key": KEY }, { "Key": "no-such-key" }] }),
            Vec::new(),
        ),
        (
            "a key whose value is empty",
            json!({ "TagFilters": [{ "Key": "Owner", "Values": [""] }] }),
            estate.arns_of(&["a write that overwrites a seeded key and empties another"]),
        ),
        (
            "the bare service token",
            json!({ "ResourceTypeFilters": [BARE_TYPE] }),
            every.clone(),
        ),
        (
            "the qualified resource type",
            json!({ "ResourceTypeFilters": [TOPIC_TYPE] }),
            every.clone(),
        ),
        (
            "a resource type nothing of the estate answers to",
            json!({ "ResourceTypeFilters": ["sqs:queue"] }),
            Vec::new(),
        ),
        (
            "the resource type narrowed by a tag value",
            json!({
                "ResourceTypeFilters": [TOPIC_TYPE],
                "TagFilters": [{ "Key": KEY, "Values": ["later"] }],
            }),
            estate.arns_of(&["a topic tagged only after the fact"]),
        ),
        (
            "the ARN list naming two topics",
            json!({ "ResourceARNList": [
                estate.arn("a topic tagged by its own create"),
                estate.arn("a topic carrying three keys"),
            ] }),
            estate.arns_of(&[
                "a topic tagged by its own create",
                "a topic carrying three keys",
            ]),
        ),
        (
            "the ARN list naming the untagged topic",
            json!({ "ResourceARNList": [estate.arn("a topic nothing ever tagged")] }),
            Vec::new(),
        ),
    ];

    for (case, request, expected) in rows {
        let answer = selected(&estate.router, REGION, request.clone()).await;
        assert_eq!(
            answer, expected,
            "case {case:?}: expected GetResources {request} to select {expected:?}, got \
             {answer:?}"
        );
    }
}

/// One removal of the withdrawal table: the case, the row of [`ROWS`] to untag, the keys the
/// untag names, and the tag set that topic is left with — empty where it holds no tag
/// afterwards and is therefore in no answer at all.
type Removal = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
);

/// THE DISCOVERY RULE's withdrawal: SNS's own `UntagResource` takes the key off the topic, so
/// the query that selected it stops selecting it — and a topic left with no tag at all leaves
/// the index.
#[tokio::test]
async fn an_untag_through_amazon_sns_withdraws_the_topic_from_the_index() {
    let estate = Estate::create().await;

    let removals: &[Removal] = &[
        (
            "the only key of a topic tagged by its create",
            "a topic tagged by its own create",
            &[KEY],
            &[],
        ),
        (
            "one key of three",
            "a topic carrying three keys",
            &["Team"],
            &[(KEY, "true"), ("Tier", "gold")],
        ),
        (
            "the key whose value is empty",
            "a write that overwrites a seeded key and empties another",
            &["Owner"],
            &[(KEY, "overwritten")],
        ),
        (
            "a key the topic does not carry",
            "a topic tagged only after the fact",
            &["no-such-key"],
            &[(KEY, "later")],
        ),
    ];

    let router = &estate.router;
    for (case, row, keys, left) in removals {
        let arn = estate.arn(row);
        sns_untag(router, REGION, &arn, keys).await;

        let expected: BTreeMap<String, String> = left
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        let wanted = expected.clone();
        let watched = arn.clone();
        poll_until(case, "the withdrawal", || async {
            let body = tagging_ok(router, REGION, "GetResources", json!({})).await;
            let index = reported(&body);
            match (index.get(&watched), wanted.is_empty()) {
                (None, true) => Ok(()),
                (Some(tags), false) if *tags == wanted => Ok(()),
                _ => Err(format!(
                    "{watched} is reported as {:?}, expected {wanted:?}",
                    index.get(&watched)
                )),
            }
        })
        .await;

        // And the key-only query no longer selects a topic the untag emptied.
        let by_key = selected(
            router,
            REGION,
            json!({ "TagFilters": [{ "Key": KEY }] }),
        )
        .await;
        assert_eq!(
            by_key.contains(&arn),
            expected.contains_key(KEY),
            "case {case:?}: expected the {KEY} query to select {arn} only while it carries \
             that key, got {by_key:?}"
        );
    }
}

/// THE DISCOVERY RULE on the emulator as it is ASSEMBLED: every service mounted on ONE
/// engine, the way `aws_emulator_router::build_router` builds the one the binary serves, so
/// the projection is read with every owning service in the engine beside Amazon SNS.
///
/// The rows span the Region the estate lives in — the one this emulator falls back to when a
/// request carries no credential scope, and another one, so a Region read off the request
/// rather than that fallback is what the answer is scoped by — and the spelling of the tag
/// value, including the characters an AWS tag value admits beside letters and digits
/// (`+ - = . _ : / @` and the space, per the tag-restrictions reference), which a value
/// crossing Amazon SNS's form-encoded wire and the tagging API's JSON wire has to survive
/// identically for a filter to match it.
#[tokio::test]
async fn the_assembled_emulator_discovers_a_topic_by_its_tag() {
    let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
    // As above: the suite exercises handler behaviour, not SigV4.
    router.set_request_authentication(false);

    // Columns: the case, the topic's name, and the tag its create declares.
    let rows: &[(&str, &str, (&str, &str))] = &[
        (
            "a topic tagged by its own create",
            "sns-assembled-created-tagged",
            (KEY, "assembled"),
        ),
        (
            "a topic whose tag key is its own",
            "sns-assembled-other-key",
            ("Owner", "platform"),
        ),
        (
            "a value carrying the punctuation a tag value admits",
            "sns-assembled-punctuated",
            (KEY, "a b+c=d.e_f:g/h@i-j"),
        ),
        (
            "a value that is a timestamp",
            "sns-assembled-timestamped",
            (KEY, "2026-11-17T14:53:04Z"),
        ),
        (
            "a key carrying that punctuation too",
            "sns-assembled-punctuated-key",
            ("owner:team/sub_unit.name", "payments"),
        ),
    ];

    for region in [REGION, OTHER_REGION] {
        for (case, name, (key, value)) in rows {
            let case = format!("{case} in {region}");
            let arn = topic(&router, region, name, &[(*key, *value)]).await;
            let by_key_and_value = selected(
                &router,
                region,
                json!({ "TagFilters": [{ "Key": key, "Values": [value] }] }),
            )
            .await;
            assert_eq!(
                by_key_and_value,
                vec![arn.clone()],
                "case {case:?}: expected the assembled emulator to select {arn} by \
                 {key}={value}, got {by_key_and_value:?}"
            );

            let by_another_value = selected(
                &router,
                region,
                json!({ "TagFilters": [{ "Key": key, "Values": ["never-applied"] }] }),
            )
            .await;
            assert!(
                !by_another_value.contains(&arn),
                "case {case:?}: a value {arn} never carried must not select it, got \
                 {by_another_value:?}"
            );

            sns_untag(&router, region, &arn, &[key]).await;
            let after_untag = selected(
                &router,
                region,
                json!({ "TagFilters": [{ "Key": key, "Values": [value] }] }),
            )
            .await;
            assert!(
                !after_untag.contains(&arn),
                "case {case:?}: after UntagResource({key}) the query must stop selecting \
                 {arn}, got {after_untag:?}"
            );
        }
    }
}

/// THE DISCOVERY RULE across the FORM wire's two spellings of one value: an AWS SDK's own
/// form encoder leaves the unreserved characters literal and writes a SPACE as `+`
/// (`urlencode`'s quote-plus), while a fully escaped body writes it as `%20` — both are the
/// same value, so the tag Amazon SNS stores, the tag its own reader reports and the tag the
/// index matches a `TagFilters` value against have to be the one the client sent either way.
///
/// The rows span the characters an AWS tag admits beside letters and digits — the space,
/// `+ - = . _ : / @`, and non-ASCII text ("Tag values accept a maximum of 256 Unicode
/// characters", the tag-restrictions reference) — because each one is spelled differently by
/// the two encoders, and a value only matches a filter if both wires arrive at the same bytes.
#[tokio::test]
async fn a_tag_value_crosses_the_form_wire_in_either_spelling() {
    let router = emulator();

    // Columns: the case, the tag key, and the tag value the client means.
    let rows: &[(&str, &str, &str)] = &[
        ("a value carrying a space", KEY, "value one"),
        ("a value carrying two spaces", KEY, "a b c"),
        ("a value that is a plus sign", KEY, "a+b"),
        (
            "a value carrying the punctuation a tag value admits",
            KEY,
            "a b+c=d.e_f:g/h@i-j",
        ),
        ("a non-ASCII value", KEY, "café-日本"),
        ("a key carrying a space", "owner team", "payments"),
        ("a non-ASCII key", "équipe", "payments"),
    ];

    for (index, (case, key, value)) in rows.iter().enumerate() {
        // The same tag written twice, once through each encoder, each onto its own topic, so
        // the two spellings are read back side by side.
        for (spelling, component) in [
            ("the SDK's quote-plus encoder", plus_encode as fn(&str) -> String),
            ("a fully escaped body", encode as fn(&str) -> String),
        ] {
            let case = format!("{case} through {spelling}");
            let name = format!("sns-form-wire-{index}-{}", spelling.len());
            let mut fields: Vec<(String, String)> =
                vec![("Name".to_string(), name.clone())];
            fields.extend(tag_params(&[(*key, *value)]));
            let (status, created) = sns_encoded(
                &router,
                REGION,
                "CreateTopic",
                &borrowed(&fields),
                component,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateTopic({name}) must answer 200, got {status} with {created}"
            );
            let arn = require(&created, "TopicArn");

            // The owning service's own reader: the tag it holds is the one the client sent.
            let held = sns_tags(&router, REGION, &arn).await;
            let expected: BTreeMap<String, String> =
                [((*key).to_string(), (*value).to_string())]
                    .into_iter()
                    .collect();
            assert_eq!(
                held, expected,
                "case {case:?}: expected Amazon SNS to hold {expected:?} for {arn}, got {held:?}"
            );

            // And the index matches a filter naming that key and value.
            let found = selected(
                &router,
                REGION,
                json!({ "TagFilters": [{ "Key": key, "Values": [value] }] }),
            )
            .await;
            assert!(
                found.contains(&arn),
                "case {case:?}: expected the index to select {arn} by {key}={value}, got {found:?}"
            );
        }
    }
}

/// THE DISCOVERY RULE on a STARTED emulator: the binary's own process, reached over real
/// HTTPS with SigV4-signed calls — Amazon SNS's form wire writing the tag and the tagging
/// API's JSON wire reading it back — because the two wires carry a tag key and value through
/// different encodings and the index only matches a filter if both arrive at the same bytes.
#[test]
fn a_started_emulator_discovers_a_topic_by_its_tag() {
    let emulator = Started::start();

    // Columns: the case, the topic's name, and the tag its create declares — the value
    // spellings span the characters an AWS tag value admits beside letters and digits
    // (`+ - = . _ : / @` and the space, per the tag-restrictions reference), which Amazon SNS's
    // form wire percent-encodes and the tagging API's JSON wire does not.
    let rows: &[(&str, &str, (&str, &str))] = &[
        (
            "a topic tagged by its own create",
            "sns-started-created-tagged",
            (KEY, "started"),
        ),
        (
            "a value carrying the punctuation a tag value admits",
            "sns-started-punctuated",
            (KEY, "a b+c=d.e_f:g/h@i-j"),
        ),
        (
            "a key carrying that punctuation too",
            "sns-started-punctuated-key",
            ("owner:team/sub_unit.name", "payments"),
        ),
    ];

    let authority = emulator.authority();
    for (case, name, (key, value)) in rows {
        // Both addressings a client can be in: the service host an SDK derives from the
        // Region, and the published `host:port` a client handed an endpoint url carries — the
        // only value such a client has, which is how a client configured with this emulator's
        // endpoint reaches it. Either way the connection is made to this emulator.
        for addressing in ["the service host", "the published endpoint"] {
            let case = format!("{case} addressed by {addressing}");
            let name = format!("{name}-{}", addressing.len());
            let send = |signing_name: &str, headers: &[(&str, &str)], body: Vec<u8>| {
                if addressing == "the published endpoint" {
                    emulator
                        .call_at(&authority, signing_name, "POST", "/", headers, body)
                        .unwrap_or_else(|error| panic!("case {case:?}: {error}"))
                } else {
                    emulator.call(signing_name, "POST", "/", headers, body)
                }
            };

            let mut form = format!("Action=CreateTopic&Version={SNS_VERSION}");
            for (field, field_value) in [
                ("Name", name.as_str()),
                ("Tags.member.1.Key", key),
                ("Tags.member.1.Value", value),
            ] {
                form.push('&');
                form.push_str(&plus_encode(field));
                form.push('=');
                form.push_str(&plus_encode(field_value));
            }
            let (status, created) = send(
                "sns",
                &[("content-type", "application/x-www-form-urlencoded")],
                form.into_bytes(),
            );
            assert_eq!(
                status, 200,
                "case {case:?}: CreateTopic({name}) must answer 200, got {status} with \
                 {created}; the log:\n{}",
                emulator.log()
            );
            let arn = require(&created, "TopicArn");

            // The index is read again until it reports the topic, the way a client that has
            // just tagged a resource reads it: the wait is on the CONDITION.
            let giving_up_at = Instant::now() + POLL_DEADLINE;
            let request = json!({ "TagFilters": [{ "Key": key, "Values": [value] }] });
            loop {
                let (status, text) = send(
                    "tagging",
                    &[
                        ("content-type", "application/x-amz-json-1.1"),
                        (
                            "x-amz-target",
                            "ResourceGroupsTaggingAPI_20170126.GetResources",
                        ),
                    ],
                    request.to_string().into_bytes(),
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: GetResources {request} must answer 200, got {status} with \
                     {text}; the log:\n{}",
                    emulator.log()
                );
                let body: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
                    panic!("case {case:?}: GetResources answered no JSON ({error}): {text}")
                });
                let index = reported(&body);
                if index.contains_key(&arn) {
                    let tags = &index[&arn];
                    let expected: BTreeMap<String, String> =
                        [((*key).to_string(), (*value).to_string())]
                            .into_iter()
                            .collect();
                    assert_eq!(
                        tags, &expected,
                        "case {case:?}: expected {arn} to carry {expected:?}, got {tags:?}"
                    );
                    break;
                }
                assert!(
                    Instant::now() < giving_up_at,
                    "case {case:?}: the started emulator never selected {arn} by \
                     {key}={value}; the last answer was {text}; the log:\n{}",
                    emulator.log()
                );
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

/// THE DISCOVERY RULE over the PROTOCOL the request is framed in: a `GetResources` carried as
/// Smithy RPC v2 CBOR (its operation in the URI path, its input a CBOR map) selects the same
/// resources as the same request carried as awsJson — a filter is the filter the caller sent
/// whichever framing carried it, so a list of structures (`TagFilters`) is read exactly as a
/// list of strings (`ResourceARNList`) is.
///
/// Nothing here decodes a CBOR answer: a mapping's `ResourceARN` is a CBOR text string, so the
/// ARN's own bytes are in the body when the resource was selected and absent when it was not.
#[tokio::test]
async fn a_cbor_framed_request_selects_what_its_filters_name() {
    let router = emulator();
    let arn = topic(&router, REGION, "sns-cbor-framed-one", &[(KEY, "cbor")]).await;
    let other = topic(&router, REGION, "sns-cbor-framed-two", &[(KEY, "other")]).await;

    // Columns: the case, the CBOR input, and whether the topic is selected by it.
    let rows: &[(&str, Vec<u8>, bool)] = &[
        (
            "the tag key and its value",
            cbor_tag_filter(KEY, Some("cbor")),
            true,
        ),
        ("the tag key alone", cbor_tag_filter(KEY, None), true),
        (
            "a value the topic never carried",
            cbor_tag_filter(KEY, Some("never-applied")),
            false,
        ),
        (
            "a key nothing carries",
            cbor_tag_filter("no-such-key", None),
            false,
        ),
        ("the ARN list naming the topic", cbor_arn_list(&arn), true),
        ("the ARN list naming the other topic", cbor_arn_list(&other), false),
    ];

    for (case, input, selects) in rows {
        let request = http::Request::builder()
            .method("POST")
            .uri("/service/ResourceGroupsTaggingAPI_20170126/operation/GetResources")
            .header("host", format!("tagging.{REGION}.amazonaws.com"))
            .header("content-type", "application/cbor")
            .header("accept", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor")
            .header("authorization", authorization(REGION, TAGGING))
            .body(Bytes::from(input.clone()))
            .expect("a well-formed request");
        let response = router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let rendered = String::from_utf8_lossy(&body).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: a CBOR GetResources must answer 200, got {status} with {rendered:?}"
        );
        let holds = body
            .windows(arn.len())
            .any(|window| window == arn.as_bytes());
        assert_eq!(
            holds, *selects,
            "case {case:?}: expected the CBOR answer to {} {arn}, got {rendered:?}",
            if *selects { "carry" } else { "leave out" }
        );
    }
}

/// The CBOR input of a `GetResources` whose one `TagFilters` entry names `key`, with `value`
/// when the filter carries one — a list of STRUCTURES, the member shape a framing has to read
/// as faithfully as it reads a list of strings.
fn cbor_tag_filter(key: &str, value: Option<&str>) -> Vec<u8> {
    let mut body = Vec::new();
    cbor_head(&mut body, 5, 1);
    cbor_text(&mut body, "TagFilters");
    cbor_head(&mut body, 4, 1);
    cbor_head(&mut body, 5, if value.is_some() { 2 } else { 1 });
    cbor_text(&mut body, "Key");
    cbor_text(&mut body, key);
    if let Some(value) = value {
        cbor_text(&mut body, "Values");
        cbor_head(&mut body, 4, 1);
        cbor_text(&mut body, value);
    }
    body
}

/// The CBOR input of a `GetResources` whose `ResourceARNList` names `arn` — the list of
/// strings the case above is read against.
fn cbor_arn_list(arn: &str) -> Vec<u8> {
    let mut body = Vec::new();
    cbor_head(&mut body, 5, 1);
    cbor_text(&mut body, "ResourceARNList");
    cbor_head(&mut body, 4, 1);
    cbor_text(&mut body, arn);
    body
}

/// A CBOR text string.
fn cbor_text(body: &mut Vec<u8>, text: &str) {
    cbor_head(body, 3, text.len() as u64);
    body.extend_from_slice(text.as_bytes());
}

/// The initial byte, and any length bytes after it, of a definite-length CBOR item of major
/// type `major` carrying the argument `value` — every width CBOR defines, so no length is
/// truncated into a head too narrow to hold it.
fn cbor_head(body: &mut Vec<u8>, major: u8, value: u64) {
    let major = major << 5;
    match value {
        0..=23 => body.push(major | value as u8),
        24..=0xFF => body.extend_from_slice(&[major | 24, value as u8]),
        0x100..=0xFFFF => {
            body.push(major | 25);
            body.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xFFFF_FFFF => {
            body.push(major | 26);
            body.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            body.push(major | 27);
            body.extend_from_slice(&value.to_be_bytes());
        }
    }
}

/// THE REFUSAL RULE: each member `ResourceARNList` is mutually exclusive with earns HTTP 400
/// `InvalidParameterException` whose sentence names THAT member.
#[tokio::test]
async fn each_prohibited_combination_names_the_member_it_refuses() {
    let estate = Estate::create().await;
    let arn = estate.arn("a topic tagged by its own create");

    // Columns: the case, the member combined with `ResourceARNList`, and the sentence AWS
    // serves for that combination.
    let rows: &[(&str, Value, &str)] = &[
        (
            "ResourceTypeFilters",
            json!({ "ResourceARNList": [&arn], "ResourceTypeFilters": [TOPIC_TYPE] }),
            "ResourceTypeFilters is not allowed when providing a ResourceARNList.",
        ),
        (
            "the bare service token in ResourceTypeFilters",
            json!({ "ResourceARNList": [&arn], "ResourceTypeFilters": [BARE_TYPE] }),
            "ResourceTypeFilters is not allowed when providing a ResourceARNList.",
        ),
        (
            "TagFilters",
            json!({ "ResourceARNList": [&arn], "TagFilters": [{ "Key": KEY }] }),
            "Filters is not allowed when providing a resource list.",
        ),
        (
            "TagFilters carrying a value",
            json!({
                "ResourceARNList": [&arn],
                "TagFilters": [{ "Key": KEY, "Values": ["true"] }],
            }),
            "Filters is not allowed when providing a resource list.",
        ),
        (
            "ResourcesPerPage",
            json!({ "ResourceARNList": [&arn], "ResourcesPerPage": 50 }),
            "ResourcesPerPage is not allowed when providing a ResourceARNList.",
        ),
        (
            "TagsPerPage",
            json!({ "ResourceARNList": [&arn], "TagsPerPage": 100 }),
            "TagsPerPage is not allowed when providing a ResourceARNList.",
        ),
        (
            "PaginationToken",
            json!({ "ResourceARNList": [&arn], "PaginationToken": "not-a-token" }),
            "PaginationToken is not allowed when providing a ResourceARNList.",
        ),
        (
            // Several prohibited members at once: the sentence names the FIRST one the
            // documented prohibition lists, so one request earns one answer.
            "ResourceTypeFilters beside TagFilters and a page size",
            json!({
                "ResourceARNList": [&arn],
                "ResourceTypeFilters": [TOPIC_TYPE],
                "TagFilters": [{ "Key": KEY }],
                "ResourcesPerPage": 10,
            }),
            "ResourceTypeFilters is not allowed when providing a ResourceARNList.",
        ),
    ];

    for (case, request, message) in rows {
        let (status, body) = tagging(&estate.router, REGION, "GetResources", request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {request}, got {status} with {body}"
        );
        let code = body["__type"].as_str().unwrap_or_default();
        assert!(
            code.ends_with("InvalidParameterException"),
            "case {case:?}: expected InvalidParameterException, got {body}"
        );
        assert_eq!(
            body["Message"].as_str(),
            Some(*message),
            "case {case:?}: expected the message {message:?}, got {body}"
        );
    }

    // The ARN list ALONE still answers the topic, so the refusals above are about the
    // combination and not about the list.
    let alone = selected(
        &estate.router,
        REGION,
        json!({ "ResourceARNList": [&arn] }),
    )
    .await;
    assert_eq!(
        alone,
        vec![arn.clone()],
        "case {:?}: expected the ARN list alone to select {arn}, got {alone:?}",
        "the ARN list alone"
    );
}
