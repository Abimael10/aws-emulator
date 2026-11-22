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
//!
//! WHEN that refusal is decided is the other half of the rule, asserted at the end of this
//! suite: a modelled constraint is evaluated BEFORE the operation resolves any resource
//! the request names, so a request that is invalid in itself earns the constraint refusal
//! whether or not the resources it addresses exist — and the same request with every value
//! admitted goes on to the lookup and earns the resource refusal instead.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pcaconnectorad_pcaconnectorad as pcad;

use super::harness::{call, encode_label, router, router_with_engine, ACCOUNT, REGION};
use super::pattern_refusal_messages::{
    advertised, well_formed_connector_arn, well_formed_directory_registration_arn,
    DIRECTORY_REGISTRATION_ARN_PATTERN, PATTERN_REQUIREMENT, TEMPLATE_ARN_PATTERN,
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

// ---------------------------------------------------------------------------
// WHEN a modelled constraint is evaluated: before the operation resolves anything.
// ---------------------------------------------------------------------------

/// One clause of the per-member envelope, for a value the request carries OUTSIDE its
/// body: the other half of the split, so a row about a URI-bound value states the
/// sentence that half is refused in.
fn uri_clause(member: &str, value: &str, pattern: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member}' failed to satisfy \
         constraint: {PATTERN_REQUIREMENT}{}",
        advertised(pattern)
    )
}

/// A `CreateTemplate` body: the parent connector it names, the template's own `Name`, the
/// names its definition supersedes (left out when there are none, since
/// `SupersededTemplates` is modelled `length 1..100`) and, optionally, a `ClientToken`.
/// The smallest `TemplateV2` the union accepts
/// (`smithy_query(query="operation",operation="CreateTemplate")`).
fn create_template_body(
    connector_arn: &str,
    name: &str,
    superseded: &[&str],
    client_token: Option<&str>,
) -> Value {
    let mut definition = json!({
        "CertificateValidity": {
            "ValidityPeriod": { "PeriodType": "YEARS", "Period": 1 },
            "RenewalPeriod": { "PeriodType": "WEEKS", "Period": 6 },
        },
        "PrivateKeyAttributes": { "MinimalKeyLength": 2048, "KeySpec": "KEY_EXCHANGE" },
        "PrivateKeyFlags": { "ClientVersion": "WINDOWS_SERVER_2016" },
        "EnrollmentFlags": { "NoSecurityExtension": true },
        "SubjectNameFlags": { "SanRequireDirectoryGuid": true },
        "GeneralFlags": { "MachineType": true },
        "Extensions": { "KeyUsage": { "UsageFlags": { "KeyEncipherment": true } } },
    });
    if !superseded.is_empty() {
        definition["SupersededTemplates"] = json!(superseded);
    }
    let mut body = json!({
        "ConnectorArn": connector_arn,
        "Name": name,
        "Definition": { "TemplateV2": definition },
    });
    if let Some(token) = client_token {
        body["ClientToken"] = json!(token);
    }
    body
}

/// One request that breaks a modelled constraint AND names resources this service does
/// not hold, so which of the two it is refused for states the ORDER the two checks run
/// in.
struct PrecedenceCase {
    /// Why this row is here: the operation, the member, the constraint it breaks and the
    /// resource it names.
    case: &'static str,
    method: &'static str,
    /// The request target, carrying the offending value when the value travels outside
    /// the body.
    target: String,
    body: Option<Value>,
    /// The whole message the constraint refusal states, computed from the model: the
    /// validator's bracketed findings for a body member, the per-member clause for a
    /// value bound to the URI.
    expected: String,
    /// The same request with every value its member admits.
    admitted_target: String,
    admitted_body: Option<Value>,
    /// The resource that request is then refused for, as the modelled shape spells the
    /// type, and the identifier the refusal echoes.
    resource_type: &'static str,
    identifier: String,
}

/// The rows: six operations, four constraint classes (a `@pattern`, a list `@length`
/// minimum, a string `@length` maximum, an enumerated type's value set), both halves of
/// the body/URI split, and three resource types on the far side of the lookup.
fn precedence_cases() -> Vec<PrecedenceCase> {
    // Nothing is created before these calls, so every resource they name is absent: a
    // request refused for a constraint cannot have been refused for the resource.
    let absent_connector = well_formed_connector_arn();
    let absent_template = well_formed_template_arn();
    let absent_registration = well_formed_directory_registration_arn();
    let entries_of_absent_template = format!(
        "/templates/{}/accessControlEntries",
        encode_label(&absent_template)
    );
    let name_of_absent_registration = format!(
        "/directoryRegistrations/{}/servicePrincipalNames/{}",
        encode_label(&absent_registration),
        encode_label(&absent_connector)
    );
    // A certificate-authority ARN whose resource id is not the UUID its expression
    // requires, so the only thing wrong with the request is that member.
    let malformed_ca_arn =
        format!("arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/not-a-uuid");
    // A client token one character past the modelled maximum, out of characters the
    // token's own expression admits.
    let over_long_token = "t".repeat(CLIENT_TOKEN_MAX_LENGTH + 1);
    let longest_admitted_token = "t".repeat(CLIENT_TOKEN_MAX_LENGTH);
    vec![
        PrecedenceCase {
            case: "CreateConnector, a capitalised directory id, for a directory this \
                   service holds no registration for",
            method: "POST",
            target: "/connectors".to_string(),
            body: Some({
                let mut body = connector_body();
                body["DirectoryId"] = json!("D-0123456789");
                body
            }),
            expected: report(&[regex_finding(DIRECTORY_ID_PATTERN, "D-0123456789")]),
            admitted_target: "/connectors".to_string(),
            admitted_body: Some(connector_body()),
            resource_type: "DirectoryRegistration",
            identifier: WELL_FORMED_DIRECTORY_ID.to_string(),
        },
        PrecedenceCase {
            case: "CreateConnector, a certificate-authority ARN without the resource id \
                   its expression requires, for an unregistered directory",
            method: "POST",
            target: "/connectors".to_string(),
            body: Some({
                let mut body = connector_body();
                body["CertificateAuthorityArn"] = json!(malformed_ca_arn);
                body
            }),
            expected: report(&[regex_finding(
                CERTIFICATE_AUTHORITY_ARN_PATTERN,
                &malformed_ca_arn,
            )]),
            admitted_target: "/connectors".to_string(),
            admitted_body: Some(connector_body()),
            resource_type: "DirectoryRegistration",
            identifier: WELL_FORMED_DIRECTORY_ID.to_string(),
        },
        PrecedenceCase {
            case: "CreateConnector, no security group at all, for an unregistered directory",
            method: "POST",
            target: "/connectors".to_string(),
            body: Some({
                let mut body = connector_body();
                body["VpcInformation"]["SecurityGroupIds"] = json!([]);
                body
            }),
            expected: report(&[array_finding(SECURITY_GROUP_IDS_MIN, 0)]),
            admitted_target: "/connectors".to_string(),
            admitted_body: Some(connector_body()),
            resource_type: "DirectoryRegistration",
            identifier: WELL_FORMED_DIRECTORY_ID.to_string(),
        },
        PrecedenceCase {
            case: "CreateTemplate, a name outside the expression its shape declares, under \
                   a connector never created",
            method: "POST",
            target: "/templates".to_string(),
            body: Some(create_template_body(
                &absent_connector,
                "Web,Server",
                &[],
                None,
            )),
            expected: report(&[regex_finding(
                pcad::state::TEMPLATE_NAME_PATTERN,
                "Web,Server",
            )]),
            admitted_target: "/templates".to_string(),
            admitted_body: Some(create_template_body(
                &absent_connector,
                "WebServerTemplate",
                &[],
                None,
            )),
            resource_type: "Connector",
            identifier: absent_connector.clone(),
        },
        PrecedenceCase {
            case: "CreateTemplate, a superseded template name outside that same expression, \
                   under a connector never created",
            method: "POST",
            target: "/templates".to_string(),
            body: Some(create_template_body(
                &absent_connector,
                "WebServerTemplate",
                &["Legacy=Template"],
                None,
            )),
            expected: report(&[regex_finding(
                pcad::state::TEMPLATE_NAME_PATTERN,
                "Legacy=Template",
            )]),
            admitted_target: "/templates".to_string(),
            admitted_body: Some(create_template_body(
                &absent_connector,
                "WebServerTemplate",
                &["LegacyTemplate"],
                None,
            )),
            resource_type: "Connector",
            identifier: absent_connector.clone(),
        },
        PrecedenceCase {
            case: "CreateTemplate, a client token past the modelled maximum, under a \
                   connector never created",
            method: "POST",
            target: "/templates".to_string(),
            body: Some(create_template_body(
                &absent_connector,
                "WebServerTemplate",
                &[],
                Some(&over_long_token),
            )),
            expected: report(&[too_long_finding(&over_long_token, CLIENT_TOKEN_MAX_LENGTH)]),
            admitted_target: "/templates".to_string(),
            admitted_body: Some(create_template_body(
                &absent_connector,
                "WebServerTemplate",
                &[],
                Some(&longest_admitted_token),
            )),
            resource_type: "Connector",
            identifier: absent_connector.clone(),
        },
        PrecedenceCase {
            case: "CreateServicePrincipalName, a client token its expression does not \
                   admit, under a registration never created",
            method: "POST",
            target: name_of_absent_registration.clone(),
            body: Some(json!({ "ClientToken": "token with spaces" })),
            expected: report(&[regex_finding(CLIENT_TOKEN_PATTERN, "token with spaces")]),
            admitted_target: name_of_absent_registration,
            admitted_body: Some(json!({ "ClientToken": "token-without-spaces" })),
            resource_type: "DirectoryRegistration",
            identifier: absent_registration.clone(),
        },
        PrecedenceCase {
            case: "CreateServicePrincipalName, a connector ARN for the registration its \
                   PATH names, for a registration that does not exist either",
            method: "POST",
            target: format!(
                "/directoryRegistrations/{}/servicePrincipalNames/{}",
                encode_label(&absent_connector),
                encode_label(&absent_connector)
            ),
            body: Some(json!({})),
            expected: uri_clause(
                "directoryRegistrationArn",
                &absent_connector,
                DIRECTORY_REGISTRATION_ARN_PATTERN,
            ),
            admitted_target: format!(
                "/directoryRegistrations/{}/servicePrincipalNames/{}",
                encode_label(&absent_registration),
                encode_label(&absent_connector)
            ),
            admitted_body: Some(json!({})),
            resource_type: "DirectoryRegistration",
            identifier: absent_registration,
        },
        PrecedenceCase {
            case: "CreateTemplateGroupAccessControlEntry, a word for the security \
                   identifier it takes in its body, under a template never created",
            method: "POST",
            target: entries_of_absent_template.clone(),
            body: Some({
                let mut body = access_control_entry_body();
                body["GroupSecurityIdentifier"] = json!("not-a-valid-sid");
                body
            }),
            expected: report(&[regex_finding(GROUP_SID_PATTERN, "not-a-valid-sid")]),
            admitted_target: entries_of_absent_template.clone(),
            admitted_body: Some(access_control_entry_body()),
            resource_type: "Template",
            identifier: absent_template.clone(),
        },
        PrecedenceCase {
            case: "CreateTemplateGroupAccessControlEntry, a value the access right's \
                   enumerated type does not admit, under a template never created",
            method: "POST",
            target: entries_of_absent_template.clone(),
            body: Some({
                let mut body = access_control_entry_body();
                body["AccessRights"]["Enroll"] = json!("MAYBE");
                body
            }),
            expected: report(&[enum_finding("MAYBE", &ACCESS_RIGHT_VALUES)]),
            admitted_target: entries_of_absent_template.clone(),
            admitted_body: Some(access_control_entry_body()),
            resource_type: "Template",
            identifier: absent_template.clone(),
        },
        PrecedenceCase {
            case: "CreateTemplateGroupAccessControlEntry, a word for the template ARN its \
                   PATH names, for a template that does not exist either",
            method: "POST",
            target: "/templates/not-a-valid-arn/accessControlEntries".to_string(),
            body: Some(access_control_entry_body()),
            expected: uri_clause("templateArn", "not-a-valid-arn", TEMPLATE_ARN_PATTERN),
            admitted_target: entries_of_absent_template,
            admitted_body: Some(access_control_entry_body()),
            resource_type: "Template",
            identifier: absent_template.clone(),
        },
        PrecedenceCase {
            case: "GetTemplate, a word for the template ARN its path names, for a template \
                   that does not exist either",
            method: "GET",
            target: "/templates/not-a-valid-arn".to_string(),
            body: None,
            expected: uri_clause("templateArn", "not-a-valid-arn", TEMPLATE_ARN_PATTERN),
            admitted_target: format!("/templates/{}", encode_label(&absent_template)),
            admitted_body: None,
            resource_type: "Template",
            identifier: absent_template,
        },
    ]
}

/// A modelled constraint is evaluated before the operation resolves anything: each row
/// breaks a constraint AND names absent resources, and every one of them is refused for
/// the constraint — in the words of the half the value travels in — and not for the
/// resource.
#[tokio::test]
async fn a_constraint_refusal_precedes_every_resource_lookup() {
    let router = router();
    for row in precedence_cases() {
        let (status, code, parsed) = call(&router, row.method, &row.target, row.body.as_ref()).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {:?}: a request outside a modelled constraint is refused for that \
             constraint whatever it names, got {status} / {code:?} / {parsed}",
            row.case
        );
        assert_eq!(
            parsed["message"].as_str(),
            Some(row.expected.as_str()),
            "case {:?}: expected message {:?}, got {parsed}",
            row.case,
            row.expected
        );
    }
}

/// The other half of the ordering rule: the SAME request with every value its members
/// admit reaches the lookup, so it is refused for the resource it names — which is what
/// makes the refusals above an ordering and not a blanket rejection.
#[tokio::test]
async fn the_same_request_with_every_value_admitted_reaches_the_lookup() {
    let router = router();
    for row in precedence_cases() {
        let (status, code, parsed) = call(
            &router,
            row.method,
            &row.admitted_target,
            row.admitted_body.as_ref(),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {:?}: a request every constraint admits reaches the lookup, \
             got {status} / {code:?} / {parsed}",
            row.case
        );
        let expected = format!("{} was not found: {}.", row.resource_type, row.identifier);
        assert_eq!(
            parsed["Message"].as_str(),
            Some(expected.as_str()),
            "case {:?}: expected message {expected:?}, got {parsed}",
            row.case
        );
    }
}

/// A request refused for a constraint PERSISTS NOTHING: the store holds no resource of
/// any kind after every row above has been refused, so no value a modelled constraint
/// forbids was ever written.
#[tokio::test]
async fn a_request_refused_for_a_constraint_persists_nothing() {
    let (router, engine) = router_with_engine();
    for row in precedence_cases() {
        let (status, code, parsed) = call(&router, row.method, &row.target, row.body.as_ref()).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {:?}: expected the constraint refusal, got {status} / {code:?} / {parsed}",
            row.case
        );
    }

    let shared = pcad::state::service_state(&*engine).expect("the service's own state");
    let store = shared.lock();
    let held = [
        ("connectors", store.connectors.len()),
        ("templates", store.templates.len()),
        (
            "directory registrations",
            store.directory_registrations.len(),
        ),
        (
            "service principal names",
            store.service_principal_names.len(),
        ),
        (
            "access control entries",
            store.access_control_entries.len(),
        ),
    ];
    for (resource, count) in held {
        assert_eq!(
            count, 0,
            "case {resource:?}: a refused request persists nothing, got {count} record(s)"
        );
    }
}

/// The directory ids `CreateDirectoryRegistration` refuses, one row per way a value can
/// miss the expression its member declares.
///
/// `DirectoryId` travels in that operation's BODY
/// (`smithy_query(query="operation",operation="CreateDirectoryRegistration")`: POST
/// `/directoryRegistrations`, success 202, `DirectoryId` `@required` with `@pattern`
/// [`DIRECTORY_ID_PATTERN`]), so the rule this suite states places its refusal on the body
/// half: the whole message is the validator's bracketed finding over that expression and the
/// submitted value, with no member path and no error count.
///
/// The rows differ along everything the expression constrains — the prefix, its case, how
/// many characters follow it, the alphabet those characters come from, and whether the
/// expression has to match the WHOLE value — so the rule is asserted over the class rather
/// than over one value.
fn rejected_directory_ids() -> Vec<(&'static str, String)> {
    vec![
        (
            "a word carrying neither the prefix nor an identifier",
            "not-a-directory-at-all".to_string(),
        ),
        ("the prefix capitalised", "D-0123456789".to_string()),
        (
            "the identifier in upper-case hexadecimal",
            "d-ABCDEF0123".to_string(),
        ),
        (
            "one character short of the modelled count",
            "d-012345678".to_string(),
        ),
        (
            "one character past the modelled count",
            "d-01234567890".to_string(),
        ),
        (
            "letters outside the hexadecimal alphabet among the characters",
            "d-01234567yz".to_string(),
        ),
        ("the prefix alone", "d-".to_string()),
        ("the prefix without its separator", "d0123456789".to_string()),
        ("no value at all", String::new()),
        (
            "an identifier the expression admits, with a trailing space",
            format!("{WELL_FORMED_DIRECTORY_ID} "),
        ),
        (
            "an identifier the expression admits inside a longer word, which the \
             whole-value match refuses",
            format!("prefix{WELL_FORMED_DIRECTORY_ID}suffix"),
        ),
        (
            "the registration's ARN where the directory's identifier belongs",
            well_formed_directory_registration_arn(),
        ),
    ]
}

/// Three directory ids the expression DOES admit, differing in the characters that follow
/// the prefix, so the accepted half of the rule is asserted over more than one value.
const ADMITTED_DIRECTORY_IDS: [&str; 3] =
    [WELL_FORMED_DIRECTORY_ID, "d-abcdef0123", "d-00000000ff"];

/// The ARNs `ListDirectoryRegistrations` answers with, read through the wire — which also
/// pins that the collection key is there to read at all.
async fn listed_registrations(router: &MultiServiceRouter) -> Vec<String> {
    let (status, code, parsed) = call(router, "GET", "/directoryRegistrations", None).await;
    assert_eq!(
        (status, code.as_str()),
        (200, ""),
        "ListDirectoryRegistrations must answer normally, got {status} / {code:?} / {parsed}"
    );
    let entries = parsed["DirectoryRegistrations"].as_array().unwrap_or_else(|| {
        panic!("ListDirectoryRegistrations must answer its collection key, got {parsed}")
    });
    entries
        .iter()
        .filter_map(|summary| summary["Arn"].as_str())
        .map(str::to_string)
        .collect()
}

/// `CreateDirectoryRegistration`'s directory id is a BODY member, so a value its expression
/// does not admit earns the validator's bracketed finding and nothing else — and the
/// collection it would have been added to is left exactly as it was.
#[tokio::test]
async fn create_directory_registration_refuses_a_directory_id_with_the_bracketed_finding_alone() {
    let router = router();
    for (case, value) in rejected_directory_ids() {
        let before = listed_registrations(&router).await;
        let body = json!({ "DirectoryId": value });
        let (status, code, parsed) =
            call(&router, "POST", "/directoryRegistrations", Some(&body)).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: a directory id outside its expression is refused with 400 \
             ValidationException, got {status} / {code:?} / {parsed}"
        );
        let expected = report(&[regex_finding(DIRECTORY_ID_PATTERN, &value)]);
        assert_eq!(
            parsed["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected message {expected:?}, got {parsed}"
        );
        let after = listed_registrations(&router).await;
        assert_eq!(
            before, after,
            "case {case:?}: a refused create adds no registration, got {after:?} \
             where the collection held {before:?}"
        );
    }
}

/// The findings are the WHOLE message for this operation too: none of the per-member
/// envelope's parts reach it — no error count, no `directoryId` path, no `Member must …`
/// requirement — and the modelled expression is quoted as the model writes it, anchors
/// included.
#[tokio::test]
async fn create_directory_registrations_refusal_states_no_part_of_the_per_member_envelope() {
    let router = router();
    for (case, value) in rejected_directory_ids() {
        let body = json!({ "DirectoryId": value });
        let (_, _, parsed) = call(&router, "POST", "/directoryRegistrations", Some(&body)).await;
        let message = parsed["message"].as_str().unwrap_or_default().to_string();
        for absent in [
            "validation error",
            "failed to satisfy constraint",
            "directoryId",
            PATTERN_REQUIREMENT,
        ] {
            assert!(
                !message.contains(absent),
                "case {case:?}: this refusal states no {absent:?}, got {message:?}"
            );
        }
        assert!(
            message.starts_with('[') && message.ends_with(']'),
            "case {case:?}: the finding is stated in one pair of brackets, got {message:?}"
        );
        assert!(
            message.contains(DIRECTORY_ID_PATTERN),
            "case {case:?}: the finding quotes the modelled expression \
             {DIRECTORY_ID_PATTERN:?} as the model writes it, got {message:?}"
        );
    }
}

/// Only the wording is the validating layer's: the CHECK is the model's own expression, so
/// a directory id it admits is registered and read back from the collection the refusals
/// above left alone.
#[tokio::test]
async fn a_directory_id_the_expression_admits_is_registered_and_listed() {
    let router = router();
    for directory_id in ADMITTED_DIRECTORY_IDS {
        let before = listed_registrations(&router).await;
        let body = json!({ "DirectoryId": directory_id });
        let (status, code, parsed) =
            call(&router, "POST", "/directoryRegistrations", Some(&body)).await;
        assert_eq!(
            (status, code.as_str()),
            (202, ""),
            "case {directory_id:?}: a directory id the expression admits is accepted, \
             got {status} / {code:?} / {parsed}"
        );
        let arn = parsed["DirectoryRegistrationArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {directory_id:?}: the create answered no ARN, got {parsed}")
            })
            .to_string();
        let after = listed_registrations(&router).await;
        assert!(
            after.contains(&arn) && after.len() == before.len() + 1,
            "case {directory_id:?}: the minted {arn:?} is the one registration added, \
             got {after:?} where the collection held {before:?}"
        );
    }
}
