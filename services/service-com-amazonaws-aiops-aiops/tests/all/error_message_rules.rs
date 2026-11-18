// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a 4xx, over the whole surface that can answer one.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`,
//! restJson1 serialization, the modelled-constraint walk, this service's refusal
//! renderer and the handlers — so what is asserted is the message a signed AWS SDK
//! request observes.
//!
//! Two rules are under test, each over a SET of inputs rather than one recorded case:
//!
//! 1. A `ValidationException` message raised by the modelled-constraint walk names the
//!    member and the requirement it violated, in the AWS envelope
//!    (`N validation error(s) detected: …`), and carries NOTHING of the value that
//!    violated it; a `@pattern` requirement is stated as the expression the service
//!    applies, without the `^`/`$` a model spells it with. The rows span the constraint
//!    classes (length minimum and maximum, range minimum and maximum, pattern, value
//!    set, absence and a service rule the model does not express), the members (`name`,
//!    `roleArn`, `retentionInDays`, `encryptionConfiguration.type`, `identifier`,
//!    `maxResults`, `policy`) and the four operations that can be refused this way. The
//!    rules the model does NOT express — a resource policy that is not a JSON document,
//!    and a cross-account source role in the investigation account — are worded as the
//!    service words those error classes instead, and are pinned by
//!    `tests/all/service_worded_refusals.rs`.
//! 2. A `ResourceNotFoundException` message names the modelled RESOURCE TYPE that is
//!    absent — `<Resource> does not exist.` — and carries nothing of the identifier
//!    that named it. The rows span the six operations that read or write a group by
//!    `{identifier}`, the identifier forms each accepts (only an ARN resolves a group,
//!    so a bare name or id is reported absent too), and both resources the policy
//!    operations reference (an absent GROUP is reported as the group; a group that
//!    exists with no policy as the policy).
//! 3. WHICH SUBJECT a clause of rule 1 takes is decided by whether the request carried
//!    a value at that path, never by whether the refusal has one to show: a member the
//!    request did not carry is the only `Value null` subject, and a violation raised on
//!    a value whose bytes are withheld — a `@sensitive` member's — is stated as every
//!    other present-but-invalid member is. The rows span the two subjects, the
//!    constraint classes a withheld value can break, and both layers that word one (the
//!    boundary walk and a handler's own rule).

use std::sync::Arc;

use aws_smithy_schema::traits::SensitiveTrait;
use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::{LengthTrait, PatternTrait};
use serde_json::{json, Value};
use service_com_amazonaws_aiops_aiops::ops::types::{
    CREATE_INVESTIGATION_GROUP_INPUT_SCHEMA, ENCRYPTION_CONFIGURATION_TYPE_VALUES,
    LIST_INVESTIGATION_GROUPS_INPUT_SCHEMA,
};
use service_com_amazonaws_aiops_aiops::state;
use smithy_server_runtime::{MultiServiceRouter, Violation, ViolationKind};

/// The region every request below signs for.
const REGION: &str = "us-west-2";

/// A role ARN that satisfies the modelled `RoleArn` (len 20..2048, `^arn:.*$`).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/AIOpsAssistantRole";

/// A policy document inside the modelled `InvestigationGroupPolicyDocument` bounds.
const POLICY: &str = r#"{"Version":"2012-10-17","Statement":[]}"#;

/// The `@pattern` the model declares for the group `name`
/// (`smithy_query(query="shape",shape="StringWithPatternAndLengthLimits")`).
const MODEL_NAME_PATTERN: &str = r"^[\-_A-Za-z0-9\[\]\(\)\{\}\.: ]+$";

/// The expression the SERVICE applies to a group name, as its recorded 400
/// `ValidationException` responses quote it: the modelled class plus `!`, `?`, `@` and
/// `;`, and no anchors.
const SERVICE_NAME_PATTERN: &str = r"[\-_A-Za-z0-9\[\]\(\)\{\}\.:!?@; ]+";

/// The modelled `@pattern` of `RoleArn`, as the service reports it (unanchored).
const ROLE_ARN_PATTERN: &str = "arn:.*";

/// The modelled `@pattern` of `InvestigationGroupIdentifier`, as the service reports
/// it (unanchored) — `smithy_query(query="shape",shape="InvestigationGroupIdentifier")`.
const IDENTIFIER_PATTERN: &str = r"(?:[\-_A-Za-z0-9]{1,512}|arn:(aws|aws-us-gov|aws-cn|aws-iso|aws-iso-b):aiops:[a-zA-Z0-9-]*:[0-9]{12}:investigation-group\/[A-Za-z0-9]{16})";

/// One emulator with an empty store, mounted exactly as the router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_aiops_aiops::EmulatedService::new(
        engine,
    ));
    router
}

/// A request signed for `aiops` in [`REGION`], the signal the router routes on.
fn request(method: &str, uri: &str, body: Option<&Value>) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("aiops.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDAIOPSTEST/20260101/{REGION}/aiops/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    builder.body(payload).expect("a well-formed test request")
}

/// Status, error code and `message` of one call.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, String, String) {
    let response = router.dispatch(request(method, uri, body)).await;
    let status = response.status().as_u16();
    let header_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(header_type)
        .unwrap_or_default()
        // An error `__type` may be namespaced (`com.amazonaws.aiops#Foo`).
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, code, message)
}

/// Percent-encode a value for use as one `@httpLabel` path segment, which is what an
/// SDK does: a non-greedy label may not carry a raw `/`.
fn label(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '/' => "%2F".to_string(),
            ' ' => "%20".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// One row of the refusal table: the case name, the request (method, URI, body), the
/// value the request carried for the member under test (so the assertion can check it
/// is NOT echoed), and the message the service must answer with.
type RefusalCase = (
    &'static str,
    &'static str,
    String,
    Option<Value>,
    String,
    String,
);

/// One clause of a validation message: the member and the requirement, no value.
fn clause(member_path: &str, requirement: &str) -> String {
    format!("Value at '{member_path}' failed to satisfy constraint: {requirement}")
}

/// One clause for a member the request did not carry at all.
fn absent_clause(member_path: &str) -> String {
    format!("Value null at '{member_path}' failed to satisfy constraint: Member must not be null")
}

/// The AWS envelope over the clauses of one message.
fn envelope(clauses: &[String]) -> String {
    match clauses {
        [clause] => format!("1 validation error detected: {clause}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// The requirement a `@pattern` states.
fn pattern_requirement(pattern: &str) -> String {
    format!("Member must satisfy regular expression pattern: {pattern}")
}

/// The generated schema of one member of `input`, or `None` when `input` has no such
/// member — the one lookup every reader of a modelled trait below goes through, so all
/// of them resolve a member the way the boundary walk does.
fn member_schema<'a>(input: &'a Schema, member: &str) -> Option<&'a Schema> {
    input
        .members()
        .iter()
        .copied()
        .find(|candidate| candidate.member_name() == Some(member))
}

/// The `@pattern` expression the model declares for one member of `input`, or `None`
/// when it declares none — read off the generated schema, which is the same trait map
/// the boundary walk enforces, so this cannot drift from the model.
fn modelled_member_pattern(input: &Schema, member: &str) -> Option<&'static str> {
    let declared = member_schema(input, member)?
        .traits()?
        .get(&PatternTrait::TRAIT_ID)?;
    Some(declared.as_any().downcast_ref::<PatternTrait>()?.regex)
}

/// The length MAXIMUM the model declares for one member of `input`, or `None` when it
/// declares none — read off the generated schema, the same trait map the boundary walk
/// enforces, so a bound asserted here cannot drift from the model.
fn modelled_member_length_max(input: &Schema, member: &str) -> Option<i64> {
    let declared = member_schema(input, member)?
        .traits()?
        .get(&LengthTrait::TRAIT_ID)?;
    declared.as_any().downcast_ref::<LengthTrait>()?.max
}

/// Whether the model marks one member of `input` `@sensitive` — the marker that makes
/// the boundary walk withhold the value of every violation it raises on that member.
fn member_is_sensitive(input: &Schema, member: &str) -> bool {
    member_schema(input, member)
        .and_then(|member_schema| member_schema.traits())
        .is_some_and(|traits| traits.get(&SensitiveTrait::TRAIT_ID).is_some())
}

/// Identifier forms that resolve no group: a bare label no group carries, a bare
/// 16-character id (a form the modelled pattern admits but no `{identifier}` member
/// documents, so it resolves nothing even when a group carries that id) and the full ARN
/// of a group that was never created.
fn unknown_identifiers() -> Vec<(&'static str, String)> {
    vec![
        ("a bare name no group carries", "no-such-group".to_string()),
        ("a bare id", "AAAAAAAAAAAAAAAA".to_string()),
        (
            "a full ARN",
            format!(
                "arn:aws:aiops:{REGION}:000000000000:investigation-group/AAAAAAAAAAAAAAAA"
            ),
        ),
    ]
}

/// The ARN of the group named `name`, read off `ListInvestigationGroups`.
async fn arn_of(router: &MultiServiceRouter, name: &str) -> String {
    let response = router
        .dispatch(request("GET", "/investigationGroups", None))
        .await;
    let body: Value = serde_json::from_slice(&response.into_body()).expect("a JSON body");
    body.get("investigationGroups")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("no investigationGroups in {body:?}"))
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|item| item.get("arn").and_then(Value::as_str))
        .unwrap_or_else(|| panic!("no group named {name:?} in {body:?}"))
        .to_string()
}

/// The six operations that resolve a group through `{identifier}`, as
/// `(name, method, path suffix, body)`.
fn group_readers() -> Vec<(&'static str, &'static str, &'static str, Option<Value>)> {
    vec![
        ("GetInvestigationGroup", "GET", "", None),
        ("DeleteInvestigationGroup", "DELETE", "", None),
        (
            "UpdateInvestigationGroup",
            "PATCH",
            "",
            Some(json!({ "isCloudTrailEventHistoryEnabled": false })),
        ),
        ("GetInvestigationGroupPolicy", "GET", "/policy", None),
        (
            "PutInvestigationGroupPolicy",
            "POST",
            "/policy",
            Some(json!({ "policy": POLICY })),
        ),
        ("DeleteInvestigationGroupPolicy", "DELETE", "/policy", None),
    ]
}

#[tokio::test]
async fn a_validation_refusal_names_the_member_and_its_constraint_and_echoes_no_value() {
    let router = emulator();
    let allowed: Vec<String> = ENCRYPTION_CONFIGURATION_TYPE_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    let long_name = "n".repeat(513);

    // Rows: (case, method, uri, body, the value the request carried, expected message).
    // The rows differ along the CONSTRAINT CLASS, the member and the operation; the
    // recorded cases (a name outside its pattern, maxResults above its maximum) are two
    // rows among them.
    let cases: Vec<RefusalCase> = vec![
        (
            "a name outside its pattern",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": "invalid!name#with*bad*chars", "roleArn": ROLE_ARN })),
            "invalid!name#with*bad*chars".to_string(),
            envelope(&[clause("name", &pattern_requirement(SERVICE_NAME_PATTERN))]),
        ),
        (
            "a name carrying a path separator",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": "bad/name", "roleArn": ROLE_ARN })),
            "bad/name".to_string(),
            envelope(&[clause("name", &pattern_requirement(SERVICE_NAME_PATTERN))]),
        ),
        (
            "a name one character past its length maximum",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": long_name, "roleArn": ROLE_ARN })),
            long_name.clone(),
            envelope(&[clause(
                "name",
                "Member must have length less than or equal to 512",
            )]),
        ),
        (
            "an empty name violates two constraints at once",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": "", "roleArn": ROLE_ARN })),
            // An empty value cannot be looked for in the message; the clauses are what
            // this row pins.
            "\u{0}".to_string(),
            envelope(&[
                clause("name", "Member must have length greater than or equal to 1"),
                clause("name", &pattern_requirement(SERVICE_NAME_PATTERN)),
            ]),
        ),
        (
            "a required member the request never carried",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "roleArn": ROLE_ARN })),
            "\u{0}".to_string(),
            envelope(&[absent_clause("name")]),
        ),
        (
            "a roleArn outside its pattern",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": "bad-role", "roleArn": "not-an-arn-but-long-enough" })),
            "not-an-arn-but-long-enough".to_string(),
            envelope(&[clause("roleArn", &pattern_requirement(ROLE_ARN_PATTERN))]),
        ),
        (
            "a roleArn below its length minimum",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": "short-role", "roleArn": "arn:aws:iam" })),
            "arn:aws:iam".to_string(),
            envelope(&[clause(
                "roleArn",
                "Member must have length greater than or equal to 20",
            )]),
        ),
        (
            "a retention below its range minimum",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": "r6", "roleArn": ROLE_ARN, "retentionInDays": 6 })),
            "6".to_string(),
            envelope(&[clause(
                "retentionInDays",
                "Member must have value greater than or equal to 7",
            )]),
        ),
        (
            "a retention above its range maximum",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "name": "r91", "roleArn": ROLE_ARN, "retentionInDays": 91 })),
            "91".to_string(),
            envelope(&[clause(
                "retentionInDays",
                "Member must have value less than or equal to 90",
            )]),
        ),
        (
            "an encryption type outside the modelled value set",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({
                "name": "bad-enc",
                "roleArn": ROLE_ARN,
                "encryptionConfiguration": { "type": "MY_OWN_KEY" },
            })),
            "MY_OWN_KEY".to_string(),
            envelope(&[clause(
                "encryptionConfiguration.type",
                &format!("Member must satisfy enum value set: [{}]", allowed.join(", ")),
            )]),
        ),
        (
            "a page size above its range maximum",
            "GET",
            "/investigationGroups?maxResults=999".to_string(),
            None,
            "999".to_string(),
            envelope(&[clause(
                "maxResults",
                "Member must have value less than or equal to 50",
            )]),
        ),
        (
            "a page size below its range minimum",
            "GET",
            "/investigationGroups?maxResults=0".to_string(),
            None,
            "0".to_string(),
            envelope(&[clause(
                "maxResults",
                "Member must have value greater than or equal to 1",
            )]),
        ),
        (
            "a page token this operation never issued",
            "GET",
            "/investigationGroups?nextToken=not-a-token".to_string(),
            None,
            "not-a-token".to_string(),
            envelope(&[clause(
                "nextToken",
                "Member must be a pagination token returned by a previous \
                 ListInvestigationGroups call",
            )]),
        ),
        (
            "an identifier outside its pattern",
            "GET",
            format!("/investigationGroups/{}", label("bad identifier")),
            None,
            "bad identifier".to_string(),
            envelope(&[clause(
                "identifier",
                &pattern_requirement(IDENTIFIER_PATTERN),
            )]),
        ),
        (
            "a required body member the policy request never carried",
            "POST",
            "/investigationGroups/some-group/policy".to_string(),
            Some(json!({})),
            "\u{0}".to_string(),
            envelope(&[absent_clause("policy")]),
        ),
    ];

    for (case, method, uri, body, submitted, expected) in cases {
        let (status, code, message) = call(&router, method, &uri, body.as_ref()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} / {code} / {message:?}"
        );
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code} / {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert!(
            !message.contains(&submitted),
            "case {case:?}: the message must not echo the submitted value {submitted:?}, \
             got {message:?}"
        );
    }
}

#[tokio::test]
async fn the_reported_name_pattern_is_keyed_to_the_expression_the_model_declares() {
    // The override that reports the service's wider name class is keyed on the model's
    // own expression, so this is what keeps the two from drifting apart silently.
    assert_eq!(
        modelled_member_pattern(CREATE_INVESTIGATION_GROUP_INPUT_SCHEMA, "name"),
        Some(MODEL_NAME_PATTERN),
        "the modelled name pattern changed: re-read the recorded service expression \
         before trusting the override"
    );

    // Rows: the member whose live constraint differs from the model's, and three
    // expressions that have no override and so are reported as the model writes them,
    // minus the anchors that make the match total.
    for (case, modelled, expected) in [
        ("the name class the service applies", MODEL_NAME_PATTERN, SERVICE_NAME_PATTERN),
        ("an anchored expression with no override", r"^arn:.*$", ROLE_ARN_PATTERN),
        ("an already unanchored expression", r"[a-z]{1,4}", r"[a-z]{1,4}"),
        ("an escaped trailing dollar is not an anchor", r"^costs\$", r"costs\$"),
    ] {
        let actual = state::applied_pattern(modelled);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn an_absent_investigation_group_is_reported_as_the_absent_resource() {
    let router = emulator();
    let expected = format!("{} does not exist.", state::INVESTIGATION_GROUP_RESOURCE);

    for (operation, method, suffix, body) in group_readers() {
        for (form, identifier) in unknown_identifiers() {
            let uri = format!("/investigationGroups/{}{suffix}", label(&identifier));
            let (status, code, message) = call(&router, method, &uri, body.as_ref()).await;
            let case = format!("{operation} with {form}");
            assert_eq!(
                status, 404,
                "case {case:?}: expected 404, got {status} / {code} / {message:?}"
            );
            assert_eq!(
                code, "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {code} / {message:?}"
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains(&identifier),
                "case {case:?}: the message must not echo the identifier {identifier:?}, \
                 got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_absent_policy_on_a_group_that_exists_is_a_different_resource() {
    let router = emulator();
    let group_absent = format!("{} does not exist.", state::INVESTIGATION_GROUP_RESOURCE);
    let policy_absent = format!(
        "{} does not exist.",
        state::INVESTIGATION_GROUP_POLICY_RESOURCE
    );
    assert_ne!(
        group_absent, policy_absent,
        "the two conditions must be distinguishable, got {group_absent:?} for both"
    );

    // Rows: the same GET policy call against the same group at three points in that
    // group's life, so what the 404 reports tracks the STORE and not the request. The
    // group is addressed by the ARN its create handed back, the only identifier that
    // resolves it.
    let (status, code, before) = call(
        &router,
        "GET",
        "/investigationGroups/policy-probe/policy",
        None,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (404, "ResourceNotFoundException"),
        "case \"before the group exists\": expected a 404, got {before:?}"
    );
    assert_eq!(
        before, group_absent,
        "case \"before the group exists\": expected {group_absent:?}, got {before:?}"
    );

    let (status, code, created) = call(
        &router,
        "POST",
        "/investigationGroups",
        Some(&json!({ "name": "policy-probe", "roleArn": ROLE_ARN })),
    )
    .await;
    assert_eq!(
        status, 201,
        "creating the group: expected 201, got {status} / {code} / {created:?}"
    );
    let response = router
        .dispatch(request(
            "POST",
            "/investigationGroups",
            Some(&json!({ "name": "policy-probe-2", "roleArn": ROLE_ARN })),
        ))
        .await;
    assert_eq!(
        response.status().as_u16(),
        402,
        "a second create in one Region is refused, which is what keeps the probe alone"
    );
    let arn = arn_of(&router, "policy-probe").await;
    let policy_uri = format!("/investigationGroups/{}/policy", label(&arn));

    // A bare label is not an address in this service, so the same call through the
    // group's own name reports the GROUP absent even now that the group exists.
    let (status, code, by_name) = call(
        &router,
        "GET",
        "/investigationGroups/policy-probe/policy",
        None,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (404, "ResourceNotFoundException"),
        "case \"addressed by the bare name\": expected a 404, got {by_name:?}"
    );
    assert_eq!(
        by_name, group_absent,
        "case \"addressed by the bare name\": expected {group_absent:?}, got {by_name:?}"
    );

    let (status, code, after_create) = call(&router, "GET", &policy_uri, None).await;
    assert_eq!(
        (status, code.as_str()),
        (404, "ResourceNotFoundException"),
        "case \"group present, policy absent\": got {after_create:?}"
    );
    assert_eq!(
        after_create, policy_absent,
        "case \"group present, policy absent\": expected {policy_absent:?}, \
         got {after_create:?}"
    );

    // With a policy stored the same call succeeds, which is what shows the 404s above
    // are read off the store rather than wired to the path.
    let (status, code, message) =
        call(&router, "POST", &policy_uri, Some(&json!({ "policy": POLICY }))).await;
    assert_eq!(
        status, 200,
        "putting the policy: expected 200, got {status} / {code} / {message:?}"
    );
    let response = router.dispatch(request("GET", &policy_uri, None)).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "reading the policy back: expected 200"
    );
    let body: Value = serde_json::from_slice(&response.into_body()).expect("a JSON body");
    assert_eq!(
        body.get("policy").and_then(Value::as_str),
        Some(POLICY),
        "reading the policy back: expected the stored document, got {body:?}"
    );

    // And once the group is deleted every reader reports the GROUP again.
    let (status, code, message) = call(
        &router,
        "DELETE",
        &format!("/investigationGroups/{}", label(&arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting the group: expected 200, got {status} / {code} / {message:?}"
    );
    let (status, _, after_delete) = call(&router, "GET", &policy_uri, None).await;
    assert_eq!(
        (status, after_delete.as_str()),
        (404, group_absent.as_str()),
        "case \"after the group is deleted\": expected {group_absent:?}, got {after_delete:?}"
    );
}

/// The subject a clause takes when the request carried NOTHING at that path, which is
/// the one condition it states.
const ABSENT_SUBJECT: &str = "Value null at";

/// One row of the subject table: the case name, the request (method, URI, body), the
/// value the request carried at the path the refusal names (`None` where it carried
/// none, which is the absent-member case), and the message the service must answer with.
type SubjectCase = (
    &'static str,
    &'static str,
    String,
    Option<Value>,
    Option<String>,
    String,
);

/// The requirement the HANDLER states for a page token it did not issue — the layer
/// below the boundary walk, which words the same member without a value either.
const PAGE_TOKEN_REQUIREMENT: &str =
    "Member must be a pagination token returned by a previous ListInvestigationGroups call";

#[tokio::test]
async fn a_withheld_sensitive_value_is_named_as_a_present_member_and_only_absence_is_null() {
    let router = emulator();

    // The premise is read off the MODEL, not written here: the page token is a member a
    // request carries whose violations are withheld, and the length maximum a boundary
    // refusal of it states is the model's.
    assert!(
        member_is_sensitive(LIST_INVESTIGATION_GROUPS_INPUT_SCHEMA, "nextToken"),
        "the page token is the member whose violation values are withheld; the model no \
         longer marks it @sensitive, so this suite is pinning nothing"
    );
    let max = modelled_member_length_max(LIST_INVESTIGATION_GROUPS_INPUT_SCHEMA, "nextToken")
        .and_then(|max| usize::try_from(max).ok())
        .expect("the model declares a length maximum for the page token");
    let length_requirement = format!("Member must have length less than or equal to {max}");
    let one_past_the_bound = "a".repeat(max + 1);
    let far_past_the_bound = "b".repeat(max * 2 + 1);
    let at_the_bound = "c".repeat(max);

    // Rows: (case, method, uri, body, the value the request carried at the refused path —
    // `None` where it carried none — expected message). They differ along WHETHER the
    // request carried a value there, how far past the bound the withheld value was, and
    // which layer worded the refusal (the boundary walk for a bound, the handler for its
    // own token rule) — so the subject is decided by the request and not by the layer or
    // by the value being shown.
    let cases: Vec<SubjectCase> = vec![
        (
            "a withheld page token one character past its length maximum",
            "GET",
            format!("/investigationGroups?nextToken={one_past_the_bound}"),
            None,
            Some(one_past_the_bound.clone()),
            envelope(&[clause("nextToken", &length_requirement)]),
        ),
        (
            "a withheld page token far past its length maximum",
            "GET",
            format!("/investigationGroups?nextToken={far_past_the_bound}"),
            None,
            Some(far_past_the_bound.clone()),
            envelope(&[clause("nextToken", &length_requirement)]),
        ),
        (
            "a withheld page token at its length maximum, which the handler refuses",
            "GET",
            format!("/investigationGroups?nextToken={at_the_bound}"),
            None,
            Some(at_the_bound.clone()),
            envelope(&[clause("nextToken", PAGE_TOKEN_REQUIREMENT)]),
        ),
        (
            "a short withheld page token no page issued",
            "GET",
            "/investigationGroups?nextToken=not-a-token".to_string(),
            None,
            Some("not-a-token".to_string()),
            envelope(&[clause("nextToken", PAGE_TOKEN_REQUIREMENT)]),
        ),
        (
            "a required member the request never carried, the one absent subject",
            "POST",
            "/investigationGroups".to_string(),
            Some(json!({ "roleArn": ROLE_ARN })),
            None,
            envelope(&[absent_clause("name")]),
        ),
    ];

    for (case, method, uri, body, submitted, expected) in cases {
        let (status, code, message) = call(&router, method, &uri, body.as_ref()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} / {code} / {message:?}"
        );
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code} / {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert_eq!(
            message.contains(ABSENT_SUBJECT),
            submitted.is_none(),
            "case {case:?}: the {ABSENT_SUBJECT:?} subject states that the request carried \
             nothing at that path, got {message:?}"
        );
        // Only a row that carried a value has one the message could echo.
        if let Some(submitted) = submitted.as_deref() {
            assert!(
                !message.contains(submitted),
                "case {case:?}: the message must not echo the submitted value {submitted:?}, \
                 got {message:?}"
            );
        }
    }

    // One request breaking a withheld member's bound AND a shown member's: both clauses
    // name a present member, in one envelope.
    let (status, code, message) = call(
        &router,
        "GET",
        &format!("/investigationGroups?nextToken={one_past_the_bound}&maxResults=999"),
        None,
    )
    .await;
    let case = "a withheld page token past its bound beside a page size past its own";
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected a 400 ValidationException, got {status} / {code} / {message:?}"
    );
    for expected_clause in [
        clause("nextToken", &length_requirement),
        clause(
            "maxResults",
            "Member must have value less than or equal to 50",
        ),
    ] {
        assert!(
            message.contains(&expected_clause),
            "case {case:?}: expected the clause {expected_clause:?}, got {message:?}"
        );
    }
    assert!(
        !message.contains(ABSENT_SUBJECT),
        "case {case:?}: neither member was absent, got {message:?}"
    );
    assert!(
        !message.contains(&one_past_the_bound),
        "case {case:?}: the message must not echo the submitted token, got {message:?}"
    );
}

#[test]
fn a_withheld_value_takes_the_present_member_subject_whatever_it_violated() {
    // The wire path above can only break the ONE bound this model gives its withheld
    // member, so the rule is pinned here over every constraint class a violation can
    // carry, each handed to the refusal renderer exactly as the walk records it: a
    // withheld value (`sensitive`, no value to show), a value the walk echoes, and no
    // value at all.
    //
    // Rows: (case, the violation as recorded, the expected message).
    let cases = [
        (
            "a withheld value past a length maximum",
            Violation {
                path: "nextToken".to_string(),
                kind: ViolationKind::MaxLength(2048),
                value: None,
                sensitive: true,
            },
            clause(
                "nextToken",
                "Member must have length less than or equal to 2048",
            ),
        ),
        (
            "a withheld value below a length minimum",
            Violation {
                path: "nextToken".to_string(),
                kind: ViolationKind::MinLength(1),
                value: None,
                sensitive: true,
            },
            clause(
                "nextToken",
                "Member must have length greater than or equal to 1",
            ),
        ),
        (
            "a withheld value outside a pattern",
            Violation {
                path: "nextToken".to_string(),
                kind: ViolationKind::Pattern(r"^[a-z]+$".to_string()),
                value: None,
                sensitive: true,
            },
            clause(
                "nextToken",
                &pattern_requirement("[a-z]+"),
            ),
        ),
        (
            "a withheld value outside an enum value set",
            Violation {
                path: "encryptionConfiguration.type".to_string(),
                kind: ViolationKind::EnumValue(vec!["AWS_OWNED_KEY".to_string()]),
                value: None,
                sensitive: true,
            },
            clause(
                "encryptionConfiguration.type",
                "Member must satisfy enum value set: [AWS_OWNED_KEY]",
            ),
        ),
        (
            "a withheld value under a range minimum",
            Violation {
                path: "retentionInDays".to_string(),
                kind: ViolationKind::MinValue("7".to_string()),
                value: None,
                sensitive: true,
            },
            clause(
                "retentionInDays",
                "Member must have value greater than or equal to 7",
            ),
        ),
        (
            "a withheld value inside a list element",
            Violation {
                path: "crossAccountConfigurations.1.member.sourceRoleArn".to_string(),
                kind: ViolationKind::MaxLength(2048),
                value: None,
                sensitive: true,
            },
            clause(
                "crossAccountConfigurations.1.member.sourceRoleArn",
                "Member must have length less than or equal to 2048",
            ),
        ),
        (
            "a value the walk echoes rather than withholds",
            Violation {
                path: "name".to_string(),
                kind: ViolationKind::MaxLength(512),
                value: Some("n".repeat(513)),
                sensitive: false,
            },
            clause("name", "Member must have length less than or equal to 512"),
        ),
        (
            "no value at that path at all",
            Violation {
                path: "name".to_string(),
                kind: ViolationKind::MissingRequiredMember,
                value: None,
                sensitive: false,
            },
            absent_clause("name"),
        ),
    ];

    for (case, violation, expected_clause) in cases {
        let expected = envelope(std::slice::from_ref(&expected_clause));
        let message = state::validation_exception_message(std::slice::from_ref(&violation));
        assert_eq!(
            message.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        let actual = message.unwrap_or_default();
        assert_eq!(
            actual.contains(ABSENT_SUBJECT),
            violation.value.is_none() && !violation.sensitive,
            "case {case:?}: the {ABSENT_SUBJECT:?} subject states that the request carried \
             nothing at that path, got {actual:?}"
        );
        if let Some(shown) = violation.value.as_deref() {
            assert!(
                !actual.contains(shown),
                "case {case:?}: no clause echoes the value it refused, got {actual:?}"
            );
        }
    }
}
