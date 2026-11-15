// SPDX-License-Identifier: CC0-1.0

//! The modelled members an IAM reader reports for an entity that holds nothing in them,
//! end to end over the awsQuery wire.
//!
//! RULE under test: a reader that reports an IAM entity reports that entity's modelled set
//! members — the `Tags` attached to it, the `RoleLastUsed` recorded against a role, and the
//! `OrganizationsDecisionDetail` a simulated decision was reached under — as an element
//! whatever the entity holds in them, EMPTY when it holds nothing. The member is the report
//! of a set, so "the entity carries no tags" is the empty report rather than a missing
//! member: absent would be a third answer the member does not have, and a client reading
//! `response.Tags` without a null guard — valid against AWS, where the member is always
//! there — would fail on an untagged entity.
//!
//! The one case where the member really is withheld is the resource LISTINGS, which "return
//! a subset of the available attributes for the resource" and say so operation by operation
//! (`ListRoles`, `ListUsers`, `ListPolicies`, `ListInstanceProfiles`,
//! `ListVirtualMFADevices`); those are pinned by the listing suites beside this one
//! (`tests/all/role_listing.rs`, `tests/all/instance_profile_listing.rs`, …) and are not
//! re-asserted here.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour of an entity that was PERSISTED rather than a handler called directly. The
//! awsQuery harness is `tests/all/user_lifecycle.rs`'s and the per-entity create helpers are
//! the ones the tag suites own, so a request is built exactly one way across this crate.
//!
//! The suites are tables whose rows differ along what the rule generalises over: which
//! entity is read (seven kinds, each addressed its own way), how many tags it holds (none,
//! one, several), which reader answers, and — for the role — which optional members the
//! create supplied.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::instance_profile_tags::create_instance_profile;
use crate::open_id_connect_provider_tag_updates::create_provider;
use crate::policy_simulation::{ALLOWED, IMPLICIT_DENY, principal, simulate};
use crate::policy_tags::create_policy;
use crate::role_lifecycle::{borrowed, optional, param, reported_tags};
use crate::role_tags::{TRUST_POLICY, create_role, tags, with_tag_params};
use crate::saml_providers::{MetadataForm, metadata_document};
use crate::server_certificate_tag_updates::upload_certificate;
use crate::user_lifecycle::{call, element, router};
use crate::user_tags::create_user;

/// How many `<tag>` elements the body carries, counted on the OPEN tag so a member whose
/// content is a list of `<member>` elements is counted as well as an empty one.
fn element_count(body: &str, tag: &str) -> usize {
    body.matches(&format!("<{tag}>")).count()
}

/// The tag sets one row of the entity table is run with: the rows differ along the tag
/// count, which is what the rule generalises over — the empty set is the case the defect
/// was reported for, and the non-empty ones are what proves the empty report is not a
/// report of nothing.
fn tag_sets() -> Vec<(&'static str, Vec<(String, String)>)> {
    vec![
        ("no-tags", Vec::new()),
        ("one-tag", tags(&[("team", "payments")])),
        (
            "several-tags",
            tags(&[
                ("env", "test"),
                ("owner", "ops"),
                ("project/name", "emulator"),
            ]),
        ),
    ]
}

/// The seven entity kinds a `Tags` member is modelled on, as this suite addresses them.
/// Each is created and then read back through ITS OWN reader, which is the operation IAM
/// documents as the one that reports the entity's tags.
const TAGGED_ENTITIES: &[&str] = &[
    "managed policy",
    "role",
    "user",
    "instance profile",
    "OIDC provider",
    "SAML provider",
    "server certificate",
    "virtual MFA device",
];

/// Create the entity of `kind` carrying `held`, then answer the body of the response that
/// reports that entity's tags.
///
/// For every kind but the virtual MFA device that is a read of the created entity — the
/// round trip through the store. A virtual MFA device's `Tags` member is modelled on the
/// device the create answers with and on no later read of the device
/// (`ListVirtualMFADevices` withholds it and `ListMFADeviceTags` reports the tags alone),
/// so for that kind the create's own response is the report.
async fn report_of(
    router: &MultiServiceRouter,
    kind: &str,
    suffix: &str,
    held: &[(String, String)],
) -> String {
    match kind {
        "managed policy" => {
            let arn = create_policy(router, &format!("policy-{suffix}"), None, held).await;
            read(router, "GetPolicy", &[("PolicyArn", arn.as_str())], kind).await
        }
        "role" => {
            let name = format!("role-{suffix}");
            create_role(router, &name, held).await;
            read(router, "GetRole", &[("RoleName", name.as_str())], kind).await
        }
        "user" => {
            let name = format!("user-{suffix}");
            create_user(router, &name, "/", held).await;
            read(router, "GetUser", &[("UserName", name.as_str())], kind).await
        }
        "instance profile" => {
            let name = format!("profile-{suffix}");
            create_instance_profile(router, &name, held).await;
            read(
                router,
                "GetInstanceProfile",
                &[("InstanceProfileName", name.as_str())],
                kind,
            )
            .await
        }
        "OIDC provider" => {
            let arn = create_provider(
                router,
                &format!("https://oidc-{suffix}.example.com"),
                held,
            )
            .await;
            read(
                router,
                "GetOpenIDConnectProvider",
                &[("OpenIDConnectProviderArn", arn.as_str())],
                kind,
            )
            .await
        }
        "SAML provider" => {
            let arn = create_saml_provider(router, &format!("saml-{suffix}"), held).await;
            read(
                router,
                "GetSAMLProvider",
                &[("SAMLProviderArn", arn.as_str())],
                kind,
            )
            .await
        }
        "server certificate" => {
            let name = format!("cert-{suffix}");
            upload_certificate(router, &name, None, held).await;
            read(
                router,
                "GetServerCertificate",
                &[("ServerCertificateName", name.as_str())],
                kind,
            )
            .await
        }
        "virtual MFA device" => {
            let params = with_tag_params(
                vec![param("VirtualMFADeviceName", &format!("mfa-{suffix}"))],
                held,
            );
            let (status, body) =
                call(router, "CreateVirtualMFADevice", &borrowed(&params)).await;
            assert_eq!(
                status, 200,
                "case {kind:?}/{suffix:?}: the fixture CreateVirtualMFADevice must succeed, \
                 got {status}: {body}"
            );
            body
        }
        other => panic!("the table names no reader for {other:?}"),
    }
}

/// One read that must succeed, answering its body.
async fn read(
    router: &MultiServiceRouter,
    operation: &str,
    params: &[(&str, &str)],
    kind: &str,
) -> String {
    let (status, body) = call(router, operation, params).await;
    assert_eq!(
        status, 200,
        "case {kind:?}: {operation} must report the created entity, got {status}: {body}"
    );
    body
}

/// Create the SAML provider `name` carrying `held`, answering the ARN it is held under.
///
/// `CreateSAMLProvider` takes a metadata document rather than a path or a document of its
/// own, so this suite builds one through the shared generator
/// (`tests/all/saml_providers.rs`); nothing here is about the document.
async fn create_saml_provider(
    router: &MultiServiceRouter,
    name: &str,
    held: &[(String, String)],
) -> String {
    let document = metadata_document(MetadataForm::Entity, "https://idp.example.com/saml", None);
    let params = with_tag_params(
        vec![param("Name", name), param("SAMLMetadataDocument", &document)],
        held,
    );
    let (status, body) = call(router, "CreateSAMLProvider", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the fixture CreateSAMLProvider must succeed, got {status}: {body}"
    );
    element(&body, "SAMLProviderArn")
}

/// RULE: the reader of a tagged IAM entity reports its `Tags` member whatever the entity
/// holds — the tags it carries, and the EMPTY list for one carrying none.
#[tokio::test]
async fn every_reader_of_a_tagged_entity_reports_its_tag_set() {
    let router = router();
    for kind in TAGGED_ENTITIES {
        for (count_case, held) in tag_sets() {
            let case = format!("{kind} with {count_case}");
            let suffix = format!(
                "{}-{count_case}",
                kind.replace(' ', "-").to_ascii_lowercase()
            );
            let body = report_of(&router, kind, &suffix, &held).await;
            let reported = element_count(&body, "Tags");
            assert_eq!(
                reported, 1,
                "case {case:?}: the report must carry exactly one <Tags> element, got \
                 {reported}: {body}"
            );
            let expected: BTreeMap<String, String> = held.iter().cloned().collect();
            let actual = reported_tags(&body);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the held tags {expected:?}, got {actual:?}: {body}"
            );
        }
    }
}

/// One role-creation row: the optional members the create supplies, beside the description
/// the read must report back.
struct RoleCase {
    case: &'static str,
    description: Option<&'static str>,
    held: Vec<(String, String)>,
    max_session_duration: Option<&'static str>,
}

/// RULE: a role that has never been assumed is reported with an EMPTY `RoleLastUsed`
/// detail — the member is the report of the role's last use, and nothing is invented for a
/// role nothing has used — and every other member the create supplied is reported back.
#[tokio::test]
async fn every_reader_of_a_role_reports_an_empty_last_used_detail() {
    // Rows differ along what the rule generalises over: which optional members the role
    // was created with, since none of them is what decides whether the detail is reported.
    let rows = [
        RoleCase {
            case: "a bare role",
            description: None,
            held: Vec::new(),
            max_session_duration: None,
        },
        RoleCase {
            case: "a role created with a description",
            description: Some("sim2real probe"),
            held: Vec::new(),
            max_session_duration: None,
        },
        RoleCase {
            case: "a tagged role",
            description: None,
            held: tags(&[("team", "payments")]),
            max_session_duration: None,
        },
        RoleCase {
            case: "a role created with every optional member",
            description: Some("every member supplied"),
            held: tags(&[("env", "test"), ("owner", "ops")]),
            max_session_duration: Some("7200"),
        },
    ];

    let router = router();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let role_name = format!("last-used-role-{index}");
        let mut leading = vec![
            param("RoleName", &role_name),
            param("AssumeRolePolicyDocument", TRUST_POLICY),
        ];
        if let Some(description) = row.description {
            leading.push(param("Description", description));
        }
        if let Some(duration) = row.max_session_duration {
            leading.push(param("MaxSessionDuration", duration));
        }
        let params = with_tag_params(leading, &row.held);
        let (status, created) = call(&router, "CreateRole", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateRole must succeed, got {status}: {created}"
        );

        let (status, fetched) = call(&router, "GetRole", &[("RoleName", role_name.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetRole must report the created role, got {status}: {fetched}"
        );

        // Both readers of the role report the member, so a caller cannot see it on one and
        // miss it on the other.
        for (reader, body) in [("CreateRole", &created), ("GetRole", &fetched)] {
            let detail = optional(body, "RoleLastUsed");
            assert_eq!(
                detail.as_deref(),
                Some(""),
                "case {case:?}: {reader} must report an empty <RoleLastUsed>, got \
                 {detail:?}: {body}"
            );
            // Nothing is invented for a role that was never used: the detail carries
            // neither of its two members.
            assert_eq!(
                element_count(body, "LastUsedDate"),
                0,
                "case {case:?}: {reader} must report no last-used instant, got {body}"
            );
            // The description the create supplied is reported back by both readers, the
            // one other member this row varies.
            let reported = optional(body, "Description");
            assert_eq!(
                reported.as_deref(),
                row.description,
                "case {case:?}: {reader} must report the supplied description, got \
                 {reported:?}: {body}"
            );
        }
    }
}

/// RULE: every evaluation result a simulation answers with reports the
/// `OrganizationsDecisionDetail` the decision was reached under — an allow where no service
/// control policy caps the simulated account, since the member reports what the policies in
/// force decided rather than whether the request named any.
#[tokio::test]
async fn every_simulated_decision_reports_what_organizations_decided() {
    // Rows differ along what the rule generalises over: the number of actions the page
    // decides and the decision each is reported with, neither of which is what decides
    // whether the detail is reported.
    let rows: Vec<(&str, &[&str], Vec<&str>)> = vec![
        ("one action, allowed", &["iam:ListRoles"], vec![ALLOWED]),
        (
            "one action nothing grants",
            &["iam:DeleteUser"],
            vec![IMPLICIT_DENY],
        ),
        (
            "three actions, allowed and denied",
            &["iam:ListRoles", "iam:DeleteUser", "iam:ListGroups"],
            vec![ALLOWED, IMPLICIT_DENY, ALLOWED],
        ),
    ];

    let router = router();
    let allow_lists = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:ListGroups","iam:ListRoles"],"Resource":"*"}]}"#;
    for (index, (case, actions, expected)) in rows.iter().enumerate() {
        let arn = principal(
            &router,
            case,
            &format!("simulated-principal-{index}"),
            &[("allow-lists", allow_lists)],
        )
        .await;
        let (status, body) = simulate(&router, &arn, actions, &[], &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the simulation must succeed, got {status}: {body}"
        );
        let decisions = crate::policy_simulation::reported(&body);
        let reported: Vec<&str> = decisions
            .iter()
            .map(|(_, decision)| decision.as_str())
            .collect();
        assert_eq!(
            reported, *expected,
            "case {case:?}: expected the decisions {expected:?}, got {reported:?}: {body}"
        );
        // One detail per result the page carries, each stating the decision was not an
        // Organizations one.
        let details = element_count(&body, "OrganizationsDecisionDetail");
        assert_eq!(
            details,
            expected.len(),
            "case {case:?}: every one of the {} results must carry an \
             <OrganizationsDecisionDetail>, got {details}: {body}",
            expected.len()
        );
        let allowed = crate::user_lifecycle::elements(&body, "AllowedByOrganizations");
        assert_eq!(
            allowed,
            vec!["true".to_string(); expected.len()],
            "case {case:?}: no service control policy caps the account, so every result \
             must report an allow, got {allowed:?}: {body}"
        );
    }
}
