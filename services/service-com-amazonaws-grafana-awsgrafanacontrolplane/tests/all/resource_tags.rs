// SPDX-License-Identifier: CC0-1.0

//! Tagging an Amazon Managed Grafana workspace, end to end over HTTP.
//!
//! The rule under test: `TagResource` associates the submitted tag keys and values with
//! the resource its `resourceArn` names, adding a key the resource does not carry and
//! replacing the value of one it does, leaving every other tag alone; `ListTagsForResource`
//! then reports exactly the stored set, and so does every operation that projects a
//! workspace's `tags`. A `resourceArn` naming no workspace this store holds is a
//! `ResourceNotFoundException`, and one that names no Grafana workspace at all is a
//! `ValidationException`.
//!
//! `ValidationException`. A served tag write answers 204 No Content with no body: its
//! output structure declares no member, so there is nothing for a caller to parse.
//!
//! Every assertion goes through the router, so each case exercises routing,
//! deserialization, the handler, the persisted state and serialization.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_grafana_awsgrafanacontrolplane::state::LIFECYCLE_TRANSITION;
use service_com_amazonaws_grafana_awsgrafanacontrolplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service; its region is the region the workspace
/// ARNs below are built for.
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/grafana/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The region [`SIGNED_FOR_EU_WEST_1`] signs for.
const REGION: &str = "eu-west-1";

/// The canonical account id every emulated ARN carries (`emulator_account_id()`),
/// spelled out here rather than imported so a wrong account in the implementation
/// cannot make this test pass.
const ACCOUNT: &str = "000000000000";

/// `TagMap`'s published bounds, from
/// `smithy_query(query="shape", shape="TagMap")`: at most 50 entries, keys
/// `TagKey` of length 1..128 and values `TagValue` of length 0..256.
const MAX_TAGS: usize = 50;
const MAX_KEY_LEN: usize = 128;
const MAX_VALUE_LEN: usize = 256;

/// The status a SERVED tag mutation answers: `TagResourceResponse` declares no member, so
/// the answer carries nothing to parse and is 204 No Content. The protocol leaves the code
/// defaulted (`smithy_query(query="operation",operation="TagResource")` reports
/// `codeDefaulted`), so this is the service's own behaviour.
const TAG_MUTATION_SUCCESS_STATUS: u16 = 204;

/// How long a case waits for something the clock owes the store before giving up, and how
/// often it looks again. Nothing asserts on how long it actually took.
const CLOCK_DEADLINE: Duration = LIFECYCLE_TRANSITION.saturating_mul(20);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    fn error_code(&self) -> &str {
        self.body["__type"].as_str().unwrap_or_default()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "grafana.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_EU_WEST_1);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// The URI a tagging call goes to.
///
/// The tagging operations bind `resourceArn` to a single, NON-greedy `{resourceArn}`
/// path segment (`/tags/{resourceArn}`), so a client percent-encodes the ARN's `:` and
/// `/` before putting it in the path — exactly as an AWS SDK does.
fn tags_uri(arn: &str) -> String {
    let encoded: String = arn
        .chars()
        .map(|c| match c {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            other => other.to_string(),
        })
        .collect();
    format!("/tags/{encoded}")
}

/// The ARN of a workspace in this account and region:
/// `arn:aws:grafana:<region>:<account>:/workspaces/<workspaceId>`, the form Amazon
/// Managed Grafana's resource ARNs carry. Spelled out here rather than imported.
fn workspace_arn(id: &str) -> String {
    format!("arn:aws:grafana:{REGION}:{ACCOUNT}:/workspaces/{id}")
}

/// Create a workspace carrying `tags` and answer its id.
async fn create_workspace(router: &MultiServiceRouter, tags: &BTreeMap<String, String>) -> String {
    let mut request = json!({
        "accountAccessType": "CURRENT_ACCOUNT",
        "permissionType": "CUSTOMER_MANAGED",
        "authenticationProviders": ["AWS_SSO"],
    });
    if !tags.is_empty() {
        request["tags"] = json!(tags);
    }
    let created = call(router, "POST", "/workspaces", Some(request)).await;
    assert_eq!(
        created.status, 202,
        "CreateWorkspace must succeed to set a tagging case up, got {} with {:?}",
        created.status, created.body
    );
    created.body["workspace"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateWorkspace answered no workspace id: {:?}", created.body))
        .to_string()
}

async fn tag(router: &MultiServiceRouter, arn: &str, tags: &BTreeMap<String, String>) -> Answer {
    call(router, "POST", &tags_uri(arn), Some(json!({"tags": tags}))).await
}

async fn list_tags(router: &MultiServiceRouter, arn: &str) -> Answer {
    call(router, "GET", &tags_uri(arn), None).await
}

async fn describe(router: &MultiServiceRouter, id: &str) -> Answer {
    call(router, "GET", &format!("/workspaces/{id}"), None).await
}

/// What one call answered on the wire, unparsed: the status, the length it states for its
/// body, and the body bytes themselves — so a body a renderer wrote is visible whatever it
/// holds.
struct RawAnswer {
    status: u16,
    length: Option<String>,
    body: Vec<u8>,
}

impl RawAnswer {
    /// The body as text, for an assertion message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Send one request and report exactly what came back.
async fn raw_call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> RawAnswer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "grafana.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_EU_WEST_1);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let length = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    RawAnswer {
        status,
        length,
        body: response.into_body().to_vec(),
    }
}

/// The length an answer that carries no content states for the body it does not have, so a
/// client that frames a bodyless message by its stated length reads an explicitly empty
/// one. It is the framing of the response this emulator hands out; what a socket client
/// reads is framed by the layer below this crate.
const NO_CONTENT_LENGTH: &str = "0";

/// Both tag mutations answer the no-content status with no body at all and state the length
/// that nothing has, while every answer that DOES carry members keeps the status that says
/// a body follows.
///
/// The rows span what the rule generalises over rather than one recorded call: which
/// mutation is called (so both the POST and the DELETE route), how many keys one call
/// carries, whether a key is new, overwrites one already there, is removed, or is not
/// carried by the resource at all, and whether the call is the first or a repeat. The
/// answer is the same for all of them, which is the point: the status and the absent body
/// follow the OUTPUT SHAPE — an output structure with no member — not how much the call
/// changed.
#[tokio::test]
async fn a_served_tag_mutation_answers_no_content_with_no_body() {
    // (case, method, the query the DELETE carries, the body the POST carries)
    let cases: Vec<(&str, &str, String, Option<Value>)> = vec![
        (
            "one new key written",
            "POST",
            String::new(),
            Some(json!({"tags": {"env": "dev"}})),
        ),
        (
            "several keys written at once, one of them overwriting",
            "POST",
            String::new(),
            Some(json!({"tags": {"env": "prod", "team": "observability", "owner": "ana"}})),
        ),
        (
            "a key written with the value it already carries",
            "POST",
            String::new(),
            Some(json!({"tags": {"env": "prod"}})),
        ),
        (
            "one carried key removed",
            "DELETE",
            "?tagKeys=owner".to_string(),
            None,
        ),
        (
            "two keys named in one removal",
            "DELETE",
            "?tagKeys=env&tagKeys=team".to_string(),
            None,
        ),
        (
            "a key the resource does not carry, so nothing is removed",
            "DELETE",
            "?tagKeys=never-set".to_string(),
            None,
        ),
    ];

    let router = emulator();
    let id = create_workspace(&router, &BTreeMap::new()).await;
    let arn = workspace_arn(&id);

    for (case, method, query, body) in cases {
        let uri = format!("{}{query}", tags_uri(&arn));
        for attempt in ["first", "repeated"] {
            let answer = raw_call(&router, method, &uri, body.clone()).await;
            assert_eq!(
                answer.status, TAG_MUTATION_SUCCESS_STATUS,
                "case {case:?} ({attempt}): expected HTTP {TAG_MUTATION_SUCCESS_STATUS}, got {} \
                 with {:?}",
                answer.status,
                answer.text()
            );
            assert!(
                answer.body.is_empty(),
                "case {case:?} ({attempt}): expected no body at all, got {:?}",
                answer.text()
            );
            assert_eq!(
                answer.length.as_deref(),
                Some(NO_CONTENT_LENGTH),
                "case {case:?} ({attempt}): expected a stated length of {NO_CONTENT_LENGTH}, \
                 got {:?}",
                answer.length
            );
        }
    }

    // An answer that carries members keeps the status that says a body follows: the tag
    // reader and the workspace read are both still answered with content, so the
    // no-content status is the mutation's and not the service's.
    for (case, uri) in [
        ("ListTagsForResource", tags_uri(&arn)),
        ("DescribeWorkspace", format!("/workspaces/{id}")),
    ] {
        let answer = raw_call(&router, "GET", &uri, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answer.status,
            answer.text()
        );
        assert!(
            !answer.body.is_empty(),
            "case {case:?}: expected a body to parse, got none"
        );
    }
}

/// The tag map at `value`, as an ordered map so two sets compare regardless of the
/// order the JSON object happened to carry.
fn tag_map(case: &str, what: &str, value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: {what} carried no tags object, got {value:?}"))
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: {what} tag {key:?} is not a string: {value:?}")
                    })
                    .to_string(),
            )
        })
        .collect()
}

/// The tags `ListTagsForResource` reports for `arn`.
async fn reported_tags(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
) -> BTreeMap<String, String> {
    let listed = list_tags(router, arn).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected ListTagsForResource 200, got {} with {:?}",
        listed.status, listed.body
    );
    tag_map(case, "ListTagsForResource", &listed.body["tags"])
}

/// A tag set from literal pairs.
fn pairs(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// One row of the merge table: the tags the workspace was created with, the tag sets
/// submitted in order, and the tags the resource must then carry.
struct MergeCase {
    name: &'static str,
    created_with: BTreeMap<String, String>,
    submissions: Vec<BTreeMap<String, String>>,
    expected: BTreeMap<String, String>,
}

/// The rule, over a set of inputs differing in tag count, key overlap, value form and
/// the number of calls the tags arrive in.
#[tokio::test]
async fn tag_resource_merges_the_submitted_tags_and_the_readers_report_them() {
    let fifty: Vec<(String, String)> = (0..MAX_TAGS)
        .map(|i| (format!("key{i:02}"), format!("value{i:02}")))
        .collect();
    let fifty: BTreeMap<String, String> = fifty.into_iter().collect();

    let cases = vec![
        MergeCase {
            name: "one tag onto an untagged workspace",
            created_with: BTreeMap::new(),
            submissions: vec![pairs(&[("env", "dev")])],
            expected: pairs(&[("env", "dev")]),
        },
        MergeCase {
            name: "a key the workspace does not carry is appended",
            created_with: pairs(&[("env", "dev")]),
            submissions: vec![pairs(&[("team", "observability")])],
            expected: pairs(&[("env", "dev"), ("team", "observability")]),
        },
        MergeCase {
            name: "a key the workspace carries takes the new value",
            created_with: pairs(&[("env", "dev"), ("team", "observability")]),
            submissions: vec![pairs(&[("env", "prod")])],
            expected: pairs(&[("env", "prod"), ("team", "observability")]),
        },
        MergeCase {
            name: "one call that both replaces and appends",
            created_with: pairs(&[("env", "dev"), ("team", "observability")]),
            submissions: vec![pairs(&[("env", "prod"), ("owner", "ana")])],
            expected: pairs(&[("env", "prod"), ("team", "observability"), ("owner", "ana")]),
        },
        MergeCase {
            name: "an empty tag value is a value, not an absence",
            created_with: BTreeMap::new(),
            submissions: vec![pairs(&[("note", "")])],
            expected: pairs(&[("note", "")]),
        },
        MergeCase {
            name: "non-ASCII keys and values",
            created_with: BTreeMap::new(),
            submissions: vec![pairs(&[("équipe", "données"), ("チーム", "観測")])],
            expected: pairs(&[("équipe", "données"), ("チーム", "観測")]),
        },
        MergeCase {
            name: "three calls, each carrying a different key, all persist",
            created_with: BTreeMap::new(),
            submissions: vec![
                pairs(&[("a", "1")]),
                pairs(&[("b", "2")]),
                pairs(&[("c", "3")]),
            ],
            expected: pairs(&[("a", "1"), ("b", "2"), ("c", "3")]),
        },
        MergeCase {
            name: "re-submitting the same pair changes nothing",
            created_with: pairs(&[("env", "dev")]),
            submissions: vec![pairs(&[("env", "dev")]), pairs(&[("env", "dev")])],
            expected: pairs(&[("env", "dev")]),
        },
        MergeCase {
            name: "the maximum number of tags the model admits in one request",
            created_with: BTreeMap::new(),
            submissions: vec![fifty.clone()],
            expected: fifty,
        },
        MergeCase {
            name: "tagging a workspace created with tags keeps the created ones",
            created_with: pairs(&[("created", "yes")]),
            submissions: vec![pairs(&[("tagged", "yes")])],
            expected: pairs(&[("created", "yes"), ("tagged", "yes")]),
        },
    ];

    let router = emulator();
    for case in cases {
        let name = case.name;
        let id = create_workspace(&router, &case.created_with).await;
        let arn = workspace_arn(&id);

        for (index, submission) in case.submissions.iter().enumerate() {
            let tagged = tag(&router, &arn, submission).await;
            assert_eq!(
                tagged.status, TAG_MUTATION_SUCCESS_STATUS,
                "case {name:?}: TagResource call {index} must answer \
                 {TAG_MUTATION_SUCCESS_STATUS}, got {} with {:?}",
                tagged.status, tagged.body
            );
            assert_eq!(
                tagged.body,
                Value::Null,
                "case {name:?}: TagResource call {index} answers no content, so it carries no \
                 body, got {:?}",
                tagged.body
            );
        }

        let reported = reported_tags(&router, name, &arn).await;
        assert_eq!(
            reported, case.expected,
            "case {name:?}: ListTagsForResource must report the merged tags, got {reported:?}"
        );

        // Round trip through the other reader of a workspace's tags.
        let described = describe(&router, &id).await;
        assert_eq!(
            described.status, 200,
            "case {name:?}: expected DescribeWorkspace 200, got {} with {:?}",
            described.status, described.body
        );
        let on_workspace = tag_map(name, "DescribeWorkspace", &described.body["workspace"]["tags"]);
        assert_eq!(
            on_workspace, case.expected,
            "case {name:?}: the described workspace must carry the merged tags, got {on_workspace:?}"
        );
    }
}

/// `TagKey` is `@length(min: 1, max: 128)` and `TagValue` `@length(min: 0, max: 256)`:
/// each published bound is accepted and one character past it is refused, leaving the
/// stored tags as they were.
///
/// The two refusals carry different codes because two different layers raise them: a tag
/// VALUE is walked by the service's request-body validator, which refuses the body as a
/// bad request, while a tag KEY is a map key the walk does not descend into, so the
/// handler refuses it with the modelled `ValidationException`.
#[tokio::test]
async fn tag_lengths_are_accepted_at_the_published_bounds_and_refused_past_them() {
    let cases: Vec<(&str, String, String, u16, &str)> = vec![
        (
            "the shortest key the model admits",
            "k".to_string(),
            "v".to_string(),
            TAG_MUTATION_SUCCESS_STATUS,
            "",
        ),
        (
            "the longest key the model admits",
            "k".repeat(MAX_KEY_LEN),
            "v".to_string(),
            TAG_MUTATION_SUCCESS_STATUS,
            "",
        ),
        (
            "one character past the longest key",
            "k".repeat(MAX_KEY_LEN + 1),
            "v".to_string(),
            400,
            "ValidationException",
        ),
        (
            "the shortest value the model admits",
            "empty".to_string(),
            String::new(),
            TAG_MUTATION_SUCCESS_STATUS,
            "",
        ),
        (
            "the longest value the model admits",
            "long".to_string(),
            "v".repeat(MAX_VALUE_LEN),
            TAG_MUTATION_SUCCESS_STATUS,
            "",
        ),
        (
            "one character past the longest value",
            "long".to_string(),
            "v".repeat(MAX_VALUE_LEN + 1),
            400,
            "BadRequestException",
        ),
    ];

    let router = emulator();
    for (case, key, value, expected_status, expected_code) in cases {
        let id = create_workspace(&router, &BTreeMap::new()).await;
        let arn = workspace_arn(&id);
        let submission: BTreeMap<String, String> = [(key.clone(), value.clone())].into();
        let answer = tag(&router, &arn, &submission).await;
        assert_eq!(
            answer.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {} with {:?}",
            answer.status, answer.body
        );

        let reported = reported_tags(&router, case, &arn).await;
        if expected_status == TAG_MUTATION_SUCCESS_STATUS {
            assert_eq!(
                reported, submission,
                "case {case:?}: the accepted tag must be stored verbatim, got {reported:?}"
            );
        } else {
            assert_eq!(
                answer.error_code(),
                expected_code,
                "case {case:?}: expected {expected_code}, got {:?}",
                answer.body
            );
            assert!(
                reported.is_empty(),
                "case {case:?}: a refused request must store nothing, got {reported:?}"
            );
        }
    }
}

/// A resource holds at most `TagMap`'s 50 entries: a request that would take it past
/// that is refused and leaves the stored tags untouched, while one that only rewrites
/// existing keys at the maximum is accepted.
#[tokio::test]
async fn a_request_past_the_tag_maximum_is_refused_and_stores_nothing() {
    let case = "a workspace already carrying the maximum number of tags";
    let router = emulator();
    let full: BTreeMap<String, String> = (0..MAX_TAGS)
        .map(|i| (format!("key{i:02}"), format!("value{i:02}")))
        .collect();
    let id = create_workspace(&router, &full).await;
    let arn = workspace_arn(&id);

    assert_eq!(
        reported_tags(&router, case, &arn).await.len(),
        MAX_TAGS,
        "case {case:?}: the create must have stored all {MAX_TAGS} tags"
    );

    let refused = tag(&router, &arn, &pairs(&[("one-too-many", "x")])).await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {:?}",
        refused.body
    );
    let after = reported_tags(&router, case, &arn).await;
    assert_eq!(
        after, full,
        "case {case:?}: a refused TagResource must leave the stored tags untouched, got {after:?}"
    );

    // Rewriting a key the resource already carries does not grow it, so it is accepted
    // at the maximum.
    let case = "rewriting an existing key at the maximum";
    let rewritten = tag(&router, &arn, &pairs(&[("key00", "rewritten")])).await;
    assert_eq!(
        rewritten.status, TAG_MUTATION_SUCCESS_STATUS,
        "case {case:?}: expected HTTP {TAG_MUTATION_SUCCESS_STATUS}, got {} with {:?}",
        rewritten.status, rewritten.body
    );
    let after = reported_tags(&router, case, &arn).await;
    assert_eq!(
        after.len(),
        MAX_TAGS,
        "case {case:?}: the tag count must stay at {MAX_TAGS}, got {after:?}"
    );
    assert_eq!(
        after.get("key00").map(String::as_str),
        Some("rewritten"),
        "case {case:?}: the rewritten value must be stored, got {after:?}"
    );
}

/// An ARN of the right form naming a workspace this store does not hold (never created,
/// or deleted) is a `ResourceNotFoundException` from both tagging operations.
#[tokio::test]
async fn an_arn_naming_no_stored_workspace_is_a_resource_not_found() {
    let router = emulator();
    let created = create_workspace(&router, &BTreeMap::new()).await;
    let deleted = call(&router, "DELETE", &format!("/workspaces/{created}"), None).await;
    assert_eq!(
        deleted.status, 202,
        "DeleteWorkspace must succeed to set this case up, got {} with {:?}",
        deleted.status, deleted.body
    );
    // The delete is ACCEPTED, not finished: the workspace is removed from the store once
    // the removal the clock owes it falls due, and only then does its ARN name nothing. So
    // look again until the read reports it gone rather than waiting a span of time.
    let deadline = std::time::Instant::now() + CLOCK_DEADLINE;
    loop {
        let described = describe(&router, &created).await;
        if described.status == 404 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the deleted workspace was never released, last read {} with {:?}",
            described.status,
            described.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (case, id) in [
        ("a workspace id that was never minted", "g-ffffffffff".to_string()),
        ("a workspace that was deleted", created),
    ] {
        let arn = workspace_arn(&id);
        let tagged = tag(&router, &arn, &pairs(&[("env", "dev")])).await;
        assert_eq!(
            tagged.status, 404,
            "case {case:?}: expected TagResource 404, got {} with {:?}",
            tagged.status, tagged.body
        );
        assert_eq!(
            tagged.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            tagged.body
        );
        assert_eq!(
            tagged.body["resourceId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: the refusal must name the workspace, got {:?}",
            tagged.body
        );

        let listed = list_tags(&router, &arn).await;
        assert_eq!(
            listed.status, 404,
            "case {case:?}: expected ListTagsForResource 404, got {} with {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            listed.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            listed.body
        );
    }
}

/// `resourceArn` must name a Grafana workspace: the only resource this service tags.
/// Every other form is refused with the modelled `ValidationException`, naming the
/// member at fault.
#[tokio::test]
async fn an_arn_that_names_no_grafana_workspace_is_refused() {
    let router = emulator();
    let id = create_workspace(&router, &pairs(&[("env", "dev")])).await;

    let cases = vec![
        ("not an ARN at all", id.clone()),
        ("an ARN of another service", format!("arn:aws:s3:{REGION}:{ACCOUNT}:/workspaces/{id}")),
        (
            "a Grafana ARN naming another resource type",
            format!("arn:aws:grafana:{REGION}:{ACCOUNT}:/dashboards/{id}"),
        ),
        (
            "a workspaces ARN whose id is not a workspace id",
            format!("arn:aws:grafana:{REGION}:{ACCOUNT}:/workspaces/not-an-id"),
        ),
        (
            "a Grafana ARN with no resource section",
            format!("arn:aws:grafana:{REGION}:{ACCOUNT}:"),
        ),
        ("an ARN with too few fields", format!("arn:aws:grafana:/workspaces/{id}")),
    ];

    for (case, arn) in cases {
        let tagged = tag(&router, &arn, &pairs(&[("env", "prod")])).await;
        assert_eq!(
            tagged.status, 400,
            "case {case:?}: expected TagResource 400 for {arn:?}, got {} with {:?}",
            tagged.status, tagged.body
        );
        assert_eq!(
            tagged.error_code(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            tagged.body
        );

        let listed = list_tags(&router, &arn).await;
        assert_eq!(
            listed.status, 400,
            "case {case:?}: expected ListTagsForResource 400 for {arn:?}, got {} with {:?}",
            listed.status, listed.body
        );
    }

    // The refusals touched nothing: the workspace still carries what it was created with.
    let still = reported_tags(&router, "after the refusals", &workspace_arn(&id)).await;
    assert_eq!(
        still,
        pairs(&[("env", "dev")]),
        "the refused calls must leave the workspace's tags untouched, got {still:?}"
    );
}

/// `ListTagsForResource` is `@readonly`: reading tags never changes what a later caller
/// sees, however often it is called.
#[tokio::test]
async fn list_tags_for_resource_does_not_change_what_a_later_caller_sees() {
    let case = "repeated reads";
    let router = emulator();
    let id = create_workspace(&router, &BTreeMap::new()).await;
    let arn = workspace_arn(&id);
    let submitted = pairs(&[("env", "dev"), ("team", "observability")]);
    let tagged = tag(&router, &arn, &submitted).await;
    assert_eq!(
        tagged.status, TAG_MUTATION_SUCCESS_STATUS,
        "case {case:?}: TagResource must answer {TAG_MUTATION_SUCCESS_STATUS}, got {} with {:?}",
        tagged.status, tagged.body
    );

    for read in 0..4 {
        let reported = reported_tags(&router, case, &arn).await;
        assert_eq!(
            reported, submitted,
            "case {case:?}: read {read} must report the same tags, got {reported:?}"
        );
    }
}

/// Concurrent `TagResource` calls, each carrying a different key, all persist: the
/// merge is into the stored map, not a replacement of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_tag_resource_calls_all_persist() {
    let case = "eight concurrent single-tag calls";
    let router = Arc::new(emulator());
    let id = create_workspace(&router, &pairs(&[("created", "yes")])).await;
    let arn = workspace_arn(&id);

    let submissions: Vec<BTreeMap<String, String>> = (0..8)
        .map(|i| [(format!("concurrent{i}"), format!("value{i}"))].into())
        .collect();

    let mut running = Vec::new();
    for submission in submissions.clone() {
        let router = Arc::clone(&router);
        let arn = arn.clone();
        running.push(tokio::spawn(
            async move { tag(&router, &arn, &submission).await },
        ));
    }
    for (index, task) in running.into_iter().enumerate() {
        let answer = task.await.expect("a tagging task must not panic");
        assert_eq!(
            answer.status, TAG_MUTATION_SUCCESS_STATUS,
            "case {case:?}: concurrent call {index} must answer \
             {TAG_MUTATION_SUCCESS_STATUS}, got {} with {:?}",
            answer.status, answer.body
        );
    }

    let mut expected = pairs(&[("created", "yes")]);
    for submission in &submissions {
        expected.extend(submission.clone());
    }
    let reported = reported_tags(&router, case, &arn).await;
    assert_eq!(
        reported, expected,
        "case {case:?}: every concurrent call's tag must persist, got {reported:?}"
    );
}
