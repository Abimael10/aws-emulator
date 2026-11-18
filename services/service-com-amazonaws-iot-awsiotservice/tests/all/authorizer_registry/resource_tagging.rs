// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the AWS IoT RESOURCE TAGGING plane: `TagResource`,
//! `UntagResource` and the `ListTagsForResource` reader that proves what they wrote.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a tag is metadata OF A RESOURCE, stored with the resource its
//! `resourceArn` names. `TagResource` adds or modifies the keys its request carries and
//! leaves every other key alone, `UntagResource` removes exactly the keys its request
//! names, `ListTagsForResource` reports what is stored — including the tags the resource's
//! CREATE carried, because they are the same tag set. An ARN naming no resource this
//! account holds has nothing for a WRITE to change, so both writes refuse it
//! `ResourceNotFoundException` (404) with that resource family's own sentence instead of
//! a success, while the READ serves it: it answers the tags held for the ARN presented,
//! and nothing is held for an ARN no resource answers to. A `resourceArn` that addresses
//! no resource of this service's namespace at all is a request to be fixed, not a resource
//! that is missing: `InvalidRequestException` (400), before any lookup. A write that would
//! leave a resource carrying more than `TagMap`'s modelled maximum of tags is refused
//! `LimitExceededException` (410) and changes nothing.
//!
//! The table spans what that rule generalises over: the resource FAMILY the ARN names
//! (nine of them, each with its own store, its own identifier form and its own not-found
//! sentence), the shape its create takes tags in (a `TagList` whose `Tag$Value` is
//! optional, the software-package family's `TagMap`, and the raw
//! `key1=value1&key2=value2` query string `CreateTopicRule` takes), whether a tag carries
//! a value at all, whether a key is added or modified, and whether an untagged key was
//! ever set.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MAX_RESOURCE_TAGS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// The one query-value percent-encoder this test crate has: `ListTagsForResource` binds
// its `resourceArn` to the query string, so an ARN's separators travel encoded exactly
// as an SDK client sends them.
use crate::describe_endpoint::percent_encode;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "us-east-1";

/// A policy document `CreatePolicy` accepts, so the policy family's row can be created.
const POLICY_DOCUMENT: &str =
    "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
     \"Action\":\"iot:Connect\",\"Resource\":\"*\"}]}";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// `REGION`: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` an SDK surfaces for a refusal.
    fn message(&self) -> Option<&str> {
        self.body.get("message").and_then(Value::as_str)
    }

    /// The reported tag set, keyed by `Tag$Key`, or `None` when the response carries no
    /// `tags` member at all.
    fn tags(&self) -> Option<BTreeMap<String, Option<String>>> {
        let reported = self.body.get("tags")?.as_array()?;
        Some(
            reported
                .iter()
                .map(|tag| {
                    (
                        tag.get("Key")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        tag.get("Value")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    )
                })
                .collect(),
        )
    }
}

/// One request: the method, the path, the body and the `x-amz-tagging` header a
/// `CreateTopicRule` carries its tags in.
struct Call {
    method: &'static str,
    path: String,
    body: Option<Value>,
    tagging: Option<&'static str>,
}

impl Call {
    fn new(method: &'static str, path: String, body: Option<Value>) -> Self {
        Self {
            method,
            path,
            body,
            tagging: None,
        }
    }

    fn tagged(mut self, tagging: &'static str) -> Self {
        self.tagging = Some(tagging);
        self
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn send(&self, call: &Call) -> Reply {
        let payload = call.body.as_ref().map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let mut builder = http::Request::builder()
            .method(call.method)
            .uri(format!(
                "https://iot.{REGION}.amazonaws.com{path}",
                path = call.path
            ))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json");
        if let Some(tagging) = call.tagging {
            builder = builder.header("x-amz-tagging", tagging);
        }
        let request = builder
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// Run a family's setup calls, asserting each one succeeded.
    async fn create(&self, label: &str, setup: &[Call]) {
        for call in setup {
            let reply = self.send(call).await;
            assert_eq!(
                reply.status, 200,
                "case {label:?}: setup {} {} expected 200, got {reply:?}",
                call.method, call.path
            );
        }
    }

    async fn tag(&self, resource_arn: &str, tags: &[(&str, Option<&str>)]) -> Reply {
        let entries: Vec<Value> = tags
            .iter()
            .map(|(key, value)| match value {
                Some(value) => json!({ "Key": key, "Value": value }),
                None => json!({ "Key": key }),
            })
            .collect();
        self.send(&Call::new(
            "POST",
            "/tags".to_string(),
            Some(json!({ "resourceArn": resource_arn, "tags": entries })),
        ))
        .await
    }

    async fn untag(&self, resource_arn: &str, tag_keys: &[&str]) -> Reply {
        self.send(&Call::new(
            "POST",
            "/untag".to_string(),
            Some(json!({ "resourceArn": resource_arn, "tagKeys": tag_keys })),
        ))
        .await
    }

    async fn list_tags(&self, resource_arn: &str) -> Reply {
        self.send(&Call::new(
            "GET",
            format!("/tags?resourceArn={}", percent_encode(resource_arn)),
            None,
        ))
        .await
    }
}

/// One resource family the tag plane addresses by ARN.
struct Family {
    label: &'static str,
    /// The calls that bring the resource into existence; the last one creates it.
    setup: Vec<Call>,
    /// The ARN the tag plane addresses that resource by — the one its create reported.
    arn: String,
    /// The tags its CREATE recorded, as the tag read must report them.
    created: Vec<(&'static str, Option<&'static str>)>,
    /// What this family reports for a tag written with NO value: absent for a family
    /// whose create takes a `TagList` (`Tag$Value` is optional), and the empty value for
    /// the software-package family, whose `TagMap` gives every key a value.
    absent_value: Option<&'static str>,
    /// The sentence this family refuses a lookup miss with.
    missing: String,
}

/// An ARN of this account's IoT namespace over `resource`.
fn iot_arn(resource: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:{resource}")
}

/// The families under test, each with the create that records it, the ARN it is addressed
/// by, and the not-found sentence its own readers state.
///
/// Nine families, chosen so every tag STORAGE shape this service holds is covered: the
/// `TagList` families (thing group, thing type, policy, authorizer, command, role alias),
/// the software-package family's `TagMap` (a package and one of its versions, whose
/// identity is the pair), and the topic-rule family, whose create carries its tags as the
/// raw `key1=value1&key2=value2` query string.
fn families() -> Vec<Family> {
    vec![
        Family {
            label: "thing group",
            setup: vec![Call::new(
                "POST",
                "/thing-groups/fleet-alpha".to_string(),
                Some(json!({ "tags": [{ "Key": "stage", "Value": "test" }] })),
            )],
            arn: iot_arn("thinggroup/fleet-alpha"),
            created: vec![("stage", Some("test"))],
            absent_value: None,
            missing: format!("ThingGroup {ACCOUNT_ID}:fleet-alpha not found."),
        },
        Family {
            label: "thing type",
            setup: vec![Call::new(
                "POST",
                "/thing-types/sensor-type".to_string(),
                Some(json!({ "tags": [{ "Key": "stage", "Value": "test" }] })),
            )],
            arn: iot_arn("thingtype/sensor-type"),
            created: vec![("stage", Some("test"))],
            absent_value: None,
            missing: format!("Thing Type {ACCOUNT_ID}:sensor-type not found."),
        },
        Family {
            label: "policy, created with a VALUELESS tag",
            setup: vec![Call::new(
                "POST",
                "/policies/publish-policy".to_string(),
                Some(json!({
                    "policyDocument": POLICY_DOCUMENT,
                    "tags": [{ "Key": "unvalued" }],
                })),
            )],
            arn: iot_arn("policy/publish-policy"),
            created: vec![("unvalued", None)],
            absent_value: None,
            missing: "Policy publish-policy does not exist".to_string(),
        },
        Family {
            label: "authorizer",
            setup: vec![Call::new(
                "POST",
                "/authorizer/gatekeeper".to_string(),
                Some(json!({
                    "authorizerFunctionArn":
                        format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:auth"),
                    "tags": [{ "Key": "stage", "Value": "test" }],
                })),
            )],
            arn: iot_arn("authorizer/gatekeeper"),
            created: vec![("stage", Some("test"))],
            absent_value: None,
            missing: "Authorizer gatekeeper not found".to_string(),
        },
        Family {
            label: "command, created with NO tags",
            setup: vec![Call::new("PUT", "/commands/reboot".to_string(), Some(json!({})))],
            arn: iot_arn("command/reboot"),
            created: vec![],
            absent_value: None,
            missing: "Command reboot cannot be found.".to_string(),
        },
        Family {
            label: "role alias",
            setup: vec![Call::new(
                "POST",
                "/role-aliases/device-credentials".to_string(),
                Some(json!({
                    "roleArn": format!("arn:aws:iam::{ACCOUNT_ID}:role/device"),
                    "tags": [{ "Key": "stage", "Value": "test" }],
                })),
            )],
            arn: iot_arn("rolealias/device-credentials"),
            created: vec![("stage", Some("test"))],
            absent_value: None,
            missing: "Role alias device-credentials not found".to_string(),
        },
        Family {
            label: "software package, whose create takes a TagMap",
            setup: vec![Call::new(
                "PUT",
                "/packages/firmware".to_string(),
                Some(json!({ "tags": { "stage": "test" } })),
            )],
            arn: iot_arn("package/firmware"),
            created: vec![("stage", Some("test"))],
            absent_value: Some(""),
            missing: "Package firmware not found".to_string(),
        },
        Family {
            label: "package version, identified by the pair",
            setup: vec![
                Call::new(
                    "PUT",
                    "/packages/fleet-firmware".to_string(),
                    Some(json!({})),
                ),
                Call::new(
                    "PUT",
                    "/packages/fleet-firmware/versions/1.0.0".to_string(),
                    Some(json!({ "tags": { "stage": "test" } })),
                ),
            ],
            arn: iot_arn("package/fleet-firmware/version/1.0.0"),
            created: vec![("stage", Some("test"))],
            absent_value: Some(""),
            missing: "PackageVersion 1.0.0 for package fleet-firmware not found".to_string(),
        },
        Family {
            label: "topic rule, whose create takes a tag QUERY STRING",
            setup: vec![Call::new(
                "POST",
                "/rules/temperature".to_string(),
                Some(json!({ "sql": "SELECT * FROM 'sensors/#'", "actions": [] })),
            )
            .tagged("stage=test&unvalued")],
            arn: iot_arn("rule/temperature"),
            // The tag query string gives every key it carries a value, so a pair written
            // without one carries the empty value `TagValue` admits.
            created: vec![("stage", Some("test")), ("unvalued", Some(""))],
            absent_value: None,
            missing: "Rule temperature not found".to_string(),
        },
    ]
}

/// The tag set a family reports for `expected`, with a valueless tag rendered the way
/// that family stores one.
fn as_reported(
    expected: &[(&str, Option<&str>)],
    absent_value: Option<&str>,
) -> BTreeMap<String, Option<String>> {
    expected
        .iter()
        .map(|(key, value)| {
            (
                (*key).to_string(),
                value.or(absent_value).map(str::to_string),
            )
        })
        .collect()
}

/// A tag written through the tag plane is stored with the resource and read back from it,
/// for every family — beside the tags the resource's create carried, which are the same
/// set.
#[tokio::test]
async fn tags_round_trip_through_the_tag_plane_for_every_family() {
    let emulator = Emulator::new();
    for family in families() {
        let label = family.label;
        emulator.create(label, &family.setup).await;

        // What the CREATE recorded is what the read reports: the create's tags and the
        // tag plane's tags are one set, not two.
        let read = emulator.list_tags(&family.arn).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: a tag read of an existing resource expected 200, got {read:?}"
        );
        assert_eq!(
            read.tags(),
            Some(as_reported(&family.created, family.absent_value)),
            "case {label:?}: expected the tags its create carried, got {read:?}"
        );
        assert!(
            read.body.get("nextToken").is_none(),
            "case {label:?}: expected no continuation token, got {read:?}"
        );

        // Two tags ADDED: one with a value, one with none.
        let written = emulator
            .tag(&family.arn, &[("owner", Some("fleet-team")), ("probe", None)])
            .await;
        assert_eq!(
            written.status, 200,
            "case {label:?}: a tag write expected 200, got {written:?}"
        );

        // One key MODIFIED, which replaces its value and adds no tag.
        let modified = emulator.tag(&family.arn, &[("owner", Some("platform"))]).await;
        assert_eq!(
            modified.status, 200,
            "case {label:?}: modifying a tag expected 200, got {modified:?}"
        );

        let mut expected = family.created.clone();
        expected.push(("owner", Some("platform")));
        expected.push(("probe", None));
        let read = emulator.list_tags(&family.arn).await;
        assert_eq!(
            read.tags(),
            Some(as_reported(&expected, family.absent_value)),
            "case {label:?}: expected the merged tag set {expected:?}, got {read:?}"
        );

        // A second read reports the same set: the read wrote nothing.
        let reread = emulator.list_tags(&family.arn).await;
        assert_eq!(
            reread.tags(),
            read.tags(),
            "case {label:?}: a second read reported {reread:?} after {read:?}"
        );

        // Exactly the named keys are removed — a key that was never set removes nothing,
        // and every key the request does not name stays.
        let removed = emulator.untag(&family.arn, &["owner", "never-set"]).await;
        assert_eq!(
            removed.status, 200,
            "case {label:?}: an untag expected 200, got {removed:?}"
        );
        let mut expected = family.created.clone();
        expected.push(("probe", None));
        let read = emulator.list_tags(&family.arn).await;
        assert_eq!(
            read.tags(),
            Some(as_reported(&expected, family.absent_value)),
            "case {label:?}: expected {expected:?} after the untag, got {read:?}"
        );
    }
}

/// An ARN that names no resource this account holds is refused by both WRITES with that
/// resource family's own not-found sentence, while the READ serves it: it answers the
/// tags held for the ARN presented, and nothing is held for an ARN no resource answers
/// to.
#[tokio::test]
async fn an_arn_naming_no_resource_is_refused_by_the_writes_and_served_by_the_read() {
    let emulator = Emulator::new();
    for family in families() {
        let label = family.label;
        let replies = [
            ("TagResource", emulator.tag(&family.arn, &[("stage", Some("test"))]).await),
            ("UntagResource", emulator.untag(&family.arn, &["stage"]).await),
        ];
        for (operation, reply) in replies {
            assert_eq!(
                reply.status, 404,
                "case {label:?} {operation}: expected 404, got {reply:?}"
            );
            assert_eq!(
                reply
                    .error_code
                    .as_deref()
                    .map(|code| code.split(':').next().unwrap_or(code)),
                Some("ResourceNotFoundException"),
                "case {label:?} {operation}: expected ResourceNotFoundException, got {reply:?}"
            );
            assert_eq!(
                reply.message(),
                Some(family.missing.as_str()),
                "case {label:?} {operation}: expected {:?}, got {reply:?}",
                family.missing
            );
        }

        let read = emulator.list_tags(&family.arn).await;
        assert_eq!(
            read.status, 200,
            "case {label:?} ListTagsForResource: expected 200, got {read:?}"
        );
        assert_eq!(
            read.tags(),
            Some(BTreeMap::new()),
            "case {label:?} ListTagsForResource: expected no tags, got {read:?}"
        );

        // The refused writes recorded nothing: once the resource exists, the only tags
        // it carries are the ones its own create carried.
        emulator.create(label, &family.setup).await;
        let read = emulator.list_tags(&family.arn).await;
        assert_eq!(
            read.tags(),
            Some(as_reported(&family.created, family.absent_value)),
            "case {label:?}: expected only the tags its create carried, got {read:?}"
        );
    }
}

/// A `resourceArn` that addresses no resource of this service's namespace is refused as a
/// BAD REQUEST by all three operations — before any lookup, so whether the account holds
/// anything is irrelevant.
#[tokio::test]
async fn a_value_addressing_no_iot_resource_is_refused_as_a_bad_request() {
    let emulator = Emulator::new();
    // One row per way a value can address no resource of this namespace: not an ARN at
    // all, an ARN of another vendor, an ARN of another account, an IoT ARN carrying no
    // identifier, and an IoT ARN of a resource type this service holds no tag set for.
    let cases = [
        "fleet-alpha",
        "arn:aws:s3:::reports",
        &format!("arn:aws:iot:{REGION}:000000000001:thinggroup/fleet-alpha"),
        &iot_arn("thinggroup"),
        &iot_arn("thinggroup/"),
        &iot_arn("subscription/42"),
    ]
    .map(str::to_string);
    for case in cases {
        let replies = [
            ("TagResource", emulator.tag(&case, &[("stage", Some("test"))]).await),
            ("UntagResource", emulator.untag(&case, &["stage"]).await),
            ("ListTagsForResource", emulator.list_tags(&case).await),
        ];
        for (operation, reply) in replies {
            assert_eq!(
                reply.status, 400,
                "case {case:?} {operation}: expected 400, got {reply:?}"
            );
            assert_eq!(
                reply
                    .error_code
                    .as_deref()
                    .map(|code| code.split(':').next().unwrap_or(code)),
                Some("InvalidRequestException"),
                "case {case:?} {operation}: expected InvalidRequestException, got {reply:?}"
            );
        }
    }
}

/// A write that would leave a resource carrying more than the modelled maximum of tags is
/// refused, and changes nothing; one that only MODIFIES keys already stored at that
/// maximum is not a growth, so it is served.
#[tokio::test]
async fn a_write_past_the_tag_maximum_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();
    let arn = iot_arn("thinggroup/bounded");
    emulator
        .create(
            "bounded thing group",
            &[Call::new(
                "POST",
                "/thing-groups/bounded".to_string(),
                Some(json!({})),
            )],
        )
        .await;

    // The resource is filled to the modelled maximum in ONE write, which is served.
    let keys: Vec<String> = (0..MAX_RESOURCE_TAGS).map(|index| format!("key{index}")).collect();
    let at_limit: Vec<(&str, Option<&str>)> = keys
        .iter()
        .map(|key| (key.as_str(), Some("first")))
        .collect();
    let filled = emulator.tag(&arn, &at_limit).await;
    assert_eq!(
        filled.status, 200,
        "case {MAX_RESOURCE_TAGS} tags: expected 200, got {filled:?}"
    );

    // MODIFYING every one of them adds no tag, so it stays inside the maximum.
    let modified: Vec<(&str, Option<&str>)> = keys
        .iter()
        .map(|key| (key.as_str(), Some("second")))
        .collect();
    let remodified = emulator.tag(&arn, &modified).await;
    assert_eq!(
        remodified.status, 200,
        "case modifying every stored key: expected 200, got {remodified:?}"
    );

    // One key MORE would leave it over the maximum: refused, and the stored set is
    // untouched.
    let over = emulator.tag(&arn, &[("one-too-many", Some("x"))]).await;
    assert_eq!(
        over.status, 410,
        "case one key past the maximum: expected 410, got {over:?}"
    );
    assert_eq!(
        over.error_code
            .as_deref()
            .map(|code| code.split(':').next().unwrap_or(code)),
        Some("LimitExceededException"),
        "case one key past the maximum: expected LimitExceededException, got {over:?}"
    );
    let read = emulator.list_tags(&arn).await;
    assert_eq!(
        read.tags(),
        Some(as_reported(&modified, None)),
        "case one key past the maximum: the stored tags changed, got {read:?}"
    );
}

/// The tag read answers the whole tag set in one page, so it issues no continuation token
/// — and a token it could not have issued is refused rather than silently answering a
/// first page.
#[tokio::test]
async fn the_tag_read_issues_no_continuation_token_and_refuses_one() {
    let emulator = Emulator::new();
    let arn = iot_arn("thinggroup/paged");
    emulator
        .create(
            "paged thing group",
            &[Call::new(
                "POST",
                "/thing-groups/paged".to_string(),
                Some(json!({ "tags": [{ "Key": "stage", "Value": "test" }] })),
            )],
        )
        .await;

    let read = emulator.list_tags(&arn).await;
    assert_eq!(read.status, 200, "case a tagged resource: got {read:?}");
    assert!(
        read.body.get("nextToken").is_none(),
        "case a tagged resource: expected no continuation token, got {read:?}"
    );

    let resumed = emulator
        .send(&Call::new(
            "GET",
            format!(
                "/tags?resourceArn={}&nextToken={}",
                percent_encode(&arn),
                percent_encode("not-a-token")
            ),
            None,
        ))
        .await;
    assert_eq!(
        resumed.status, 400,
        "case a continuation token this listing never issued: expected 400, got {resumed:?}"
    );
    assert_eq!(
        resumed
            .error_code
            .as_deref()
            .map(|code| code.split(':').next().unwrap_or(code)),
        Some("InvalidRequestException"),
        "case a continuation token this listing never issued: got {resumed:?}"
    );
}
