// SPDX-License-Identifier: CC0-1.0

//! What EVERY failing DataZone answer carries in its BODY, whichever renderer wrote it.
//!
//! RULE 1 — the MEMBERS: this service speaks `restJson1`
//! (`smithy_query(query="service_info")` reports protocols `[restJson1]`), and a
//! `restJson1` failure names its error SHAPE in the
//! `x-amzn-errortype` response header, so the body of a failure carries the error shape's
//! own members and nothing else — no `__type` discriminator (that member belongs to the
//! AWS JSON protocol family, which conveys the shape in the body because it has no such
//! header), and no member written as the empty string (on the wire that is an absent
//! member, not a present empty one).
//!
//! The rows below differ along the one thing that decides WHICH renderer writes the body:
//! a failure a HANDLER raised, serialized by this service's own protocol, and a failure
//! the request never reached a handler for because it named no modelled operation,
//! rendered by the routing seam. The rule is one rule, so one table asserts it over both,
//! and it holds for the error classes either seam can answer with.
//!
//! RULE 2 — the TEXT a refusal of the INPUT carries in that `message` member: the AWS
//! validation envelope over one clause per violation, where a modelled `@pattern` is
//! reported in WHOLE-VALUE form (each outer anchor the modelled find expression carries is
//! dropped, each one it lacks is spelled out as `.*`) and a `@sensitive` member's value is
//! withheld, so the clause names the member alone. The rows differ along what the rule
//! generalises over: whether the member is `@sensitive`, how the modelled pattern is
//! anchored, where the member arrives (a path label or the body), which constraint it
//! violates, and which stage checks it — the request boundary, or the handler, for the one
//! pattern the boundary's regex engine cannot compile.
//!
//! The harness (`Emulator`, `Answer`) is the one every suite in this crate drives the
//! service with.

use crate::harness::Emulator;
use serde_json::{json, Value};

/// The member a JSON error envelope of the AWS JSON protocol family names its error shape
/// in — the one `restJson1` names in a header instead, so no body here may carry it.
const SHAPE_DISCRIMINATOR: &str = "__type";

/// A domain identifier no domain in the store carries, satisfying `DomainId`'s modelled
/// pattern `^dzd[-_][a-zA-Z0-9_-]{1,36}$` (smithy_query shape="DomainId") so that the
/// request reaches a handler rather than being refused before one runs.
const UNSERVED_DOMAIN: &str = "dzd_noenvelopedomain0";

/// One failing call: how it is addressed, and the body members its answer must carry.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    members: &'static [&'static str],
}

/// The body members of an answer, sorted, so an assertion names exactly what went on the
/// wire.
fn members(body: &Value) -> Vec<String> {
    match body.as_object() {
        Some(object) => {
            let mut names: Vec<String> = object.keys().cloned().collect();
            names.sort();
            names
        }
        None => vec![format!("<not an object: {body}>")],
    }
}

#[tokio::test]
async fn every_failure_body_carries_the_error_shapes_own_members_whichever_renderer_wrote_it() {
    let dz = Emulator::new();

    let rows = vec![
        // Rendered by the ROUTING seam: the request reached this service, in its
        // protocol, but named no modelled operation, so no handler ran.
        Row {
            case: "a path no operation is bound to",
            method: "GET",
            uri: "/v2/not-a-datazone-resource".to_string(),
            body: None,
            members: &["message"],
        },
        Row {
            case: "a method no operation binds on a bound path",
            method: "PUT",
            uri: "/v2/domains".to_string(),
            body: Some(json!({ "name": "envelope domain" })),
            members: &["message"],
        },
        Row {
            case: "a path one segment longer than a bound one",
            method: "GET",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}/projects/p0/deeper"),
            body: None,
            members: &["message"],
        },
        Row {
            case: "the root path",
            method: "GET",
            uri: "/".to_string(),
            body: None,
            members: &["message"],
        },
        // Serialized by this service's own protocol: a handler ran and raised a
        // modelled error.
        Row {
            case: "a miss a handler raised, naming the domain it could not resolve",
            method: "POST",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}/projects"),
            body: Some(json!({ "name": "project in no domain" })),
            members: &["message"],
        },
        Row {
            case: "a denial a handler raised, whose own identifier IS the domain",
            method: "DELETE",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}"),
            body: None,
            members: &[],
        },
    ];

    for Row {
        case,
        method,
        uri,
        body,
        members: expected,
    } in rows
    {
        let answer = dz.call(method, &uri, body).await;
        assert!(
            answer.status >= 400,
            "case {case:?}: expected a failure, got {} {:?}",
            answer.status,
            answer.body
        );
        assert!(
            answer.body.get(SHAPE_DISCRIMINATOR).is_none(),
            "case {case:?}: the error shape travels in the header, got {:?}",
            answer.body
        );
        // `error_code` is read from `x-amzn-errortype`, falling back to the body's
        // discriminator — which the assertion above proved absent — so a code here came
        // from the header this protocol resolves the shape from.
        assert!(
            !answer.error_code.is_empty(),
            "case {case:?}: expected the error shape in the header, got {:?} {:?}",
            answer.error_code,
            answer.body
        );
        let on_the_wire = members(&answer.body);
        assert_eq!(
            on_the_wire,
            expected
                .iter()
                .map(|member| (*member).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected exactly these members, got {:?}",
            answer.body
        );
        for member in &on_the_wire {
            let value = answer.body.get(member).and_then(Value::as_str);
            assert!(
                value.is_none_or(|text| !text.is_empty()),
                "case {case:?}: {member:?} must be absent rather than empty, got {:?}",
                answer.body
            );
        }
    }

    // A SUCCESS is not an error envelope at all: it keeps every member the model gives
    // it, including one the rule above would have dropped from a failure.
    let created = dz.create_domain(json!({ "name": "envelope" })).await;
    assert_eq!(
        created.status, 201,
        "CreateDomain: expected 201, got {} {:?}",
        created.status, created.body
    );
    assert!(
        created.body.get("id").is_some(),
        "a success must carry its own members, got {:?}",
        created.body
    );
}

/// One refusal of the input provoked below: the call that provokes it, the member the
/// clause has to name, the value that clause may show (`None` for a `@sensitive` member,
/// whose value a refusal withholds) and the modelled `@pattern` in the form the refusal
/// reports it.
struct PatternRow {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    member: &'static str,
    shown: Option<&'static str>,
    reported_pattern: &'static str,
}

/// The `message` a validation refusal of ONE member that violated its `@pattern` carries:
/// fixed text whose only variable parts are the member, the value when there is one to
/// show, and the reported expression.
fn expected_pattern_message(member: &str, shown: Option<&str>, reported_pattern: &str) -> String {
    let value = match shown {
        Some(value) => format!("Value '{value}' "),
        None => "Value ".to_string(),
    };
    format!(
        "1 validation error detected: {value}at '{member}' failed to satisfy constraint: \
         Member must satisfy regular expression pattern: {reported_pattern}"
    )
}

/// Every call the pattern half of RULE 2 is asserted over. SOURCE for every modelled
/// pattern and every `@sensitive` marker named here:
/// `smithy_query(query="operation", operation="CreateAccountPool,CreateProjectProfile,CreateEnvironmentProfile,CreateAssetFilter,CreateFormType,ListProjects")`.
fn pattern_rows() -> Vec<PatternRow> {
    // `AccountPoolName`, `ProjectProfileName`, `EnvironmentProfileName` and `FilterName`
    // are each 1..64 of `^[\w -]+$` and each `@sensitive`, so the value is withheld and
    // the anchors are dropped. `!` is in none of their classes and every name below is
    // within the length bound, so the pattern is the only constraint violated.
    let reported_name_pattern = r"[\w -]+";
    vec![
        PatternRow {
            case: "a sensitive body member of a create (CreateAccountPool)",
            method: "POST",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}/account-pools"),
            body: Some(json!({
                "name": "bad pool name !!",
                "resolutionStrategy": "MANUAL",
                "accountSource": {
                    "accounts": [
                        { "awsAccountId": "111122223333", "supportedRegions": ["us-east-1"] }
                    ]
                },
            })),
            member: "name",
            shown: None,
            reported_pattern: reported_name_pattern,
        },
        PatternRow {
            case: "the same member on a create that takes nothing else (CreateProjectProfile)",
            method: "POST",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}/project-profiles"),
            body: Some(json!({ "name": "bad profile name !!" })),
            member: "name",
            shown: None,
            reported_pattern: reported_name_pattern,
        },
        PatternRow {
            case: "the same member beside other constrained members (CreateEnvironmentProfile)",
            method: "POST",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}/environment-profiles"),
            body: Some(json!({
                "name": "bad environment name !!",
                "environmentBlueprintIdentifier": "blueprint1",
                "projectIdentifier": "project1",
            })),
            member: "name",
            shown: None,
            reported_pattern: reported_name_pattern,
        },
        PatternRow {
            case: "the same member on a create addressed by two path labels (CreateAssetFilter)",
            method: "POST",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}/assets/someasset/filters"),
            body: Some(json!({
                "name": "bad filter name !!",
                "configuration": { "columnConfiguration": { "includedColumnNames": ["amount"] } },
            })),
            member: "name",
            shown: None,
            reported_pattern: reported_name_pattern,
        },
        // A member that is NOT `@sensitive`, and arrives in a PATH LABEL rather than the
        // body: `DomainId` is `^dzd[-_][a-zA-Z0-9_-]{1,36}$`, so the value IS shown and
        // only the anchors go. `.` is outside the tail's class.
        PatternRow {
            case: "a non-sensitive path label (ListProjects)",
            method: "GET",
            uri: "/v2/domains/dzd_bad.scope/projects".to_string(),
            body: None,
            member: "domainIdentifier",
            shown: Some("dzd_bad.scope"),
            reported_pattern: r"dzd[-_][a-zA-Z0-9_-]{1,36}",
        },
        // The one pattern the request boundary cannot check — `FormTypeName` needs
        // look-around — so this clause is written by the HANDLER and has to read exactly
        // like every clause the boundary writes. Only the OUTERMOST anchors are the
        // expression's own: the ones inside its alternation stay. `0` is excluded by the
        // look-ahead, and the name is within 1..128.
        PatternRow {
            case: "a sensitive member the handler checks (CreateFormType)",
            method: "POST",
            uri: format!("/v2/domains/{UNSERVED_DOMAIN}/form-types"),
            body: Some(json!({
                "name": "0badformtype",
                "model": { "smithy": "structure SomeForm {}" },
                "owningProjectIdentifier": "project1",
            })),
            member: "name",
            shown: None,
            reported_pattern: r"(amazon.datazone.)?(?![0-9_])\w+$|^_\w*[a-zA-Z0-9]\w*",
        },
    ]
}

#[tokio::test]
async fn a_pattern_refusal_reports_the_modelled_pattern_in_whole_value_form() {
    let dz = Emulator::new();

    for PatternRow {
        case,
        method,
        uri,
        body,
        member,
        shown,
        reported_pattern,
    } in pattern_rows()
    {
        let answer = dz.call(method, &uri, body).await;
        assert_eq!(
            (answer.status, answer.error_code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a value outside its modelled pattern is refused as input, got \
             {} {:?}",
            answer.status,
            answer.body
        );
        let expected = expected_pattern_message(member, shown, reported_pattern);
        let message = answer.s("message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn an_unanchored_pattern_is_reported_with_both_sides_spelled_out() {
    let dz = Emulator::new();

    // `GroupIdentifier` is the member of this model whose `@pattern` is anchored at
    // NEITHER end (smithy_query shape="GroupIdentifier"), and it is not `@sensitive`, so
    // the value is shown and what the find expression admitted on either side of its match
    // is spelled out around the modelled expression — whose own inner anchors stay. The
    // expression itself is not retyped here: that is the model's, and this rule is about
    // what is added around it.
    let answer = dz
        .call(
            "POST",
            &format!("/v2/domains/{UNSERVED_DOMAIN}/group-profiles"),
            Some(json!({ "groupIdentifier": "" })),
        )
        .await;
    assert_eq!(
        (answer.status, answer.error_code.as_str()),
        (400, "ValidationException"),
        "CreateGroupProfile: a value outside its modelled pattern is refused as input, got \
         {} {:?}",
        answer.status,
        answer.body
    );
    let message = answer.s("message");
    let prefix = "1 validation error detected: Value '' at 'groupIdentifier' failed to satisfy \
                  constraint: Member must satisfy regular expression pattern: .*(^";
    assert!(
        message.starts_with(prefix),
        "expected a message opening {prefix:?}, got {message:?}"
    );
    assert!(
        message.ends_with(").*"),
        "expected the reported expression to spell out its trailing side, got {message:?}"
    );
}

#[tokio::test]
async fn a_length_refusal_of_a_sensitive_member_withholds_the_value_it_refused() {
    let dz = Emulator::new();

    // `RequestReason` is 1..4096 and `@sensitive`
    // (smithy_query(query="operation", operation="CreateSubscriptionRequest")), so the
    // clause names the member and the bound it broke and carries nothing of the payload.
    let oversized = "r".repeat(5000);
    let answer = dz
        .call(
            "POST",
            &format!("/v2/domains/{UNSERVED_DOMAIN}/subscription-requests"),
            Some(json!({
                "subscribedPrincipals": [{ "project": { "identifier": "someproject" } }],
                "subscribedListings": [{ "identifier": "somelisting" }],
                "requestReason": oversized,
            })),
        )
        .await;
    assert_eq!(
        (answer.status, answer.error_code.as_str()),
        (400, "ValidationException"),
        "CreateSubscriptionRequest: a value past its modelled bound is refused as input, got \
         {} {:?}",
        answer.status,
        answer.body
    );
    let message = answer.s("message");
    let expected = "1 validation error detected: Value at 'requestReason' failed to satisfy \
                    constraint: Member must have length less than or equal to 4096";
    assert_eq!(
        message, expected,
        "expected {expected:?}, got {message:?}"
    );
}
