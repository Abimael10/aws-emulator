// SPDX-License-Identifier: CC0-1.0

//! How the `/capacity-providers` backend of this control plane WORDS a request that breaks a
//! modelled constraint, end to end over HTTP.
//!
//! `bedrock-agentcore-control` is one endpoint in front of several AgentCore services, one
//! per resource family, and they do not word a constraint refusal alike
//! (`tests/all/validation_message_rendering.rs` covers the counted clause the other families
//! state). This family states the other form, and every row below asserts the SAME rule over
//! a set of operations, members, constraint classes and request locations rather than over
//! the one request a report happened to cite:
//!
//! * the envelope prefixes NO violation count;
//! * the offending member is addressed as the JSON pointer that names it in the request —
//!   `/`-separated, at any depth, with a list element named by its 0-based index;
//! * no value of the caller's is quoted;
//! * a `@range` or `@length` violation states the WINDOW the model declares, naming BOTH
//!   bounds, rather than the single bound the value fell outside; a member the model bounds
//!   on one side states that bound alone.
//!
//! Sources: `smithy_query(query="closure", operation="CreateCapacityProvider")` for every
//! member and bound asserted here (`InstanceLifecycleConfiguration.idleInstanceTimeout` and
//! `maxLifetime` `@range(60, 1209600)`, `VolumeIops` `@range(100, 256000)`,
//! `EbsCardIndex` `@range(min: 0)`), and `smithy_query(query="operation",
//! operation="ListCapacityProviders,UpdateCapacityProvider")` for `maxResults`
//! `@range(1, 100)`, `NextToken` `@length(1, 2048)` and `CapacityProviderId`'s `@pattern`.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// `InstanceLifecycleConfiguration.idleInstanceTimeout` / `maxLifetime` `@range` bounds.
const TIMEOUT_MIN: i64 = 60;
const TIMEOUT_MAX: i64 = 1_209_600;

/// The window those two members' clause states, from the bounds above.
const TIMEOUT_WINDOW: &str = "Member must be between 60 and 1209600, inclusive";

/// `NextToken`'s modelled `@length` maximum.
const NEXT_TOKEN_MAX: usize = 2048;

/// One router over one engine, whose calls are signed as a principal the access decision
/// allows ([`crate::authorized_signing_identity`]): every row reads the clause this family's
/// backend words, including the row addressing a provider by an identifier no provider can
/// carry, which the published credential alone is refused for
/// (`unnameable_identifier_authorization.rs`).
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::authorized_signing_identity::given_authorized_caller(&engine);
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service: what the router routes on, and the
/// identity it names.
fn authorization() -> String {
    let key = crate::authorized_signing_identity::AUTHORIZED_KEY_ID;
    format!(
        "AWS4-HMAC-SHA256 Credential={key}/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the status, the `x-amzn-errortype` header and the `message` the body carries.
struct Answer {
    status: u16,
    error_type: String,
    message: String,
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answer {
        status: response.status().as_u16(),
        error_type,
        message: parsed
            .pointer("/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

/// A `CreateCapacityProvider` body inside every modelled constraint, which each row below
/// breaks in exactly one place.
///
/// Every value satisfies the member it fills: the name its `@pattern`, the role and the
/// subnet/security-group ids theirs, the instance type its `@length`, and the lifecycle
/// timeout its `@range`.
fn valid_create_body() -> Value {
    json!({
        "name": "capacityProvider1",
        "permissionsConfiguration": {
            "capacityProviderOperatorRoleArn":
                "arn:aws:iam::000000000000:role/AgentCoreCapacityProvider"
        },
        "computeConfiguration": {
            "ec2Configuration": {
                "launchTemplateSource": {
                    "launchParameters": {
                        "operatingSystem": "LINUX_X86_64",
                        "instanceRequirements": {"allowedInstanceTypes": ["m5.large"]}
                    }
                },
                "vpcConfiguration": {
                    "subnets": ["subnet-0123456789abcdef0"],
                    "securityGroups": ["sg-0123456789abcdef0"]
                },
                "lifecycleConfiguration": {"idleInstanceTimeout": 900}
            }
        }
    })
}

/// `valid_create_body` with the member at `pointer` replaced by `value` — the one place a row
/// differs from a request this service accepts.
fn create_body_with(pointer: &str, value: Value) -> Value {
    let mut body = valid_create_body();
    *body
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("{pointer} is a member of the valid body")) = value;
    body
}

/// One refused request: how it is sent, and the clause this family must answer with.
struct Row {
    /// What this row is a case of, named in every assertion it makes.
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// The JSON pointer the clause addresses the offending member by.
    pointer: String,
    /// The requirement the clause states.
    requirement: String,
}

/// Assert the status and the error code every constraint refusal of this service carries.
fn assert_refused(answer: &Answer, case: &str) {
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (400, "ValidationException"),
        "case {case:?}: a value outside its modelled constraint is refused 400 \
         ValidationException, got {} {:?} with {:?}",
        answer.status,
        answer.error_type,
        answer.message
    );
}

/// Assert the whole sentence this family states for `row`, and the three things it never
/// carries: a violation count, a dotted member path, or a value the caller sent.
fn assert_pointer_form(answer: &Answer, row: &Row) {
    let case = row.case;
    assert_refused(answer, case);
    let expected = format!(
        "validation error detected. Value at '{}' failed to satisfy constraint: {}",
        row.pointer, row.requirement
    );
    assert_eq!(
        answer.message, expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message
    );
    assert!(
        !answer.message.starts_with(char::is_numeric)
            && !answer.message.contains("validation error detected:"),
        "case {case:?}: this family's envelope prefixes no violation count, got {:?}",
        answer.message
    );
    assert!(
        !answer.message.contains("Value '"),
        "case {case:?}: this family's clause quotes no value, got {:?}",
        answer.message
    );
}

/// RULE: a `@range` violation anywhere in this family's input states the member as a JSON
/// pointer and the whole window the model declares — whichever bound the value fell outside,
/// at whatever depth the member sits, and whether it travelled in the body or the URI.
#[tokio::test]
async fn a_range_violation_states_the_pointer_and_the_whole_window() {
    let router = router();
    let timeout_pointer =
        "/computeConfiguration/ec2Configuration/lifecycleConfiguration/idleInstanceTimeout";
    let lifetime_pointer =
        "/computeConfiguration/ec2Configuration/lifecycleConfiguration/maxLifetime";
    let rows = vec![
        Row {
            case: "idleInstanceTimeout one past its modelled maximum",
            method: "PUT",
            uri: "/capacity-providers".to_string(),
            body: Some(create_body_with(timeout_pointer, json!(TIMEOUT_MAX + 1))),
            pointer: timeout_pointer.to_string(),
            requirement: TIMEOUT_WINDOW.to_string(),
        },
        Row {
            // The same member below the window: the clause states the window, not the bound
            // the value broke.
            case: "idleInstanceTimeout one below its modelled minimum",
            method: "PUT",
            uri: "/capacity-providers".to_string(),
            body: Some(create_body_with(timeout_pointer, json!(TIMEOUT_MIN - 1))),
            pointer: timeout_pointer.to_string(),
            requirement: TIMEOUT_WINDOW.to_string(),
        },
        Row {
            case: "maxLifetime, a sibling member the same shape declares",
            method: "PUT",
            uri: "/capacity-providers".to_string(),
            body: Some(create_body_with(
                "/computeConfiguration/ec2Configuration/lifecycleConfiguration",
                json!({"maxLifetime": TIMEOUT_MAX + 1}),
            )),
            pointer: lifetime_pointer.to_string(),
            requirement: TIMEOUT_WINDOW.to_string(),
        },
        Row {
            case: "rootVolume.iops, a member of another nested shape",
            method: "PUT",
            uri: "/capacity-providers".to_string(),
            body: Some(create_body_with(
                "/computeConfiguration/ec2Configuration",
                {
                    let mut ec2 = valid_create_body()["computeConfiguration"]["ec2Configuration"]
                        .clone();
                    ec2["rootVolume"] = json!({"volumeType": "gp3", "iops": 99});
                    ec2
                },
            )),
            pointer: "/computeConfiguration/ec2Configuration/rootVolume/iops".to_string(),
            requirement: "Member must be between 100 and 256000, inclusive".to_string(),
        },
        Row {
            // A member inside a LIST element, and one the model bounds on one side only:
            // the clause names the element by its 0-based index and states that one bound.
            case: "ephemeralVolumes[0].ebs.ebsCardIndex, bounded below only",
            method: "PUT",
            uri: "/capacity-providers".to_string(),
            body: Some(create_body_with(
                "/computeConfiguration/ec2Configuration/launchTemplateSource/launchParameters",
                json!({
                    "operatingSystem": "LINUX_X86_64",
                    "instanceRequirements": {"allowedInstanceTypes": ["m5.large"]},
                    "ephemeralVolumes": [{
                        "deviceName": "/dev/sdb",
                        "ebs": {"volumeType": "gp3", "ebsCardIndex": -1}
                    }]
                }),
            )),
            pointer: "/computeConfiguration/ec2Configuration/launchTemplateSource/\
                      launchParameters/ephemeralVolumes/0/ebs/ebsCardIndex"
                .to_string(),
            requirement: "Member must be greater than or equal to 0".to_string(),
        },
        Row {
            // Another operation of the family, and a member that travels in the query
            // string rather than the body.
            case: "ListCapacityProviders maxResults below its window",
            method: "POST",
            uri: "/capacity-providers?maxResults=0".to_string(),
            body: None,
            pointer: "/maxResults".to_string(),
            requirement: "Member must be between 1 and 100, inclusive".to_string(),
        },
        Row {
            case: "ListCapacityProviders maxResults above its window",
            method: "POST",
            uri: "/capacity-providers?maxResults=101".to_string(),
            body: None,
            pointer: "/maxResults".to_string(),
            requirement: "Member must be between 1 and 100, inclusive".to_string(),
        },
    ];

    for row in &rows {
        let answer = send(&router, row.method, &row.uri, row.body.as_ref()).await;
        assert_pointer_form(&answer, row);
    }
}

/// RULE, over the other constraint classes this family's inputs declare: the envelope, the
/// pointer and the absent value echo are the form's, whatever the violation was — and a
/// `@length` the model bounds on both sides states its window the same way a `@range` does.
#[tokio::test]
async fn every_constraint_class_is_stated_in_the_same_form() {
    let router = router();
    let over_long_token = "a".repeat(NEXT_TOKEN_MAX + 1);
    let mut without_name = valid_create_body();
    without_name
        .as_object_mut()
        .expect("the create body is an object")
        .remove("name");
    let rows = vec![
        Row {
            case: "ListCapacityProviders nextToken past its @length window",
            method: "POST",
            uri: format!("/capacity-providers?nextToken={over_long_token}"),
            body: None,
            pointer: "/nextToken".to_string(),
            requirement: format!(
                "Member must have length between 1 and {NEXT_TOKEN_MAX}, inclusive"
            ),
        },
        Row {
            // A `@pattern` is quoted without the whole-value anchors the model spells it
            // with, the one rendering of an expression this service publishes.
            case: "UpdateCapacityProvider capacityProviderId outside its @pattern",
            method: "PUT",
            uri: "/capacity-providers/9notaprovider-abcdefghij".to_string(),
            body: Some(json!({})),
            pointer: "/capacityProviderId".to_string(),
            requirement: "Member must satisfy regular expression pattern: \
                          [a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}"
                .to_string(),
        },
        Row {
            case: "CreateCapacityProvider with the required name absent",
            method: "PUT",
            uri: "/capacity-providers".to_string(),
            body: Some(without_name),
            pointer: "/name".to_string(),
            requirement: "Member must not be null".to_string(),
        },
    ];

    for row in &rows {
        let answer = send(&router, row.method, &row.uri, row.body.as_ref()).await;
        assert_pointer_form(&answer, row);
    }
}

/// The form is the FAMILY's, not this service's: an operation of another family still states
/// the counted clause, with the dotted member path and the value the request carried.
#[tokio::test]
async fn another_family_still_states_the_counted_clause() {
    let router = router();
    let answer = send(
        &router,
        "PUT",
        "/browser-profiles",
        Some(&json!({"name": "9profile"})),
    )
    .await;
    let case = "CreateBrowserProfile name, outside its @pattern in another family";
    assert_refused(&answer, case);
    let expected = "1 validation error detected: Value '9profile' at 'name' failed to satisfy \
                    constraint: Member must satisfy regular expression pattern: \
                    [a-zA-Z][a-zA-Z0-9_]{0,47}";
    assert_eq!(
        answer.message, expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message
    );
}

/// A value INSIDE the window is accepted — at each published bound and in between — so the
/// rows above are about the wording of a refusal and not about refusing more requests.
#[tokio::test]
async fn a_value_inside_the_window_is_not_refused() {
    let router = router();
    let pointer =
        "/computeConfiguration/ec2Configuration/lifecycleConfiguration/idleInstanceTimeout";
    // (case, the provider's name, idleInstanceTimeout) — the published bounds and one value
    // between them. Each row carries its OWN name: the name is unique within the account, so
    // a repeated one would be answered a conflict for a reason unrelated to the bound.
    let rows: [(&str, &str, i64); 3] = [
        ("the window's minimum", "windowMinimum", TIMEOUT_MIN),
        ("the documented default", "windowDefault", 900),
        ("the window's maximum", "windowMaximum", TIMEOUT_MAX),
    ];

    for (case, name, timeout) in rows {
        let mut body = create_body_with(pointer, json!(timeout));
        body["name"] = json!(name);
        let answer = send(&router, "PUT", "/capacity-providers", Some(&body)).await;
        assert_eq!(
            answer.status, 202,
            "case {case:?} ({timeout}): a value inside the modelled window is accepted, got {} \
             {:?} with {:?}",
            answer.status, answer.error_type, answer.message
        );
    }
}
