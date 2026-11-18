// SPDX-License-Identifier: CC0-1.0

//! WHICH principals a role's trust policy may name, end to end over the awsQuery wire.
//!
//! RULE: a trust policy is a resource-based policy, so its statements name the parties the role
//! trusts — and IAM reads each of them against the TYPE it was written under before it stores
//! the document, refusing with the modelled `MalformedPolicyDocumentException` (HTTP 400,
//! awsQuery code `MalformedPolicyDocument`) and `Invalid principal in policy: "<TYPE>":"<the
//! rejected value>"` — the type in upper case, the value exactly as the request wrote it — and
//! storing nothing: a refused `CreateRole` mints no role (a following `GetRole` answers
//! `NoSuchEntity`), a refused `UpdateAssumeRolePolicy` leaves the document the role already
//! held, and a refused `AcquireRole` acquires nothing. Three readings, one refusal:
//!
//! * a value outside the FORMS its principal type admits names no party at all — an account,
//!   an identity ARN or the wildcard under `AWS`, a service's published principal name under
//!   `Service`, an identity provider under `Federated`, a canonical user id under
//!   `CanonicalUser` (`state::unreadable_trust_policy_principal`);
//! * a `Service` naming a service that assumes its roles under ANOTHER service's principal
//!   rather than its own (`state::UNASSUMABLE_SERVICE_PRINCIPALS`);
//! * a `Service` naming a service by a REGIONALIZED spelling of a principal AWS publishes
//!   without a Region (`state::REGIONLESS_SERVICE_PRINCIPALS`).
//!
//! The principal each refusal points a client at — the service's own assuming principal, the
//! published Region-less name, the value under the type that admits it — is accepted and reads
//! back.
//!
//! No single recorded request is the specification, so each suite is a TABLE whose rows
//! differ along what the rules generalise over: the principal TYPE, HOW a value misses that
//! type's forms, which service of either catalog is named, the Region a regionalized name
//! carries, the form the value takes (one string, a list, the second entry of a list), the
//! element it sits under, which statement names it, the form `Statement` takes, the casing,
//! and the operation that writes the member. Both catalogs are READ here rather than retyped,
//! so a service filed in either later is exercised by this suite too. The awsQuery harness
//! (router, call, element, error_code) is the one `tests/all/user_lifecycle.rs` owns and every
//! suite in this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::role_template_acquisition::acquire;
use crate::user_lifecycle::{call, element, error_code, router};

/// The ARN the IAM power-user role template is published under — the one published template
/// whose trust policy names a service principal the REQUEST supplies
/// (`state::PUBLISHED_ROLE_TEMPLATES`, as `tests/all/published_role_template_acquisition.rs`
/// pins it), which is what makes `AcquireRole` a third writer of a caller-named principal.
const POWER_USER_TEMPLATE_ARN: &str =
    "arn:aws:iam::aws:role-template/iam.amazonaws.com/PowerUserRoleTemplate:1";

/// The trust policy every role this suite creates starts out with: the principal the services
/// of the catalog assume their roles under, which IAM accepts.
const HELD_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// A trust policy whose single statement names `principal` — written as the JSON the row
/// supplies, so a row can state one string or a list of them.
fn trust(principal: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"Service":{principal}}},"Action":"sts:AssumeRole"}}]}}"#
    )
}

/// One document the rule refuses: the JSON the request carries, and the value IAM must name in
/// its refusal.
struct Refused {
    case: &'static str,
    document: String,
    rejected: String,
}

/// Documents spanning the forms the rule generalises over. The catalog is read here rather
/// than retyped, so a service filed in it later is exercised by this suite too — and each row
/// states WHERE in the document the name sits.
fn refused_documents() -> Vec<Refused> {
    let mut rows: Vec<Refused> = Vec::new();
    for entry in state::UNASSUMABLE_SERVICE_PRINCIPALS {
        // The plainest form: the one principal of the one statement, as a string.
        rows.push(Refused {
            case: entry.principal,
            document: trust(&format!("\"{}\"", entry.principal)),
            rejected: entry.principal.to_string(),
        });
        // Beside a principal that CAN assume the role: one bad value in a list refuses the
        // whole document, and the refusal names that value.
        rows.push(Refused {
            case: entry.principal,
            document: trust(&format!(
                "[\"{}\",\"{}\"]",
                entry.assumed_under, entry.principal
            )),
            rejected: entry.principal.to_string(),
        });
    }
    let first = state::UNASSUMABLE_SERVICE_PRINCIPALS
        .first()
        .expect("the catalog carries at least one service");
    // A service principal is a DNS-style name, so another casing addresses the same service —
    // and the refusal names the value as the request wrote it.
    let miscased: String = first
        .principal
        .char_indices()
        .map(|(index, character)| {
            if index % 2 == 0 {
                character.to_ascii_uppercase()
            } else {
                character
            }
        })
        .collect();
    rows.push(Refused {
        case: "another casing of the same service",
        document: trust(&format!("\"{miscased}\"")),
        rejected: miscased,
    });
    // The complement element names principals too.
    rows.push(Refused {
        case: "the complement element",
        document: format!(
            r#"{{"Statement":[{{"Effect":"Deny","NotPrincipal":{{"Service":"{}"}},"Action":"sts:AssumeRole"}}]}}"#,
            first.principal
        ),
        rejected: first.principal.to_string(),
    });
    // The second statement of a list, after one IAM accepts.
    rows.push(Refused {
        case: "the second statement of a list",
        document: format!(
            r#"{{"Statement":[{{"Effect":"Allow","Principal":{{"Service":"{}"}},"Action":"sts:AssumeRole"}},{{"Effect":"Allow","Principal":{{"Service":"{}"}},"Action":"sts:AssumeRole"}}]}}"#,
            first.assumed_under, first.principal
        ),
        rejected: first.principal.to_string(),
    });
    // The single statement OBJECT the grammar admits in place of a list.
    rows.push(Refused {
        case: "the single statement object the grammar admits",
        document: format!(
            r#"{{"Statement":{{"Effect":"Allow","Principal":{{"Service":"{}"}},"Action":"sts:AssumeRole"}}}}"#,
            first.principal
        ),
        rejected: first.principal.to_string(),
    });
    // The service named beside an account principal, under the same element.
    rows.push(Refused {
        case: "beside an account principal under the same element",
        document: format!(
            r#"{{"Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::123456789012:root","Service":"{}"}},"Action":"sts:AssumeRole"}}]}}"#,
            first.principal
        ),
        rejected: first.principal.to_string(),
    });
    rows
}

/// Documents the rule leaves alone: a service principal nothing says cannot assume a role, in
/// the forms a trust policy writes one in.
fn accepted_documents() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for entry in state::UNASSUMABLE_SERVICE_PRINCIPALS {
        // The principal the refusal points a client at: writing it is what makes the document
        // storable, which is the whole point of naming it.
        rows.push((
            format!("the principal {} assumes its roles under", entry.principal),
            trust(&format!("\"{}\"", entry.assumed_under)),
        ));
    }
    for principal in [
        // Service principals outside the catalog: AWS Lambda and Amazon EC2, the two the
        // model's own `CreateRole` example names, and the principal of a service of the same
        // family that DOES assume roles under its own name.
        "lambda.amazonaws.com",
        "ec2.amazonaws.com",
        "neptune-graph.amazonaws.com",
    ] {
        rows.push((
            format!("a service principal outside the catalog: {principal}"),
            trust(&format!("\"{principal}\"")),
        ));
    }
    rows.push((
        "a list of service principals outside the catalog".to_string(),
        trust("[\"lambda.amazonaws.com\",\"ec2.amazonaws.com\"]"),
    ));
    rows.push((
        "an account principal and a federated one, naming no service".to_string(),
        r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::123456789012:root","Federated":"accounts.google.com"},"Action":"sts:AssumeRole"}]}"#
            .to_string(),
    ));
    rows
}

/// One `CreateRole` call carrying `document`.
async fn create_role(
    router: &MultiServiceRouter,
    role_name: &str,
    document: &str,
) -> (u16, String) {
    call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", document),
        ],
    )
    .await
}

/// One `UpdateAssumeRolePolicy` call carrying `document`.
async fn update_trust_policy(
    router: &MultiServiceRouter,
    role_name: &str,
    document: &str,
) -> (u16, String) {
    call(
        router,
        "UpdateAssumeRolePolicy",
        &[("RoleName", role_name), ("PolicyDocument", document)],
    )
    .await
}

/// The trust policy `GetRole` reports, percent-decoded back to plain JSON as IAM's contract
/// tells a caller to read a policy document.
async fn reported_trust_policy(router: &MultiServiceRouter, role_name: &str) -> String {
    let (status, body) = call(router, "GetRole", &[("RoleName", role_name)]).await;
    assert_eq!(
        status, 200,
        "case {role_name:?}: GetRole must succeed, got {status}: {body}"
    );
    crate::user_lifecycle::document_element(&body, "AssumeRolePolicyDocument")
}

/// RULE: `CreateRole` refuses a trust policy naming a service that assumes its roles under
/// another service's principal — `MalformedPolicyDocument`, HTTP 400, IAM's invalid-principal
/// sentence naming the rejected value — and mints no role.
#[tokio::test]
async fn creating_a_role_trusting_a_service_that_cannot_assume_one_is_refused() {
    let router = router();
    for (index, row) in refused_documents().into_iter().enumerate() {
        let role_name = format!("refused-{index}");
        let (status, body) = create_role(&router, &role_name, &row.document).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400 for {}, got {status}: {body}",
            row.case, row.document
        );
        assert_eq!(
            error_code(&body),
            "MalformedPolicyDocument",
            "case {:?}: expected the modelled awsQuery code, got {body}",
            row.case
        );
        let message = element(&body, "Message");
        assert_eq!(
            message,
            state::invalid_trust_policy_principal(&row.rejected),
            "case {:?}: expected IAM's invalid-principal sentence, got {message:?}",
            row.case
        );

        // Nothing was persisted: the name the refused create carried resolves to no role.
        let (get_status, got) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            get_status, 404,
            "case {:?}: a refused create must persist nothing, got {get_status}: {got}",
            row.case
        );
    }
}

/// RULE: a trust policy whose services nothing says cannot assume a role is STORED, and
/// `GetRole` reports it — the refusal above is about the named service, not about naming one.
#[tokio::test]
async fn a_trust_policy_naming_an_assumable_service_is_stored_and_read_back() {
    let router = router();
    for (index, (case, document)) in accepted_documents().into_iter().enumerate() {
        let role_name = format!("accepted-{index}");
        let (status, body) = create_role(&router, &role_name, &document).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be accepted, got {status}: {body}"
        );
        let reported = reported_trust_policy(&router, &role_name).await;
        assert_eq!(
            reported, document,
            "case {case:?}: expected the stored document to read back, got {reported:?}"
        );
    }
}

/// RULE: `AcquireRole` renders a trust policy from a role template whose principal the
/// request supplies, so the rendered document is resolved the same way — a template rendered
/// with a service that assumes its roles under another service's principal is refused with
/// the same code and sentence, and no role is acquired.
#[tokio::test]
async fn acquiring_a_role_from_a_template_resolves_its_rendered_service_principal() {
    let router = router();
    for (index, entry) in state::UNASSUMABLE_SERVICE_PRINCIPALS.iter().enumerate() {
        let role_name = format!("acquired-{index}");
        let (status, body) = acquire(
            &router,
            None,
            POWER_USER_TEMPLATE_ARN,
            Some(0),
            &[
                ("AWSServiceName", &[entry.principal]),
                ("RoleName", &[role_name.as_str()]),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400 for a rendered trust policy naming it, got {status}: {body}",
            entry.principal
        );
        assert_eq!(
            error_code(&body),
            "MalformedPolicyDocument",
            "case {:?}: expected the modelled awsQuery code, got {body}",
            entry.principal
        );
        let message = element(&body, "Message");
        assert_eq!(
            message,
            state::invalid_trust_policy_principal(entry.principal),
            "case {:?}: expected IAM's invalid-principal sentence, got {message:?}",
            entry.principal
        );
        let (get_status, got) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            get_status, 404,
            "case {:?}: a refused acquisition must persist nothing, got {get_status}: {got}",
            entry.principal
        );

        // The same template rendered with the principal those roles ARE assumed under is
        // acquired: the refusal is about the named service, not about the template.
        let acquired = format!("acquired-{index}-substitute");
        let (status, body) = acquire(
            &router,
            None,
            POWER_USER_TEMPLATE_ARN,
            Some(0),
            &[
                ("AWSServiceName", &[entry.assumed_under]),
                ("RoleName", &[acquired.as_str()]),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the substitute principal to be acquired, got {status}: {body}",
            entry.assumed_under
        );
        let reported = reported_trust_policy(&router, &acquired).await;
        assert!(
            reported.contains(entry.assumed_under),
            "case {:?}: expected the acquired role's trust policy to name it, got {reported:?}",
            entry.assumed_under
        );
    }
}

/// RULE: `UpdateAssumeRolePolicy` writes the same member, so it resolves the same principals —
/// and a refused update leaves the role with the document it already held, while the
/// substitute principal the refusal points at is stored.
#[tokio::test]
async fn replacing_a_trust_policy_resolves_its_service_principals_too() {
    let router = router();
    let role_name = "TrustTarget";
    let (status, body) = create_role(&router, role_name, HELD_TRUST_POLICY).await;
    assert_eq!(status, 200, "the role must be created, got {status}: {body}");

    for row in refused_documents() {
        let (status, body) = update_trust_policy(&router, role_name, &row.document).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400 for {}, got {status}: {body}",
            row.case, row.document
        );
        assert_eq!(
            error_code(&body),
            "MalformedPolicyDocument",
            "case {:?}: expected the modelled awsQuery code, got {body}",
            row.case
        );
        let message = element(&body, "Message");
        assert_eq!(
            message,
            state::invalid_trust_policy_principal(&row.rejected),
            "case {:?}: expected IAM's invalid-principal sentence, got {message:?}",
            row.case
        );
        let reported = reported_trust_policy(&router, role_name).await;
        assert_eq!(
            reported, HELD_TRUST_POLICY,
            "case {:?}: a refused update must leave the held policy alone, got {reported:?}",
            row.case
        );
    }

    for (case, document) in accepted_documents() {
        let (status, body) = update_trust_policy(&router, role_name, &document).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be accepted, got {status}: {body}"
        );
        let reported = reported_trust_policy(&router, role_name).await;
        assert_eq!(
            reported, document,
            "case {case:?}: expected the stored document to read back, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// The FORMS each principal type admits, and the regionalized spelling of a principal AWS
// publishes without a Region: the two readings above the service catalog, worded with the
// same sentence over the TYPE the rejected value sat under.
// ---------------------------------------------------------------------------------------

/// An account other than this emulator's own, so the identities a row names belong to an
/// account whose IAM this emulator is not the one to ask.
const OTHER_ACCOUNT: &str = "210987654321";

/// A trust policy whose single statement's `Principal` element is `element` — written as the
/// JSON the row supplies, so a row can state one type or several, one value or a list.
fn trust_element(element: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{element},"Action":"sts:AssumeRole"}}]}}"#
    )
}

/// One document a principal reading refuses: the JSON the request carries, and the TYPE and
/// the value IAM must name in its refusal.
struct RefusedPrincipal {
    case: &'static str,
    document: String,
    principal_type: &'static str,
    rejected: String,
}

/// A canonical user id, which the Amazon S3 user guide describes as "a 64-character
/// obfuscated identifier" — the one form the `CanonicalUser` type admits.
fn canonical_user_id() -> String {
    "7a59b1cd2e".repeat(6) + "cafe"
}

/// Documents spanning the ways a principal can miss the forms of the type it was written
/// under, and the places in a document where that value can sit.
fn refused_principal_forms() -> Vec<RefusedPrincipal> {
    let refused = |case: &'static str,
                   document: String,
                   principal_type: &'static str,
                   rejected: &str| RefusedPrincipal {
        case,
        document,
        principal_type,
        rejected: rejected.to_string(),
    };
    vec![
        // The commonest mistake: a service's principal name written under the ACCOUNT type. A
        // DNS-style name is no account id, no identity ARN and no wildcard, so it names nobody
        // there — whichever service it names.
        refused(
            "a service principal under the account type",
            trust_element(r#"{"AWS":"sns.amazonaws.com"}"#),
            "AWS",
            "sns.amazonaws.com",
        ),
        refused(
            "another service's principal under the account type",
            trust_element(r#"{"AWS":"lambda.amazonaws.com"}"#),
            "AWS",
            "lambda.amazonaws.com",
        ),
        refused(
            "a regional service principal under the account type",
            trust_element(r#"{"AWS":"logs.us-east-1.amazonaws.com"}"#),
            "AWS",
            "logs.us-east-1.amazonaws.com",
        ),
        refused(
            "the second value of a list, beside an account the type admits",
            trust_element(&format!(
                r#"{{"AWS":["arn:aws:iam::{OTHER_ACCOUNT}:root","ec2.amazonaws.com"]}}"#
            )),
            "AWS",
            "ec2.amazonaws.com",
        ),
        // The other ways an `AWS` value misses that type's forms.
        refused(
            "a figure one digit short of an account id",
            trust_element(r#"{"AWS":"21098765432"}"#),
            "AWS",
            "21098765432",
        ),
        refused(
            "a figure one digit past an account id",
            trust_element(r#"{"AWS":"2109876543210"}"#),
            "AWS",
            "2109876543210",
        ),
        refused(
            "an IAM entity that is no principal",
            trust_element(&format!(
                r#"{{"AWS":"arn:aws:iam::{OTHER_ACCOUNT}:group/builders"}}"#
            )),
            "AWS",
            &format!("arn:aws:iam::{OTHER_ACCOUNT}:group/builders"),
        ),
        refused(
            "an identity ARN carrying a wildcard",
            trust_element(&format!(
                r#"{{"AWS":"arn:aws:iam::{OTHER_ACCOUNT}:role/*"}}"#
            )),
            "AWS",
            &format!("arn:aws:iam::{OTHER_ACCOUNT}:role/*"),
        ),
        // A value under each of the other three types, missing that type's own forms.
        refused(
            "a service value that is no DNS-style name",
            trust_element(r#"{"Service":"Amazon SNS"}"#),
            "Service",
            "Amazon SNS",
        ),
        refused(
            "an account id under the service type",
            trust_element(&format!(r#"{{"Service":"{OTHER_ACCOUNT}"}}"#)),
            "Service",
            OTHER_ACCOUNT,
        ),
        refused(
            "a federated value that names no identity provider",
            trust_element(&format!(
                r#"{{"Federated":"arn:aws:iam::{OTHER_ACCOUNT}:role/corp"}}"#
            )),
            "Federated",
            &format!("arn:aws:iam::{OTHER_ACCOUNT}:role/corp"),
        ),
        refused(
            "a canonical user id of the wrong length",
            trust_element(&format!(
                r#"{{"CanonicalUser":"{}"}}"#,
                &canonical_user_id()[1..]
            )),
            "CanonicalUser",
            &canonical_user_id()[1..],
        ),
        // Where in the document the value sits does not change the answer.
        refused(
            "the complement element names it",
            r#"{"Statement":[{"Effect":"Deny","NotPrincipal":{"AWS":"sqs.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
                .to_string(),
            "AWS",
            "sqs.amazonaws.com",
        ),
        refused(
            "the second statement of a list, after one IAM accepts",
            r#"{"Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"},{"Effect":"Allow","Principal":{"AWS":"s3.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
                .to_string(),
            "AWS",
            "s3.amazonaws.com",
        ),
        refused(
            "the single statement object the grammar admits",
            r#"{"Statement":{"Effect":"Allow","Principal":{"AWS":"dynamodb.amazonaws.com"},"Action":"sts:AssumeRole"}}"#
                .to_string(),
            "AWS",
            "dynamodb.amazonaws.com",
        ),
        refused(
            "beside a principal the same element's other type admits",
            trust_element(&format!(
                r#"{{"AWS":"arn:aws:iam::{OTHER_ACCOUNT}:root","Service":"Amazon SNS"}}"#
            )),
            "Service",
            "Amazon SNS",
        ),
    ]
}

/// Documents the readings store: every principal in the forms its type admits, in the shapes
/// a trust policy writes them in. Each is a FIXED POINT of the one spelling this service saves
/// a policy's account principals in (`tests/all/role_trust_policy_account_principals.rs` owns
/// that rule), so what is stored is what was submitted.
fn accepted_principal_forms() -> Vec<(String, String)> {
    vec![
        (
            "a service principal AWS publishes".to_string(),
            trust_element(r#"{"Service":"sns.amazonaws.com"}"#),
        ),
        (
            "a list of service principals".to_string(),
            trust_element(r#"{"Service":["lambda.amazonaws.com","ec2.amazonaws.com"]}"#),
        ),
        (
            // A service that publishes a REGIONAL principal for the roles it assumes: the
            // regionalized spelling is a principal, so it is stored as written.
            "a regional service principal of a service that publishes one".to_string(),
            trust_element(r#"{"Service":"logs.us-east-1.amazonaws.com"}"#),
        ),
        (
            "the wildcard under the account type".to_string(),
            trust_element(r#"{"AWS":"*"}"#),
        ),
        (
            "the untyped wildcard the language writes without a type".to_string(),
            trust_element(r#""*""#),
        ),
        (
            "another account's root and an identity of it".to_string(),
            trust_element(&format!(
                r#"{{"AWS":["arn:aws:iam::{OTHER_ACCOUNT}:root","arn:aws:iam::{OTHER_ACCOUNT}:role/deploy"]}}"#
            )),
        ),
        (
            "an STS session of another account".to_string(),
            trust_element(&format!(
                r#"{{"AWS":"arn:aws:sts::{OTHER_ACCOUNT}:assumed-role/deploy/build"}}"#
            )),
        ),
        (
            "an identity provider by ARN and a web identity by host".to_string(),
            trust_element(&format!(
                r#"{{"Federated":["arn:aws:iam::{OTHER_ACCOUNT}:saml-provider/corp","accounts.google.com"]}}"#
            )),
        ),
        (
            "a canonical user id".to_string(),
            trust_element(&format!(
                r#"{{"CanonicalUser":"{}"}}"#,
                canonical_user_id()
            )),
        ),
        (
            "every type at once, each in its own forms".to_string(),
            trust_element(&format!(
                r#"{{"AWS":"arn:aws:iam::{OTHER_ACCOUNT}:root","Service":"ec2.amazonaws.com","Federated":"accounts.google.com","CanonicalUser":"{}"}}"#,
                canonical_user_id()
            )),
        ),
    ]
}

/// Assert one refusal: the modelled code, HTTP 400, and IAM's invalid-principal sentence over
/// the TYPE the value sat under and the value as the request wrote it.
fn assert_invalid_principal(
    case: &str,
    status: u16,
    body: &str,
    principal_type: &str,
    rejected: &str,
) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status}: {body}"
    );
    assert_eq!(
        error_code(body),
        "MalformedPolicyDocument",
        "case {case:?}: expected the modelled awsQuery code, got {body}"
    );
    let message = element(body, "Message");
    let expected = format!(
        "Invalid principal in policy: \"{}\":\"{rejected}\"",
        principal_type.to_ascii_uppercase()
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
}

/// RULE: `CreateRole` refuses a trust policy naming a value outside the forms of the principal
/// type it was written under — `MalformedPolicyDocument`, HTTP 400, IAM's invalid-principal
/// sentence naming that type and the value — and mints no role.
#[tokio::test]
async fn creating_a_role_whose_principal_is_outside_its_types_forms_is_refused() {
    let router = router();
    for (index, row) in refused_principal_forms().into_iter().enumerate() {
        let role_name = format!("form-refused-{index}");
        let (status, body) = create_role(&router, &role_name, &row.document).await;
        assert_invalid_principal(row.case, status, &body, row.principal_type, &row.rejected);

        // Nothing was persisted: the name the refused create carried resolves to no role.
        let (get_status, got) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            get_status, 404,
            "case {:?}: a refused create must persist nothing, got {get_status}: {got}",
            row.case
        );
        assert_eq!(
            error_code(&got),
            "NoSuchEntity",
            "case {:?}: expected the modelled absence code, got {got}",
            row.case
        );
    }
}

/// RULE: a trust policy whose every principal is in the forms its type admits is STORED, and
/// `GetRole` reports it — the refusal above is about the value's form, not about naming a
/// principal of that type.
#[tokio::test]
async fn a_trust_policy_whose_principals_are_in_their_types_forms_is_stored() {
    let router = router();
    for (index, (case, document)) in accepted_principal_forms().into_iter().enumerate() {
        let role_name = format!("form-accepted-{index}");
        let (status, body) = create_role(&router, &role_name, &document).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be accepted, got {status}: {body}"
        );
        let reported = reported_trust_policy(&router, &role_name).await;
        assert_eq!(
            reported, document,
            "case {case:?}: expected the stored document to read back, got {reported:?}"
        );
    }
}

/// RULE: `UpdateAssumeRolePolicy` writes the same member, so it reads the same forms — and a
/// refused update leaves the role with the document it already held, while a document whose
/// principals are in their types' forms replaces it.
#[tokio::test]
async fn replacing_a_trust_policy_reads_its_principal_forms_too() {
    let router = router();
    let role_name = "FormTarget";
    let (status, body) = create_role(&router, role_name, HELD_TRUST_POLICY).await;
    assert_eq!(status, 200, "the role must be created, got {status}: {body}");

    for row in refused_principal_forms() {
        let (status, body) = update_trust_policy(&router, role_name, &row.document).await;
        assert_invalid_principal(row.case, status, &body, row.principal_type, &row.rejected);
        let reported = reported_trust_policy(&router, role_name).await;
        assert_eq!(
            reported, HELD_TRUST_POLICY,
            "case {:?}: a refused update must leave the held policy alone, got {reported:?}",
            row.case
        );
    }

    for (case, document) in accepted_principal_forms() {
        let (status, body) = update_trust_policy(&router, role_name, &document).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be accepted, got {status}: {body}"
        );
        let reported = reported_trust_policy(&router, role_name).await;
        assert_eq!(
            reported, document,
            "case {case:?}: expected the stored document to read back, got {reported:?}"
        );
    }
}

/// The regionalized spelling of `published` in `region`: the mistake a client makes by writing
/// the Region its ENDPOINT carries into a principal AWS publishes without one.
fn regionalized(published: &str, region: &str) -> String {
    let (service, zone) = published
        .split_once('.')
        .expect("a service principal is a DNS-style name");
    format!("{service}.{region}.{zone}")
}

/// RULE: a service whose role-assuming principal AWS publishes WITHOUT a Region has no
/// principal under the regionalized spelling of that name, so a trust policy naming one is
/// refused — with the `SERVICE` type in the sentence — by both writers of the member, and the
/// published spelling the refusal points at is stored.
#[tokio::test]
async fn a_regionalized_service_principal_is_refused_and_its_published_spelling_stored() {
    let router = router();
    let target = "RegionalizedTarget";
    let (status, body) = create_role(&router, target, HELD_TRUST_POLICY).await;
    assert_eq!(status, 200, "the role must be created, got {status}: {body}");

    // The catalog is read rather than retyped, and the Regions rotate over several
    // geographies and a three-word Region name, so no row is the one the report cited.
    let regions = ["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"];
    for (index, published) in state::REGIONLESS_SERVICE_PRINCIPALS.iter().enumerate() {
        let region = regions[index % regions.len()];
        let rejected = regionalized(published, region);
        let document = trust_element(&format!(r#"{{"Service":"{rejected}"}}"#));
        let case = format!("{rejected} on CreateRole");
        let role_name = format!("regionalized-{index}");
        let (status, body) = create_role(&router, &role_name, &document).await;
        assert_invalid_principal(&case, status, &body, "Service", &rejected);
        let (get_status, got) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            get_status, 404,
            "case {case:?}: a refused create must persist nothing, got {get_status}: {got}"
        );

        let case = format!("{rejected} on UpdateAssumeRolePolicy");
        let (status, body) = update_trust_policy(&router, target, &document).await;
        assert_invalid_principal(&case, status, &body, "Service", &rejected);
        let reported = reported_trust_policy(&router, target).await;
        assert_eq!(
            reported, HELD_TRUST_POLICY,
            "case {case:?}: a refused update must leave the held policy alone, got {reported:?}"
        );

        // The published spelling the refusal points a client at IS stored: the refusal is
        // about the Region in the name, not about the service.
        let accepted = format!("published-{index}");
        let document = trust_element(&format!(r#"{{"Service":"{published}"}}"#));
        let (status, body) = create_role(&router, &accepted, &document).await;
        assert_eq!(
            status, 200,
            "case {published:?}: expected the published spelling to be stored, got {status}: \
             {body}"
        );
        let reported = reported_trust_policy(&router, &accepted).await;
        assert_eq!(
            reported, document,
            "case {published:?}: expected the stored document to read back, got {reported:?}"
        );
    }
}

/// RULE: `AcquireRole` renders a trust policy from a role template whose service principal the
/// request supplies, so the rendered document is read the same way — a template rendered with a
/// regionalized spelling of a principal AWS publishes without a Region is refused with the same
/// code and sentence, and no role is acquired.
#[tokio::test]
async fn acquiring_a_role_whose_rendered_service_principal_is_regionalized_is_refused() {
    let router = router();
    for (index, published) in state::REGIONLESS_SERVICE_PRINCIPALS.iter().enumerate() {
        let rejected = regionalized(published, "eu-central-1");
        let role_name = format!("regional-acquired-{index}");
        let (status, body) = acquire(
            &router,
            None,
            POWER_USER_TEMPLATE_ARN,
            Some(0),
            &[
                ("AWSServiceName", &[rejected.as_str()]),
                ("RoleName", &[role_name.as_str()]),
            ],
        )
        .await;
        assert_invalid_principal(&rejected, status, &body, "Service", &rejected);
        let (get_status, got) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            get_status, 404,
            "case {rejected:?}: a refused acquisition must persist nothing, got {get_status}: \
             {got}"
        );
    }
}
