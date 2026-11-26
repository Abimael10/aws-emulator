// SPDX-License-Identifier: CC0-1.0

//! The trust document a role's readers report, end to end over the awsQuery wire.
//!
//! Two rules, each about the `AssumeRolePolicyDocument` member rather than about any one
//! operation:
//!
//! * a CREATE answers with the policy document the REQUEST carried, while the account holds
//!   IAM's rendering of it — the whitespace between the JSON tokens dropped, a one-element
//!   string-or-array element written as the string it names, an account principal named by an
//!   id alone written as that account's root-user ARN — and that rendering is what every later
//!   READ of the role reports (`GetRole`, `ListRoles`). Both forms are percent-encoded, which
//!   is how every IAM response carries a policy document ("policies returned by this operation
//!   are URL-encoded compliant with RFC 3986", `GetRole`), so this suite reads each of them
//!   through the runtime's own URI decoder.
//! * the trust document of a SERVICE-LINKED role is the one statement that lets the linked
//!   service assume it, written in the order a policy document states a statement in — the
//!   language version, then the effect, the principal and the action — since what a client
//!   hashes or diffs to detect drift is the document's text.
//!
//! No single recorded request is the specification, so every suite here is a TABLE whose rows
//! differ along what the rule generalises over: the spacing of the submitted document, the list
//! form of its elements, the spelling of its principal, and the service a role is linked to.
//! The awsQuery harness (router, call, element, elements) is the one
//! `tests/all/user_lifecycle.rs` owns and every suite in this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_document_encoding::assert_percent_encoded;
use crate::user_lifecycle::{
    account_id, call, document_element, document_elements, element, router,
};

/// An account other than this emulator's own, so a cross-account trust is one of the rows.
const OTHER_ACCOUNT: &str = "210987654321";

/// The path this suite's roles live under, so a listing selects exactly them.
const FIXTURE_PATH: &str = "/trust-forms/";

/// One submitted trust document, and the text the account holds for it.
struct DocumentRow {
    case: &'static str,
    /// What the request carries — and so what the create's own answer restates.
    submitted: String,
    /// What the account holds, which every read of the role reports.
    stored: String,
}

/// The rows the first rule is asserted over.
fn rows() -> Vec<DocumentRow> {
    let own = account_id();
    vec![
        DocumentRow {
            case: "a compact document the rendering leaves alone",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#.to_string(),
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#.to_string(),
        },
        DocumentRow {
            case: "the spaces an SDK's JSON serializer writes between the tokens",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Principal": {"Service": "ec2.amazonaws.com"}, "Action": "sts:AssumeRole"}]}"#.to_string(),
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#.to_string(),
        },
        DocumentRow {
            // `policyDocumentType` is `@pattern ^[\u0009\u000A\u000D\u0020-\u00FF]+$`
            // (`smithy_query(query="shape",shape="policyDocumentType")`), so the tab, the line
            // feed and the carriage return are part of what a caller may submit.
            case: "a document indented with tabs, line feeds and a carriage return",
            submitted: "{\r\n\t\"Version\": \"2012-10-17\",\n\t\"Statement\": [\n\t\t{\n\t\t\t\"Effect\": \"Allow\",\n\t\t\t\"Principal\": {\"Service\": \"ec2.amazonaws.com\"},\n\t\t\t\"Action\": \"sts:AssumeRole\"\n\t\t}\n\t]\n}".to_string(),
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#.to_string(),
        },
        DocumentRow {
            case: "a one-element action list, which the rendering writes as the string",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":["sts:AssumeRole"]}]}"#.to_string(),
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#.to_string(),
        },
        DocumentRow {
            case: "a one-element principal list, and a list of two actions that keeps its form",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":["ec2.amazonaws.com"]},"Action":["sts:AssumeRole","sts:TagSession"]}]}"#.to_string(),
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":["sts:AssumeRole","sts:TagSession"]}]}"#.to_string(),
        },
        DocumentRow {
            case: "this account named by its id alone, which is held as its root-user ARN",
            submitted: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{own}"}},"Action":"sts:AssumeRole"}}]}}"#
            ),
            stored: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{own}:root"}},"Action":"sts:AssumeRole"}}]}}"#
            ),
        },
        DocumentRow {
            case: "another account named by its id, beside a condition carrying a + and a %",
            submitted: format!(
                r#"{{"Version": "2012-10-17", "Statement": [{{"Effect": "Allow", "Principal": {{"AWS": "{OTHER_ACCOUNT}"}}, "Action": "sts:AssumeRole", "Condition": {{"StringEquals": {{"sts:ExternalId": "a+b 100%"}}}}}}]}}"#
            ),
            stored: format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{OTHER_ACCOUNT}:root"}},"Action":"sts:AssumeRole","Condition":{{"StringEquals":{{"sts:ExternalId":"a+b 100%"}}}}}}]}}"#
            ),
        },
    ]
}

/// Create one role at [`FIXTURE_PATH`] carrying `document`.
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
            ("Path", FIXTURE_PATH),
            ("AssumeRolePolicyDocument", document),
        ],
    )
    .await
}

/// RULE: the create's own answer restates the document the request carried, while the account
/// holds IAM's rendering of it — and that rendering is what `GetRole` and a page of `ListRoles`
/// report, each of them percent-encoded.
#[tokio::test]
async fn a_create_restates_its_document_and_every_read_reports_the_stored_rendering() {
    let router = router();
    let documents = rows();
    for (index, row) in documents.iter().enumerate() {
        let role_name = format!("TrustForm{index}");
        let (status, body) = create_role(&router, &role_name, &row.submitted).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateRole must succeed, got {status}: {body}",
            row.case
        );
        // The member is carried percent-encoded whichever form it holds, which is what makes
        // the two decodes below more than string comparisons.
        assert_percent_encoded(row.case, &element(&body, "AssumeRolePolicyDocument"));
        let created = document_element(&body, "AssumeRolePolicyDocument");
        assert_eq!(
            created, row.submitted,
            "case {:?}: expected the create to restate {:?}, got {created:?}",
            row.case, row.submitted
        );

        let (status, read) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetRole must succeed, got {status}: {read}",
            row.case
        );
        assert_percent_encoded(row.case, &element(&read, "AssumeRolePolicyDocument"));
        let reported = document_element(&read, "AssumeRolePolicyDocument");
        assert_eq!(
            reported, row.stored,
            "case {:?}: expected GetRole to report {:?}, got {reported:?}",
            row.case, row.stored
        );
    }

    // One page of the listing carries every fixture role, each reporting the document its own
    // role holds — the stored rendering, as the single read reports it.
    let (status, body) = call(
        &router,
        "ListRoles",
        &[("PathPrefix", FIXTURE_PATH), ("MaxItems", "1000")],
    )
    .await;
    assert_eq!(status, 200, "ListRoles must succeed, got {status}: {body}");
    let listed = document_elements(&body, "AssumeRolePolicyDocument");
    for row in &documents {
        assert!(
            listed.contains(&row.stored),
            "case {:?}: expected the listing to report {:?}, got {listed:?}",
            row.case, row.stored
        );
    }
}

/// RULE: re-submitting the document a read reported stores the same text again, so a client that
/// reads a trust policy and writes it back sees no drift.
#[tokio::test]
async fn the_stored_rendering_is_a_fixed_point_of_a_write_back() {
    let router = router();
    let role_name = "TrustFormFixedPoint";
    let (status, body) = create_role(
        &router,
        role_name,
        r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
    )
    .await;
    assert_eq!(status, 200, "the fixture create must succeed: {body}");

    for row in rows() {
        let (status, body) = call(
            &router,
            "UpdateAssumeRolePolicy",
            &[("RoleName", role_name), ("PolicyDocument", &row.submitted)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: UpdateAssumeRolePolicy must succeed, got {status}: {body}",
            row.case
        );
        let (_, read) = call(&router, "GetRole", &[("RoleName", role_name)]).await;
        let reported = document_element(&read, "AssumeRolePolicyDocument");
        assert_eq!(
            reported, row.stored,
            "case {:?}: expected the update to store {:?}, got {reported:?}",
            row.case, row.stored
        );

        let (status, body) = call(
            &router,
            "UpdateAssumeRolePolicy",
            &[("RoleName", role_name), ("PolicyDocument", &reported)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: writing back the reported document must succeed, got {status}: {body}",
            row.case
        );
        let (_, read) = call(&router, "GetRole", &[("RoleName", role_name)]).await;
        let again = document_element(&read, "AssumeRolePolicyDocument");
        assert_eq!(
            again, row.stored,
            "case {:?}: expected the stored document to be a fixed point, got {again:?}",
            row.case
        );
    }
}

/// The trust document of a service-linked role linked to `service_principal`: the one statement
/// that lets that service assume it, in the order a policy document states a statement in.
fn service_linked_trust_document(service_principal: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"Service":"{service_principal}"}},"Action":"sts:AssumeRole"}}]}}"#
    )
}

/// RULE: a service-linked role's trust document names the service the role is linked to, written
/// in the order a policy states a statement in — and the create and every later read report that
/// one text.
#[tokio::test]
async fn a_service_linked_roles_trust_document_names_its_service_in_the_order_a_policy_states_it()
{
    let router = router();
    // Rows differ along the service a role is linked to: a plain service principal, a second
    // one, a principal carrying a namespace segment, and a role asked for under a custom
    // suffix (source: the published names `tests/all/service_linked_roles.rs` pins).
    let rows: [(&str, &str, Option<&str>); 4] = [
        ("a plain service principal", "ecs.amazonaws.com", None),
        (
            "a second service, so the document is computed from the principal",
            "elasticbeanstalk.amazonaws.com",
            None,
        ),
        (
            "a principal carrying a namespace segment",
            "rds.application-autoscaling.amazonaws.com",
            None,
        ),
        (
            "a role asked for under a custom suffix",
            "autoscaling.amazonaws.com",
            Some("probe"),
        ),
    ];

    for (case, principal, suffix) in rows {
        let mut params: Vec<(&str, &str)> = vec![("AWSServiceName", principal)];
        if let Some(suffix) = suffix {
            params.push(("CustomSuffix", suffix));
        }
        let (status, created) = call(&router, "CreateServiceLinkedRole", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateServiceLinkedRole must succeed, got {status}: {created}"
        );
        let expected = service_linked_trust_document(principal);
        assert_percent_encoded(case, &element(&created, "AssumeRolePolicyDocument"));
        let reported = document_element(&created, "AssumeRolePolicyDocument");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the create to report {expected:?}, got {reported:?}"
        );

        // And the read of the record that was written reports the same text.
        let role_name = element(&created, "RoleName");
        let (status, read) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetRole must succeed, got {status}: {read}"
        );
        let reported = document_element(&read, "AssumeRolePolicyDocument");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected GetRole to report {expected:?}, got {reported:?}"
        );
    }
}
