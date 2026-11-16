// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal three IAM request classes carry, end to end over the awsQuery wire.
//!
//! The error code and the HTTP status are what a client branches on, so each suite below
//! pins the code, the status and the sentence of one class of refused request — and, where
//! the refused call is a write, that the refusal changed nothing.
//!
//! The rules under test:
//!
//! 1. A member that names a resource BY ARN is read before the store is consulted: a value
//!    that is not an ARN at all is the invalid input the operation models, an ARN whose
//!    RESOURCE TYPE the member cannot address is a request-validation failure (HTTP 400,
//!    `ValidationError`, `Invalid resource type in ARN`), and only an ARN of the addressed
//!    type reaches the lookup — whose miss is the entity-absent answer (HTTP 404).
//! 2. A bound on how many subordinate entities a resource may hold is taken before
//!    anything is said about WHICH entity a request names, so every add to a resource
//!    already at the bound is the quota refusal (HTTP 409, `LimitExceeded`) naming the
//!    quota and its value — the entity the resource already holds included.
//! 3. A name resolved against a published CATALOG that holds no entry for it names no such
//!    entity, so it is the entity-absent answer (HTTP 404, `NoSuchEntity`) in the words IAM
//!    states that catalog miss with.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour of state that was PERSISTED rather than a handler called directly. The
//! awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the metadata document
//! a SAML create needs comes from `tests/all/saml_providers.rs`, so a request is built and
//! read exactly one way across this crate.
//!
//! Each suite is a TABLE whose rows differ along what its rule generalises over: the
//! operation, the resource type the ARN names and which ARN field carries it, the role the
//! second add names and the case it is spelled in, and the shape of the catalog name.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::role_lifecycle::{borrowed, param, reported_tags};
use crate::saml_providers::{MetadataForm, expected_arn, metadata_document};
use crate::user_lifecycle::{account_id, call, element, elements, error_code, router};

// ---------------------------------------------------------------------------
// RULE 1: an ARN of the wrong resource type
// ---------------------------------------------------------------------------

/// The provider every row of the first suite leaves alone.
const HELD_PROVIDER: &str = "CorpIdP";

/// The tag that provider is created with, so a refused tag or untag call can be shown to
/// have changed nothing.
const HELD_TAG: (&str, &str) = ("team", "federation");

/// The operations taking a `SAMLProviderArn`, each with the OTHER parameters its request
/// needs, so every row drives a complete request. Source: the `SAMLProviderArn` member of
/// each operation (`smithy_query(query="operation",
/// operation="GetSAMLProvider,DeleteSAMLProvider,UpdateSAMLProvider,TagSAMLProvider,UntagSAMLProvider,ListSAMLProviderTags")`).
fn provider_operations() -> Vec<(&'static str, Vec<(String, String)>)> {
    vec![
        ("GetSAMLProvider", vec![]),
        ("ListSAMLProviderTags", vec![]),
        (
            "UpdateSAMLProvider",
            vec![param("AssertionEncryptionMode", "Allowed")],
        ),
        (
            "TagSAMLProvider",
            vec![
                param("Tags.member.1.Key", "owner"),
                param("Tags.member.1.Value", "identity"),
            ],
        ),
        (
            "UntagSAMLProvider",
            vec![param("TagKeys.member.1", HELD_TAG.0)],
        ),
        ("DeleteSAMLProvider", vec![]),
    ]
}

/// Well-formed ARNs — the `arn:` frame and its six colon-separated fields — that name no
/// SAML provider, so no `SAMLProviderArn` member can address any of them.
///
/// The rows differ along WHICH field makes the ARN name another resource: the resource
/// type (another IAM type, and one of the types this very family's neighbours use), the
/// service, a resource body carrying no type at all, the type with an empty name, and the
/// region field, which an IAM ARN leaves empty because IAM is a global service. Source for
/// the grammar a SAML provider's ARN follows: `state::saml_provider_arn`, which is what
/// `CreateSAMLProvider` holds a provider under.
fn arns_of_another_resource() -> Vec<(&'static str, String)> {
    let account = account_id();
    vec![
        (
            "another IAM resource type",
            format!("arn:aws:iam::{account}:user/federation-admin"),
        ),
        (
            "the other federation provider type",
            format!("arn:aws:iam::{account}:oidc-provider/idp.example.com"),
        ),
        (
            "another service's resource",
            "arn:aws:s3:::corporate-federation-metadata".to_string(),
        ),
        (
            "a resource body with no type",
            format!("arn:aws:iam::{account}:corp-idp-without-a-type"),
        ),
        (
            "the provider type with no name",
            format!("arn:aws:iam::{account}:saml-provider/"),
        ),
        (
            "a region-scoped IAM ARN",
            format!("arn:aws:iam:us-east-1:{account}:saml-provider/{HELD_PROVIDER}"),
        ),
    ]
}

/// Create [`HELD_PROVIDER`] carrying [`HELD_TAG`], and answer the ARN it is held under.
async fn create_held_provider(router: &smithy_server_runtime::MultiServiceRouter) -> String {
    let document = metadata_document(MetadataForm::Entity, "https://idp.example.com/saml", None);
    let params = vec![
        param("Name", HELD_PROVIDER),
        param("SAMLMetadataDocument", &document),
        param("Tags.member.1.Key", HELD_TAG.0),
        param("Tags.member.1.Value", HELD_TAG.1),
    ];
    let (status, body) = call(router, "CreateSAMLProvider", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the fixture CreateSAMLProvider must succeed, got {status}: {body}"
    );
    let arn = element(&body, "SAMLProviderArn");
    assert_eq!(
        arn,
        expected_arn(HELD_PROVIDER),
        "the fixture provider must be held under the ARN derived from its name, got {arn:?}"
    );
    arn
}

/// RULE 1: every operation naming a SAML provider by ARN refuses an ARN of another
/// resource type as a request-validation failure — HTTP 400, `ValidationError`, `Invalid
/// resource type in ARN` — without consulting the store, while an ARN that DOES name a
/// provider the account does not hold is the different, entity-absent answer (HTTP 404
/// `NoSuchEntity`). Neither refusal changes the provider the account holds.
#[tokio::test]
async fn an_arn_of_another_resource_type_is_a_validation_failure() {
    let router = router();
    let held = create_held_provider(&router).await;
    let absent = expected_arn("NeverFederatedHere");

    for (action, extra) in provider_operations() {
        for (what, arn) in arns_of_another_resource() {
            let case = format!("{action} with {what}");
            let mut params = vec![param("SAMLProviderArn", &arn)];
            params.extend(extra.iter().cloned());
            let (status, body) = call(&router, action, &borrowed(&params)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationError",
                "case {case:?}: expected ValidationError, got {:?}: {body}",
                error_code(&body)
            );
            assert_eq!(
                element(&body, "Message"),
                "Invalid resource type in ARN",
                "case {case:?}: expected IAM's wrong-resource-type sentence, got {body}"
            );
        }

        // The SAME call, handed an ARN that does name a SAML provider, reaches the lookup:
        // a provider the account does not hold is reported absent, not invalid.
        let case = format!("{action} with an unheld provider");
        let mut params = vec![param("SAMLProviderArn", &absent)];
        params.extend(extra.iter().cloned());
        let (status, body) = call(&router, action, &borrowed(&params)).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            format!("Manifest not found for arn {absent}"),
            "case {case:?}: the absence must name the ARN the request asked for, got {body}"
        );
    }

    // Every refusal above left the held provider exactly as its create wrote it: it is
    // still readable, still carries the tag it was created with, and is still the only
    // provider listed.
    let read = vec![param("SAMLProviderArn", &held)];
    let (status, body) = call(&router, "GetSAMLProvider", &borrowed(&read)).await;
    assert_eq!(
        status, 200,
        "the held provider must still read back, got {status}: {body}"
    );
    let held_tags = reported_tags(&body);
    let expected_tags: std::collections::BTreeMap<String, String> =
        [(HELD_TAG.0.to_string(), HELD_TAG.1.to_string())]
            .into_iter()
            .collect();
    assert_eq!(
        held_tags, expected_tags,
        "a refused call must leave the held tags alone, got {held_tags:?}"
    );
    let (status, body) = call(&router, "ListSAMLProviders", &[]).await;
    assert_eq!(
        status, 200,
        "ListSAMLProviders must answer 200, got {status}: {body}"
    );
    let listed = elements(&body, "Arn");
    assert_eq!(
        listed,
        vec![held.clone()],
        "a refused call must leave the account's providers alone, got {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 2: the one-role quota is taken before any sameness check
// ---------------------------------------------------------------------------

/// A trust policy with a single statement, so `CreateRole` accepts the fixture roles.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// RULE 2: "an instance profile can contain only one role, and this quota cannot be
/// increased" (`AddRoleToInstanceProfile`), so EVERY add to a profile that already holds a
/// role is refused with the quota — HTTP 409 `LimitExceeded`, `Cannot exceed quota for
/// InstanceSessionsPerInstanceProfile: 1` — whichever role it names, and the profile keeps
/// the role it held.
///
/// The rows differ along the role the second add names: the role the profile already holds
/// spelled as it was added, that same role in two other cases (IAM entity names "are not
/// distinguished by case", so these name the SAME role), a different role at the account
/// root and a different role under a path.
#[tokio::test]
async fn a_second_role_on_an_instance_profile_is_refused_by_the_one_role_quota() {
    let router = router();
    let profile = "BuildProfile";
    let (status, body) = call(
        &router,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture CreateInstanceProfile must succeed, got {status}: {body}"
    );
    for (role, path) in [("HeldRole", "/"), ("OtherRole", "/"), ("PathedRole", "/eng/")] {
        let (status, body) = call(
            &router,
            "CreateRole",
            &[
                ("RoleName", role),
                ("Path", path),
                ("AssumeRolePolicyDocument", TRUST_POLICY),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "the fixture CreateRole for {role:?} must succeed, got {status}: {body}"
        );
    }
    let (status, body) = call(
        &router,
        "AddRoleToInstanceProfile",
        &[("InstanceProfileName", profile), ("RoleName", "HeldRole")],
    )
    .await;
    assert_eq!(
        status, 200,
        "the first add must be served, got {status}: {body}"
    );

    // The published quota, and the sentence IAM states it in.
    let expected = format!(
        "Cannot exceed quota for InstanceSessionsPerInstanceProfile: {}",
        state::MAX_ROLES_PER_INSTANCE_PROFILE
    );
    assert_eq!(
        state::MAX_ROLES_PER_INSTANCE_PROFILE, 1,
        "an instance profile's role quota is one role, got {}",
        state::MAX_ROLES_PER_INSTANCE_PROFILE
    );

    for role in [
        "HeldRole",
        "heldrole",
        "HELDROLE",
        "OtherRole",
        "PathedRole",
    ] {
        let (status, body) = call(
            &router,
            "AddRoleToInstanceProfile",
            &[("InstanceProfileName", profile), ("RoleName", role)],
        )
        .await;
        assert_eq!(
            status, 409,
            "case {role:?}: expected HTTP 409, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "LimitExceeded",
            "case {role:?}: expected LimitExceeded, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            expected,
            "case {role:?}: expected the quota IAM names for this limit, got {body}"
        );
    }

    let (status, body) = call(
        &router,
        "GetInstanceProfile",
        &[("InstanceProfileName", profile)],
    )
    .await;
    assert_eq!(
        status, 200,
        "GetInstanceProfile must answer 200, got {status}: {body}"
    );
    let held = elements(&body, "RoleName");
    assert_eq!(
        held,
        vec!["HeldRole".to_string()],
        "a refused add must leave the profile holding the role it held, got {held:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 3: a name the credential-issuing catalog does not hold
// ---------------------------------------------------------------------------

/// The user every row of the credential suite names.
const CREDENTIAL_USER: &str = "federation.operator";

/// RULE 3: a `ServiceName` the catalog of services that issue service-specific credentials
/// holds no entry for names no such service, so the create and the filtered listing alike
/// answer HTTP 404 `NoSuchEntity` with `No such service <name> is supported for Service
/// Specific Credentials`, and nothing is minted — while every name the catalog DOES hold is
/// served and read back.
///
/// The rows differ along the shape of the unheld name: a service that issues no such
/// credential, a bare label that is no service principal, a name extending a held one, a
/// name prefixing a held one, one differing from a held entry only outside its DNS name,
/// and the empty name (`serviceName` carries no length constraint). The served rows are
/// every entry of `state::SERVICES_WITH_SPECIFIC_CREDENTIALS`, whose source is
/// `CreateServiceSpecificCredential`'s own documentation.
#[tokio::test]
async fn a_service_name_outside_the_credential_catalog_is_not_found() {
    let router = router();
    let (status, body) = call(&router, "CreateUser", &[("UserName", CREDENTIAL_USER)]).await;
    assert_eq!(
        status, 200,
        "the fixture CreateUser must succeed, got {status}: {body}"
    );

    for service in [
        "dynamodb.amazonaws.com",
        "codecommit",
        "codecommit.amazonaws.com.example.net",
        "notcodecommit.amazonaws.com",
        "codecommit.amazonaws.com.",
        "",
    ] {
        let expected =
            format!("No such service {service} is supported for Service Specific Credentials");
        for action in [
            "CreateServiceSpecificCredential",
            "ListServiceSpecificCredentials",
        ] {
            let case = format!("{action} for {service:?}");
            let (status, body) = call(
                &router,
                action,
                &[("UserName", CREDENTIAL_USER), ("ServiceName", service)],
            )
            .await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "NoSuchEntity",
                "case {case:?}: expected NoSuchEntity, got {:?}: {body}",
                error_code(&body)
            );
            assert_eq!(
                element(&body, "Message"),
                expected,
                "case {case:?}: expected IAM's catalog-miss sentence, got {body}"
            );
        }
    }

    // Nothing was minted by any refusal.
    let (status, body) = call(
        &router,
        "ListServiceSpecificCredentials",
        &[("UserName", CREDENTIAL_USER)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the unfiltered listing must answer 200, got {status}: {body}"
    );
    assert!(
        elements(&body, "ServiceSpecificCredentialId").is_empty(),
        "a refused create must mint nothing, got {body}"
    );

    // Every name the catalog DOES hold is served, and the credential it mints is reported
    // by the listing filtered on that same name.
    for entry in state::SERVICES_WITH_SPECIFIC_CREDENTIALS {
        let service = entry.service_name;
        let (status, body) = call(
            &router,
            "CreateServiceSpecificCredential",
            &[("UserName", CREDENTIAL_USER), ("ServiceName", service)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {service:?}: a catalog entry must be served, got {status}: {body}"
        );
        let minted = element(&body, "ServiceSpecificCredentialId");
        assert!(
            !minted.is_empty(),
            "case {service:?}: the create must report the credential it minted, got {body}"
        );

        let (status, body) = call(
            &router,
            "ListServiceSpecificCredentials",
            &[("UserName", CREDENTIAL_USER), ("ServiceName", service)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {service:?}: the filtered listing must answer 200, got {status}: {body}"
        );
        let listed = elements(&body, "ServiceSpecificCredentialId");
        assert_eq!(
            listed,
            vec![minted.clone()],
            "case {service:?}: the listing must report the credential that was minted, got \
             {listed:?}"
        );
    }
}
