// SPDX-License-Identifier: CC0-1.0

//! How a request BODY that breaks a modelled constraint is refused, and how that differs
//! from a value the request carries outside its body.
//!
//! THE RULE: the layer in front of this service validates the request's JSON body against
//! the operation's modelled input as a SCHEMA, so a body member outside its declared
//! constraint is refused with HTTP 400 `ValidationException` whose message is that
//! validator's bracketed list of findings — one per constraint the body broke, each naming
//! the rule the value failed and the value that failed it, with NO member path and NO
//! error-count prefix. A value bound to the request URI (`@httpLabel`, `@httpQuery`) is
//! refused by the service itself, in the framework's `N validation error(s) detected: Value
//! '<value>' at '<member path>' failed to satisfy constraint: …` envelope, which the
//! sibling suites ([`super::pattern_refusal_messages`],
//! [`super::arn_pattern_refusal_clauses`]) pin.
//!
//! The rows span what the rule generalises over: the constraint CLASS (a `@pattern`, a list
//! `@length` minimum, a string `@length` maximum, an enumerated type's value set), the
//! operation (five of them, each binding its members differently), the DEPTH the member
//! sits at (a top-level member, a member of a nested structure, an element of a list inside
//! one), one and two broken members in a single request, and — for the one member this
//! service binds both ways (`GroupSecurityIdentifier`, a body member of the create and a
//! path segment of the read) — both halves of the split. Every expression, bound and value
//! set is quoted from this service's own model
//! (`smithy_query(query="operation",operation="CreateConnector,CreateDirectoryRegistration,\
//! CreateServicePrincipalName,CreateTemplateGroupAccessControlEntry")` and
//! `smithy_query(query="shape",shape="AccessRight,ClientToken,DisplayName,\
//! SecurityGroupIdList,VpcInformation")`), and each expected finding is COMPUTED from it
//! rather than written out.

use serde_json::{json, Value};

use super::harness::{call, encode_label, router, ACCOUNT, REGION};
use super::pattern_refusal_messages::{
    advertised, well_formed_connector_arn, well_formed_directory_registration_arn,
    DIRECTORY_REGISTRATION_ARN_PATTERN, PATTERN_REQUIREMENT,
};

/// `DirectoryId`'s modelled expression, as the model declares it.
pub const DIRECTORY_ID_PATTERN: &str = r"^d-[0-9a-f]{10}$";

/// `CertificateAuthorityArn`'s modelled expression.
pub const CERTIFICATE_AUTHORITY_ARN_PATTERN: &str =
    r"^arn:[\w-]+:acm-pca:[\w-]+:[0-9]+:certificate-authority\/[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$";

/// `SecurityGroupId`'s modelled expression — the list's ELEMENT, not the list.
pub const SECURITY_GROUP_ID_PATTERN: &str = r"^(?:sg-[0-9a-f]{8}|sg-[0-9a-f]{17})$";

/// `GroupSecurityIdentifier`'s modelled expression.
const GROUP_SID_PATTERN: &str = r"^S-[0-9]-([0-9]+-){1,14}[0-9]+$";

/// `DisplayName`'s modelled expression, which `GroupDisplayName` targets.
const DISPLAY_NAME_PATTERN: &str = r"^[\x20-\x7E]+$";

/// `ClientToken`'s modelled expression and its `@length` maximum.
const CLIENT_TOKEN_PATTERN: &str = r"^[!-~]+$";
const CLIENT_TOKEN_MAX_LENGTH: usize = 64;

/// `SecurityGroupIds`' modelled `@length` minimum.
pub const SECURITY_GROUP_IDS_MIN: usize = 1;

/// The values `AccessRight` admits, in the order the model declares them.
const ACCESS_RIGHT_VALUES: [&str; 2] = ["ALLOW", "DENY"];

/// A directory id `DirectoryId`'s expression admits.
const WELL_FORMED_DIRECTORY_ID: &str = "d-0123456789";

/// A security group id the list element's expression admits.
const WELL_FORMED_SECURITY_GROUP: &str = "sg-0123456789abcdef0";

/// A security identifier `GroupSecurityIdentifier`'s expression admits.
const WELL_FORMED_SID: &str = "S-1-5-21-1004336348-1177238915-682003330-512";

/// A certificate-authority ARN `CertificateAuthorityArn`'s expression admits.
fn well_formed_ca_arn() -> String {
    format!(
        "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
         11111111-2222-3333-4444-555555555555"
    )
}

/// A template ARN `TemplateArn`'s expression admits.
fn well_formed_template_arn() -> String {
    format!(
        "arn:aws:pca-connector-ad:{REGION}:{ACCOUNT}:connector/\
         11111111-2222-3333-4444-555555555555/template/66666666-7777-8888-9999-aaaaaaaaaaaa"
    )
}

/// One finding of the bracketed list: a string outside the expression its schema declares,
/// which quotes the expression as the MODEL writes it — anchors included, because this is
/// the schema's own expression and not the per-member clause's rendering of it.
pub fn regex_finding(pattern: &str, value: &str) -> String {
    format!("ECMA 262 regex \"{pattern}\" does not match input string \"{value}\"")
}

/// One finding for an ARRAY carrying fewer elements than its schema admits.
pub fn array_finding(min: usize, found: usize) -> String {
    format!("array is too short: must have at least {min} elements but instance has {found} elements")
}

/// One finding for a string longer than its schema admits.
fn too_long_finding(value: &str, max: usize) -> String {
    format!(
        "string \"{value}\" is too long (length: {}, maximum allowed: {max})",
        value.chars().count()
    )
}

/// One finding for a value outside the set its enumerated type admits.
fn enum_finding(value: &str, permitted: &[&str]) -> String {
    let quoted: Vec<String> = permitted
        .iter()
        .map(|admitted| format!("\"{admitted}\""))
        .collect();
    format!(
        "instance value (\"{value}\") not found in enum (possible values: [{}])",
        quoted.join(",")
    )
}

/// The whole message a body refusal states: the validator's bracketed list over the
/// findings, in the order the operation's input declares the members that carried them.
pub fn report(findings: &[String]) -> String {
    format!("[{}]", findings.join(", "))
}

/// A `CreateConnector` body every member of which is admitted, for a row to break exactly
/// one thing in.
fn connector_body() -> Value {
    json!({
        "DirectoryId": WELL_FORMED_DIRECTORY_ID,
        "CertificateAuthorityArn": well_formed_ca_arn(),
        "VpcInformation": { "SecurityGroupIds": [WELL_FORMED_SECURITY_GROUP] },
    })
}

/// A `CreateTemplateGroupAccessControlEntry` body every member of which is admitted.
fn access_control_entry_body() -> Value {
    json!({
        "GroupSecurityIdentifier": WELL_FORMED_SID,
        "GroupDisplayName": "Domain Admins",
        "AccessRights": { "Enroll": "ALLOW", "AutoEnroll": "DENY" },
    })
}

/// One refusal: a request whose BODY carries values a modelled constraint does not admit.
struct BodyCase {
    /// Why this row is here: the operation, the member(s), and the way each value misses.
    case: &'static str,
    method: &'static str,
    /// The request target, already encoded; every value in it is one the member admits, so
    /// the only thing wrong with the request is its body.
    target: String,
    body: Value,
    /// The findings the message states, in the order it states them.
    findings: Vec<String>,
    /// The same request with every offending value replaced by one its member admits, so
    /// each row also pins that the CHECK still runs the model's own constraint.
    accepted_body: Value,
}

/// The rows.
fn body_cases() -> Vec<BodyCase> {
    let template_arn = well_formed_template_arn();
    let entries_of_template = format!(
        "/templates/{}/accessControlEntries",
        encode_label(&template_arn)
    );
    let name_of_registration = format!(
        "/directoryRegistrations/{}/servicePrincipalNames/{}",
        encode_label(&well_formed_directory_registration_arn()),
        encode_label(&well_formed_connector_arn())
    );
    // A client token one character past the modelled maximum, out of characters the
    // token's own expression admits, so the only requirement it breaks is the length.
    let over_long_token = "t".repeat(CLIENT_TOKEN_MAX_LENGTH + 1);
    vec![
        BodyCase {
            case: "CreateConnector, a word for the directory id",
            method: "POST",
            target: "/connectors".to_string(),
            body: {
                let mut body = connector_body();
                body["DirectoryId"] = json!("not-a-valid-directory-id");
                body
            },
            findings: vec![regex_finding(
                DIRECTORY_ID_PATTERN,
                "not-a-valid-directory-id",
            )],
            accepted_body: connector_body(),
        },
        BodyCase {
            case: "CreateConnector, a word for the certificate authority ARN",
            method: "POST",
            target: "/connectors".to_string(),
            body: {
                let mut body = connector_body();
                body["CertificateAuthorityArn"] = json!("not-an-acm-pca-arn");
                body
            },
            findings: vec![regex_finding(
                CERTIFICATE_AUTHORITY_ARN_PATTERN,
                "not-an-acm-pca-arn",
            )],
            accepted_body: connector_body(),
        },
        BodyCase {
            case: "CreateConnector, no security group at all",
            method: "POST",
            target: "/connectors".to_string(),
            body: {
                let mut body = connector_body();
                body["VpcInformation"]["SecurityGroupIds"] = json!([]);
                body
            },
            findings: vec![array_finding(SECURITY_GROUP_IDS_MIN, 0)],
            accepted_body: connector_body(),
        },
        BodyCase {
            case: "CreateConnector, a word for a security group id (an element of the list \
                   inside the nested structure)",
            method: "POST",
            target: "/connectors".to_string(),
            body: {
                let mut body = connector_body();
                body["VpcInformation"]["SecurityGroupIds"] = json!(["sg-not-a-group"]);
                body
            },
            findings: vec![regex_finding(SECURITY_GROUP_ID_PATTERN, "sg-not-a-group")],
            accepted_body: connector_body(),
        },
        BodyCase {
            case: "CreateConnector, a client token past the modelled maximum",
            method: "POST",
            target: "/connectors".to_string(),
            body: {
                let mut body = connector_body();
                body["ClientToken"] = json!(over_long_token);
                body
            },
            findings: vec![too_long_finding(&over_long_token, CLIENT_TOKEN_MAX_LENGTH)],
            accepted_body: connector_body(),
        },
        BodyCase {
            case: "CreateDirectoryRegistration, a word for the directory id",
            method: "POST",
            target: "/directoryRegistrations".to_string(),
            body: json!({ "DirectoryId": "not-a-valid-directory-id" }),
            findings: vec![regex_finding(
                DIRECTORY_ID_PATTERN,
                "not-a-valid-directory-id",
            )],
            accepted_body: json!({ "DirectoryId": WELL_FORMED_DIRECTORY_ID }),
        },
        BodyCase {
            case: "CreateServicePrincipalName, a client token the expression does not admit, \
                   where the same operation's ARN members travel in the URI",
            method: "POST",
            target: name_of_registration,
            body: json!({ "ClientToken": "token with spaces" }),
            findings: vec![regex_finding(CLIENT_TOKEN_PATTERN, "token with spaces")],
            accepted_body: json!({ "ClientToken": "token-without-spaces" }),
        },
        BodyCase {
            case: "CreateTemplateGroupAccessControlEntry, a word for the security identifier \
                   it takes in its BODY",
            method: "POST",
            target: entries_of_template.clone(),
            body: {
                let mut body = access_control_entry_body();
                body["GroupSecurityIdentifier"] = json!("not-a-valid-sid");
                body
            },
            findings: vec![regex_finding(GROUP_SID_PATTERN, "not-a-valid-sid")],
            accepted_body: access_control_entry_body(),
        },
        BodyCase {
            case: "CreateTemplateGroupAccessControlEntry, a value the access right's \
                   enumerated type does not admit",
            method: "POST",
            target: entries_of_template.clone(),
            body: {
                let mut body = access_control_entry_body();
                body["AccessRights"]["Enroll"] = json!("MAYBE");
                body
            },
            findings: vec![enum_finding("MAYBE", &ACCESS_RIGHT_VALUES)],
            accepted_body: access_control_entry_body(),
        },
        BodyCase {
            case: "CreateTemplateGroupAccessControlEntry, a display name carrying a \
                   character outside the printable range",
            method: "POST",
            target: entries_of_template.clone(),
            body: {
                let mut body = access_control_entry_body();
                body["GroupDisplayName"] = json!("Domain\tAdmins");
                body
            },
            findings: vec![regex_finding(DISPLAY_NAME_PATTERN, "Domain\tAdmins")],
            accepted_body: access_control_entry_body(),
        },
        BodyCase {
            case: "CreateTemplateGroupAccessControlEntry, two broken members at once",
            method: "POST",
            target: entries_of_template,
            body: {
                let mut body = access_control_entry_body();
                body["GroupSecurityIdentifier"] = json!("not-a-valid-sid");
                body["AccessRights"]["AutoEnroll"] = json!("PERHAPS");
                body
            },
            findings: vec![
                regex_finding(GROUP_SID_PATTERN, "not-a-valid-sid"),
                enum_finding("PERHAPS", &ACCESS_RIGHT_VALUES),
            ],
            accepted_body: access_control_entry_body(),
        },
    ]
}

#[tokio::test]
async fn a_body_constraint_refusal_states_the_validators_bracketed_findings() {
    let router = router();
    for row in body_cases() {
        let (status, code, parsed) =
            call(&router, row.method, &row.target, Some(&row.body)).await;
        assert_eq!(
            status, 400,
            "case {:?}: a body outside a modelled constraint is refused with 400, \
             got {status} / {code:?} / {parsed}",
            row.case
        );
        assert_eq!(
            code, "ValidationException",
            "case {:?}: expected ValidationException, got {code:?} / {parsed}",
            row.case
        );
        let expected = report(&row.findings);
        assert_eq!(
            parsed["message"].as_str(),
            Some(expected.as_str()),
            "case {:?}: expected message {expected:?}, got {parsed}",
            row.case
        );
    }
}

/// The findings are the WHOLE message: a body refusal carries none of the per-member
/// envelope's parts — no error count, no member path and no `Member must …` requirement.
#[tokio::test]
async fn a_body_constraint_refusal_states_no_part_of_the_per_member_envelope() {
    let router = router();
    for row in body_cases() {
        let (_, _, parsed) = call(&router, row.method, &row.target, Some(&row.body)).await;
        let message = parsed["message"].as_str().unwrap_or_default().to_string();
        for absent in [
            "validation error",
            "failed to satisfy constraint",
            PATTERN_REQUIREMENT,
        ] {
            assert!(
                !message.contains(absent),
                "case {:?}: a body finding states no {absent:?}, got {message:?}",
                row.case
            );
        }
        assert!(
            message.starts_with('[') && message.ends_with(']'),
            "case {:?}: the findings are listed in one pair of brackets, got {message:?}",
            row.case
        );
    }
}

/// Only the WORDING is this layer's: the check is still the model's own constraint, so a
/// body whose values every constraint admits is never refused for one — it earns no 400
/// `ValidationException` at all, whatever the operation then answers for the resources it
/// names.
#[tokio::test]
async fn a_body_every_constraint_admits_is_never_refused_for_one() {
    let router = router();
    for row in body_cases() {
        let (status, code, parsed) =
            call(&router, row.method, &row.target, Some(&row.accepted_body)).await;
        assert!(
            status != 400 && code != "ValidationException",
            "case {:?}: a body every constraint admits must earn no constraint refusal, \
             got {status} / {code:?} / {parsed}",
            row.case
        );
    }
}

/// The other half of the split, over the one member this service binds BOTH ways: the
/// security identifier `CreateTemplateGroupAccessControlEntry` takes in its body and
/// `GetTemplateGroupAccessControlEntry` takes as a path segment. The same value, the same
/// expression and two different refusals, because two different layers answer them.
#[tokio::test]
async fn the_same_member_is_refused_as_a_body_finding_and_as_a_per_member_clause() {
    let router = router();
    let template_arn = well_formed_template_arn();
    let value = "not-a-valid-sid";

    let mut body = access_control_entry_body();
    body["GroupSecurityIdentifier"] = json!(value);
    let (status, code, parsed) = call(
        &router,
        "POST",
        &format!(
            "/templates/{}/accessControlEntries",
            encode_label(&template_arn)
        ),
        Some(&body),
    )
    .await;
    let expected = report(&[regex_finding(GROUP_SID_PATTERN, value)]);
    assert_eq!(
        (status, code.as_str(), parsed["message"].as_str()),
        (400, "ValidationException", Some(expected.as_str())),
        "case \"the create, which takes the identifier in its body\": expected \
         {expected:?}, got {status} / {code:?} / {parsed}"
    );

    let (status, code, parsed) = call(
        &router,
        "GET",
        &format!(
            "/templates/{}/accessControlEntries/{}",
            encode_label(&template_arn),
            encode_label(value)
        ),
        None,
    )
    .await;
    let expected = format!(
        "1 validation error detected: Value '{value}' at 'groupSecurityIdentifier' failed \
         to satisfy constraint: {PATTERN_REQUIREMENT}{}",
        advertised(GROUP_SID_PATTERN)
    );
    assert_eq!(
        (status, code.as_str(), parsed["message"].as_str()),
        (400, "ValidationException", Some(expected.as_str())),
        "case \"the read, which takes the identifier in its URI\": expected {expected:?}, \
         got {status} / {code:?} / {parsed}"
    );
}

/// A request broken in BOTH halves is refused as the body rejection it already was: the
/// body is what the layer in front of the service stands in front of, so it answers first.
#[tokio::test]
async fn a_request_broken_in_both_halves_is_refused_as_the_body_finding() {
    let router = router();
    let body = json!({ "ClientToken": "token with spaces" });
    let (status, code, parsed) = call(
        &router,
        "POST",
        &format!(
            "/directoryRegistrations/{}/servicePrincipalNames/{}",
            encode_label("not-a-registration-arn"),
            encode_label(&well_formed_connector_arn())
        ),
        Some(&body),
    )
    .await;
    let expected = report(&[regex_finding(CLIENT_TOKEN_PATTERN, "token with spaces")]);
    assert_eq!(
        (status, code.as_str(), parsed["message"].as_str()),
        (400, "ValidationException", Some(expected.as_str())),
        "case \"a broken body and a broken path segment\": expected the body finding \
         {expected:?}, got {status} / {code:?} / {parsed}"
    );
    assert!(
        !parsed["message"]
            .as_str()
            .unwrap_or_default()
            .contains(advertised(DIRECTORY_REGISTRATION_ARN_PATTERN)),
        "case \"a broken body and a broken path segment\": the body refusal states no \
         clause about the URI, got {parsed}"
    );
}
