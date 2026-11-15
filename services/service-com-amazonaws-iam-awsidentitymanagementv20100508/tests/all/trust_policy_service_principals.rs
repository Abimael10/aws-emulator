// SPDX-License-Identifier: CC0-1.0

//! The SERVICE principals a role's trust policy may name, end to end over the awsQuery wire.
//!
//! RULE: IAM resolves every service principal a trust policy names — the values under the
//! `Service` type of a statement's `Principal` or `NotPrincipal` element — and refuses the
//! write when the named service assumes its roles under ANOTHER service's principal rather
//! than its own: the modelled `MalformedPolicyDocumentException` (HTTP 400, awsQuery code
//! `MalformedPolicyDocument`) with `Invalid principal in policy: "SERVICE":"<the rejected
//! value>"`, and nothing is stored — a refused `CreateRole` mints no role, a refused
//! `UpdateAssumeRolePolicy` leaves the document the role already held, and a refused
//! `AcquireRole` acquires nothing. The principal that service's roles ARE assumed under is
//! accepted and reads back.
//!
//! No single recorded request is the specification, so each suite is a TABLE whose rows
//! differ along what the rule generalises over: which service of the catalog
//! (`state::UNASSUMABLE_SERVICE_PRINCIPALS`) is named, the form the value takes (one string,
//! a list, the second entry of a list), the element it sits under, which statement names it,
//! the form `Statement` takes, the casing, and the operation that writes the member. The
//! awsQuery harness (router, call, element, error_code) is the one `tests/all/user_lifecycle.rs`
//! owns and every suite in this crate shares.

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
