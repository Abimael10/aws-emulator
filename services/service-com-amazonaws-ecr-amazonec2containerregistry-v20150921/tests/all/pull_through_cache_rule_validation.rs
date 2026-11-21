// SPDX-License-Identifier: CC0-1.0

//! `ValidatePullThroughCacheRule`, end to end over the wire.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes.
//!
//! The rule under test: validation is a verdict on the rule the registry HOLDS, computed
//! from that stored record and nothing else. A rule validates — HTTP 200 with
//! `isValid: true` and an empty `failure` — when its stored configuration can
//! authenticate to its upstream: the upstream is one ECR documents as serving
//! anonymously, or the rule stores a credential of the kind its member documents (a
//! Secrets Manager `ecr-pullthroughcache/` secret for `credentialArn`, an IAM role for
//! `customRoleArn`). It is invalid — `isValid: false` — only with a `failure` naming the
//! stored configuration defect: a missing credential an authenticating upstream needs, a
//! credential that is not the ARN it has to be, or a rule naming no upstream at all.
//! Nothing about the emulator serving the call — in particular its lack of outbound
//! connectivity — is part of the verdict, and validating changes nothing.

use serde_json::{json, Value};

use crate::wire::{
    at, call, emulator_with_engine, ok, seed_credential_secret, seed_pull_through_cache_rule,
};

/// A Secrets Manager secret ARN in the namespace `CredentialArn`'s modelled pattern
/// states (smithy_query operation=ValidatePullThroughCacheRule, member credentialArn:
/// `^arn:aws(-\w+)*:secretsmanager:[a-zA-Z0-9-:]+:secret:ecr\-pullthroughcache\/[a-zA-Z0-9\/_+=.@-]+$`).
const SECRET: &str =
    "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/upstream-AbCdEf";

/// A second secret, so a rule's credential is visibly its own.
const OTHER_SECRET: &str =
    "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/other-ZyXwVu";

/// An IAM role ARN for `customRoleArn`, documented as "the ARN of the IAM role associated
/// with the pull through cache rule"; the model constrains the member only by length.
const ROLE: &str = "arn:aws:iam::000000000000:role/ecr-pull-through-cache";

/// One row of the validation table: the rule to create and the verdict it earns.
struct Case<'a> {
    name: &'a str,
    prefix: &'a str,
    /// `upstreamRegistry` as the create request carries it; `None` leaves the member out,
    /// so the registry is resolved from the URL.
    upstream_registry: Option<&'a str>,
    upstream_registry_url: &'a str,
    credential_arn: Option<&'a str>,
    custom_role_arn: Option<&'a str>,
    /// The verdict `isValid` must carry.
    valid: bool,
    /// A fragment the `failure` must name, for a row that does not validate.
    failure_names: Option<&'a str>,
}

/// The validation table. The rows span the model's `UpstreamRegistry` values (smithy_query
/// shape=UpstreamRegistry) with the URL syntax the `upstreamRegistryUrl` member documents
/// for each, and differ in which credential the rule stores and in whether the upstream
/// needs one — including upstreams the issue never cited. Which upstreams are cached from
/// anonymously is the ECR user guide's supported-upstream table (Amazon ECR Public,
/// Kubernetes, Quay); every other upstream authenticates its callers.
fn cases() -> Vec<Case<'static>> {
    vec![
        Case {
            name: "ecr-public serves anonymously, so a rule with no credential is complete",
            prefix: "ecr-public-cache",
            upstream_registry: None,
            upstream_registry_url: "public.ecr.aws",
            credential_arn: None,
            custom_role_arn: None,
            valid: true,
            failure_names: None,
        },
        Case {
            name: "k8s serves anonymously too",
            prefix: "k8s-cache",
            upstream_registry: None,
            upstream_registry_url: "registry.k8s.io",
            credential_arn: None,
            custom_role_arn: None,
            valid: true,
            failure_names: None,
        },
        Case {
            name: "an anonymous upstream with a credential it does not need",
            prefix: "ecr-public-extra",
            upstream_registry: Some("ecr-public"),
            upstream_registry_url: "public.ecr.aws",
            credential_arn: Some(SECRET),
            custom_role_arn: None,
            valid: true,
            failure_names: None,
        },
        Case {
            name: "docker-hub with the secret its authentication needs",
            prefix: "docker-hub-cache",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            credential_arn: Some(SECRET),
            custom_role_arn: None,
            valid: true,
            failure_names: None,
        },
        Case {
            name: "quay with a custom role instead of a secret",
            prefix: "quay-cache",
            upstream_registry: Some("quay"),
            upstream_registry_url: "quay.io",
            credential_arn: None,
            custom_role_arn: Some(ROLE),
            valid: true,
            failure_names: None,
        },
        Case {
            name: "azure-container-registry with both credentials",
            prefix: "azure-cache",
            upstream_registry: None,
            upstream_registry_url: "contoso.azurecr.io",
            credential_arn: Some(SECRET),
            custom_role_arn: Some(ROLE),
            valid: true,
            failure_names: None,
        },
        Case {
            name: "chainguard, a registry the model documents no URL for",
            prefix: "chainguard-cache",
            upstream_registry: Some("chainguard"),
            upstream_registry_url: "cgr.dev",
            credential_arn: Some(OTHER_SECRET),
            custom_role_arn: None,
            valid: true,
            failure_names: None,
        },
        Case {
            name: "an ECR upstream reached through a custom role",
            prefix: "ecr-cache",
            upstream_registry: None,
            upstream_registry_url: "000000000000.dkr.ecr.us-east-2.amazonaws.com",
            credential_arn: None,
            custom_role_arn: Some(ROLE),
            valid: true,
            failure_names: None,
        },
        Case {
            name: "github-container-registry reached through a role instead of a secret",
            prefix: "ghcr-cache",
            upstream_registry: None,
            upstream_registry_url: "ghcr.io",
            credential_arn: None,
            custom_role_arn: Some(ROLE),
            valid: true,
            failure_names: None,
        },
        Case {
            name: "gitlab-container-registry with an empty customRoleArn beside its secret",
            prefix: "gitlab-cache",
            upstream_registry: Some("gitlab-container-registry"),
            upstream_registry_url: "registry.gitlab.com",
            credential_arn: Some(SECRET),
            // `customRoleArn` is modelled `@length(0..2048)`, so the empty string is a
            // legal value — and it names no role, so the secret is what the rule
            // authenticates with.
            custom_role_arn: Some(""),
            valid: true,
            failure_names: None,
        },
        Case {
            name: "a customRoleArn that is not an ARN at all",
            prefix: "role-plain-cache",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            credential_arn: None,
            custom_role_arn: Some("ecr-pull-through-cache"),
            valid: false,
            failure_names: Some("customRoleArn"),
        },
        Case {
            name: "a customRoleArn naming another service's resource",
            prefix: "role-vendor-cache",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            credential_arn: Some(SECRET),
            custom_role_arn: Some("arn:aws:s3:::ecr-pull-through-cache"),
            valid: false,
            failure_names: Some("customRoleArn"),
        },
        Case {
            name: "a customRoleArn naming an IAM resource that is no role",
            prefix: "role-type-cache",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            credential_arn: None,
            custom_role_arn: Some("arn:aws:iam::000000000000:user/ecr-pull-through-cache"),
            valid: false,
            failure_names: Some("customRoleArn"),
        },
    ]
}

/// The `CreatePullThroughCacheRule` request one row describes.
fn create_request(case: &Case<'_>) -> Value {
    let mut request = json!({
        "ecrRepositoryPrefix": case.prefix,
        "upstreamRegistryUrl": case.upstream_registry_url,
    });
    let members = [
        ("upstreamRegistry", case.upstream_registry),
        ("credentialArn", case.credential_arn),
        ("customRoleArn", case.custom_role_arn),
    ];
    for (member, value) in members {
        if let Some(value) = value {
            request[member] = json!(value);
        }
    }
    request
}

/// Assert the verdict `body` carries is the one `case` expects, and that its reason is
/// about the rule rather than about the emulator answering the call.
fn assert_verdict(case: &str, valid: bool, failure_names: Option<&str>, body: &Value) {
    assert_eq!(
        at(body, "/isValid"),
        json!(valid),
        "case {case:?}: expected isValid {valid}, got {:?}",
        at(body, "/isValid")
    );
    let failure = at(body, "/failure").as_str().unwrap_or_default().to_string();
    assert_eq!(
        failure.is_empty(),
        valid,
        "case {case:?}: a rule that validated carries an empty failure and one that did \
         not says why, got {failure:?}"
    );
    if let Some(fragment) = failure_names {
        assert!(
            failure.contains(fragment),
            "case {case:?}: the failure must name {fragment:?}, got {failure:?}"
        );
    }
    // The verdict is about the rule: the emulator's own lack of egress is not a property
    // of a pull through cache rule, so it is never the reason one is invalid.
    for forbidden in ["emulator", "outbound", "egress", "could not be verified"] {
        assert!(
            !failure.to_lowercase().contains(forbidden),
            "case {case:?}: the failure must describe the rule, not the service answering \
             the call, got {failure:?}"
        );
    }
}

/// The one rule the registry holds for `prefix`, as `DescribePullThroughCacheRules`
/// reports it.
async fn described(
    router: &::smithy_server_runtime::MultiServiceRouter,
    case: &str,
    prefix: &str,
) -> Value {
    let body = ok(
        router,
        case,
        "DescribePullThroughCacheRules",
        &json!({"ecrRepositoryPrefixes": [prefix]}),
    )
    .await;
    at(&body, "/pullThroughCacheRules/0")
}

#[tokio::test]
async fn a_rules_verdict_is_read_off_its_stored_configuration() {
    let (router, engine) = emulator_with_engine();
    // Every secret a row's rule names, where Secrets Manager keeps it: a credential the
    // rule stores is one ECR retrieves the upstream's credential from, so a row whose
    // credential is meant to be USABLE needs that secret to exist. The rows that turn on
    // the SYNTAX of a credential name no secret of this store at all.
    for credential in [SECRET, OTHER_SECRET] {
        seed_credential_secret(&engine, credential, false);
    }

    for case in cases() {
        ok(
            &router,
            case.name,
            "CreatePullThroughCacheRule",
            &create_request(&case),
        )
        .await;

        let validated = ok(
            &router,
            case.name,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": case.prefix}),
        )
        .await;
        assert_verdict(case.name, case.valid, case.failure_names, &validated);

        // The answer is the stored rule projected onto this operation's output shape, so
        // the members it reports are the ones the reporting operation reports.
        let rule = described(&router, case.name, case.prefix).await;
        for member in [
            "/ecrRepositoryPrefix",
            "/upstreamRegistryUrl",
            "/registryId",
            "/credentialArn",
            "/customRoleArn",
            "/upstreamRepositoryPrefix",
        ] {
            assert_eq!(
                at(&validated, member),
                at(&rule, member),
                "case {:?}: the validate reports the stored {member}, got {:?} against \
                 the stored {:?}",
                case.name,
                at(&validated, member),
                at(&rule, member)
            );
        }

        // Validating is a read: the second call answers the same verdict and the stored
        // rule is untouched.
        let again = ok(
            &router,
            case.name,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": case.prefix}),
        )
        .await;
        assert_eq!(
            again, validated,
            "case {:?}: validating twice answers the same verdict, got {again:?}",
            case.name
        );
        assert_eq!(
            described(&router, case.name, case.prefix).await,
            rule,
            "case {:?}: validating changed the stored rule",
            case.name
        );
    }
}

/// One row of the upstream table: the upstream a rule names, and whether a rule for it
/// that stores NO credential of any kind validates.
struct Upstream<'a> {
    case: &'a str,
    /// The prefix the rule is held under — its own, so no row reads another's rule.
    prefix: &'a str,
    /// `upstreamRegistry` as the rule carries it; `None` leaves the member out, so the
    /// upstream is resolved from the URL.
    upstream_registry: Option<&'a str>,
    upstream_registry_url: &'a str,
    /// The verdict a rule for that upstream with no credential stored earns.
    valid: bool,
}

/// Whether a rule needs a credential at all is a property of the UPSTREAM it caches from:
/// the registries ECR caches from without authenticating need none, and every other
/// upstream is reached with the credential the rule carries.
///
/// The rows span the model's `UpstreamRegistry` values (smithy_query shape=UpstreamRegistry)
/// with the URL the `upstreamRegistryUrl` member documents for each, every one of them
/// storing no credential — including upstreams the issue never cited. Which of them ECR
/// caches from anonymously is the ECR user guide's supported-upstream table: Amazon ECR
/// Public, Kubernetes and Quay, the public registries whose images are pulled without
/// credentials; the rest authenticate their callers.
///
/// The rules are seeded: `CreatePullThroughCacheRule` refuses a rule for an upstream that
/// authenticates its callers when the request carries no credential, so a STORED record
/// of that shape is placed in the store the verdict is read from.
#[tokio::test]
async fn whether_a_rule_needs_a_credential_follows_from_its_upstream() {
    let rows = vec![
        Upstream {
            case: "Amazon ECR Public is cached from anonymously",
            prefix: "anon-ecr-public",
            upstream_registry: Some("ecr-public"),
            upstream_registry_url: "public.ecr.aws",
            valid: true,
        },
        Upstream {
            case: "the Kubernetes registry is cached from anonymously",
            prefix: "anon-k8s",
            upstream_registry: Some("k8s"),
            upstream_registry_url: "registry.k8s.io",
            valid: true,
        },
        Upstream {
            case: "Quay is cached from anonymously",
            prefix: "anon-quay",
            upstream_registry: Some("quay"),
            upstream_registry_url: "quay.io",
            valid: true,
        },
        Upstream {
            case: "Docker Hub authenticates its callers",
            prefix: "auth-docker-hub",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            valid: false,
        },
        Upstream {
            case: "GitHub Container Registry authenticates its callers",
            prefix: "auth-ghcr",
            upstream_registry: Some("github-container-registry"),
            upstream_registry_url: "ghcr.io",
            valid: false,
        },
        Upstream {
            case: "GitLab Container Registry authenticates its callers",
            prefix: "auth-gitlab",
            upstream_registry: Some("gitlab-container-registry"),
            upstream_registry_url: "registry.gitlab.com",
            valid: false,
        },
        Upstream {
            case: "Microsoft Azure Container Registry authenticates its callers",
            prefix: "auth-azure",
            upstream_registry: Some("azure-container-registry"),
            upstream_registry_url: "contoso.azurecr.io",
            valid: false,
        },
        Upstream {
            case: "an Amazon ECR upstream is reached with a role of this account",
            prefix: "auth-ecr",
            upstream_registry: Some("ecr"),
            upstream_registry_url: "000000000000.dkr.ecr.us-east-2.amazonaws.com",
            valid: false,
        },
    ];

    let (router, engine) = emulator_with_engine();
    for row in &rows {
        seed_pull_through_cache_rule(
            &engine,
            row.prefix,
            row.upstream_registry,
            row.upstream_registry_url,
            None,
            None,
        );

        let validated = ok(
            &router,
            row.case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": row.prefix}),
        )
        .await;
        assert_verdict(
            row.case,
            row.valid,
            (!row.valid).then_some("requires authentication"),
            &validated,
        );
        assert_eq!(
            at(&validated, "/upstreamRegistryUrl"),
            json!(row.upstream_registry_url),
            "case {:?}: the validate reports the stored upstream URL, got {:?}",
            row.case,
            at(&validated, "/upstreamRegistryUrl")
        );
    }
}

/// The same question answered for a rule that names NO `upstreamRegistry`: the upstream is
/// the one its URL addresses, whatever scheme, case, port, userinfo or path the URL was
/// written with — a host addresses one registry.
///
/// The rules are seeded: `CreatePullThroughCacheRule` resolves the upstream from the URL
/// itself, so a stored rule is how a record carrying only a URL is reached.
#[tokio::test]
async fn an_upstream_url_alone_decides_whether_a_rule_needs_a_credential() {
    let rows = vec![
        Upstream {
            case: "the bare host of an anonymous upstream",
            prefix: "url-quay",
            upstream_registry: None,
            upstream_registry_url: "quay.io",
            valid: true,
        },
        Upstream {
            case: "an anonymous upstream written with a scheme, odd case, a port and a path",
            prefix: "url-quay-full",
            upstream_registry: None,
            upstream_registry_url: "https://QUAY.IO:443/v2/",
            valid: true,
        },
        Upstream {
            case: "an anonymous upstream written with userinfo",
            prefix: "url-k8s",
            upstream_registry: None,
            upstream_registry_url: "https://anonymous@registry.k8s.io",
            valid: true,
        },
        Upstream {
            case: "the host of an upstream that authenticates its callers",
            prefix: "url-ghcr",
            upstream_registry: None,
            upstream_registry_url: "https://ghcr.io/v2/",
            valid: false,
        },
        Upstream {
            case: "a host no documented upstream URL names, so nothing says it is anonymous",
            prefix: "url-unknown",
            upstream_registry: None,
            upstream_registry_url: "cgr.dev",
            valid: false,
        },
    ];

    let (router, engine) = emulator_with_engine();
    for row in &rows {
        seed_pull_through_cache_rule(
            &engine,
            row.prefix,
            row.upstream_registry,
            row.upstream_registry_url,
            None,
            None,
        );
        let validated = ok(
            &router,
            row.case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": row.prefix}),
        )
        .await;
        assert_verdict(
            row.case,
            row.valid,
            (!row.valid).then_some("requires authentication"),
            &validated,
        );
    }
}

/// One row of the credential-syntax table: the case, the `credentialArn` the rule stores,
/// and whether that value is a credential ECR could read the upstream's secret from.
type CredentialRow<'a> = (&'a str, &'a str, bool);

#[tokio::test]
async fn a_credential_the_rule_stores_has_to_be_the_arn_its_member_documents() {
    // The request boundary refuses a `credentialArn` outside `CredentialArn`'s modelled
    // pattern, so these STORED records are seeded: the verdict still has to be read off
    // them rather than assumed.
    let rows: Vec<CredentialRow<'_>> = vec![
        ("the documented secret ARN", SECRET, true),
        ("another secret of the same namespace", OTHER_SECRET, true),
        ("a value that is not an ARN", "ecr-pullthroughcache/upstream", false),
        (
            "an ARN of another service",
            "arn:aws:s3:::ecr-pullthroughcache/upstream",
            false,
        ),
        (
            "a secret outside the pull through cache namespace",
            "arn:aws:secretsmanager:eu-west-1:000000000000:secret:other/upstream-AbCdEf",
            false,
        ),
        (
            "a secret ARN carrying no Region, which the modelled class still admits (its \
             `[a-zA-Z0-9-:]+` span covers the Region and account together, colon included)",
            "arn:aws:secretsmanager::000000000000:secret:ecr-pullthroughcache/upstream-AbCdEf",
            true,
        ),
        (
            "a secret of another AWS partition, which `aws(-\\w+)*` admits",
            "arn:aws-us-gov:secretsmanager:us-gov-west-1:000000000000:secret:ecr-pullthroughcache/upstream",
            true,
        ),
        (
            "a partition outside the modelled `aws(-\\w+)*`",
            "arn:azure:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/upstream",
            false,
        ),
        (
            "a secret name carrying a character outside the modelled class",
            "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/upstream#1",
            false,
        ),
        (
            "a secret ARN whose name stops at the namespace",
            "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/",
            false,
        ),
        (
            "a secret ARN naming another resource type",
            "arn:aws:secretsmanager:eu-west-1:000000000000:other:ecr-pullthroughcache/upstream",
            false,
        ),
    ];

    let (router, engine) = emulator_with_engine();
    for (case, credential, usable) in rows {
        // A value this table expects to be USABLE is one whose secret exists, so the
        // verdict turns on the SYNTAX the row is about rather than on a missing secret;
        // the Secrets Manager store answers nothing about the rest, which are values no
        // secret could be held under anyway.
        if usable {
            seed_credential_secret(&engine, credential, false);
        }
        // The upstream authenticates its callers, so the credential is what the rule's
        // validity turns on.
        seed_pull_through_cache_rule(
            &engine,
            "credential-cache",
            Some("docker-hub"),
            "registry-1.docker.io",
            Some(credential),
            None,
        );
        let validated = ok(
            &router,
            case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": "credential-cache"}),
        )
        .await;
        assert_verdict(
            case,
            usable,
            (!usable).then_some("credentialArn"),
            &validated,
        );
        assert_eq!(
            at(&validated, "/credentialArn"),
            json!(credential),
            "case {case:?}: the validate reports the credential the rule stores, got {:?}",
            at(&validated, "/credentialArn")
        );
    }
}

#[tokio::test]
async fn a_rule_naming_no_upstream_registry_does_not_validate() {
    // `CreatePullThroughCacheRule` refuses a request that resolves to no upstream
    // registry, so a stored rule with no upstream URL is seeded.
    let (router, engine) = emulator_with_engine();
    for (case, url) in [("no upstream URL", ""), ("a blank upstream URL", "   ")] {
        seed_pull_through_cache_rule(&engine, "upstreamless", None, url, None, None);
        let validated = ok(
            &router,
            case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": "upstreamless"}),
        )
        .await;
        assert_verdict(case, false, Some("upstreamRegistryUrl"), &validated);
    }
}

/// One row of the credential-secret table: what state the Secrets Manager secret a rule's
/// `credentialArn` names is in, and the verdict that earns the rule.
struct SecretRow<'a> {
    case: &'a str,
    /// The prefix the rule is held under — its own, so no row reads another's rule.
    prefix: &'a str,
    /// The secret the rule's `credentialArn` names.
    credential_arn: &'a str,
    /// Whether that secret is placed in Secrets Manager's store at all.
    secret_exists: bool,
    /// Whether a `DeleteSecret` has scheduled it for deletion.
    secret_deleted: bool,
    /// The `customRoleArn` the rule also stores, if any.
    custom_role_arn: Option<&'a str>,
    /// The verdict `isValid` must carry.
    valid: bool,
    /// The `failureCode` the failure document must carry, for a row that does not validate.
    failure_code: Option<&'a str>,
}

/// RULE: a rule whose upstream authenticates its callers validates only when the credential
/// it stores can really be used — and whether the Secrets Manager secret a `credentialArn`
/// names holds a retrievable credential is asked of Secrets Manager, the service that owns
/// it ("Amazon ECR will attempt to retrieve the contents of the Amazon Web Services Secrets
/// Manager secret, verify the syntax", `ValidatePullThroughCacheRule`). A rule that does not
/// validate reports a failure DOCUMENT: a `failureCode` a client branches on and a
/// `failureReason` stating it.
///
/// The rows differ along the state of what the credential points at — a secret that is
/// there, one that was never created, one scheduled for deletion, one storing none of the
/// documented credential fields — and along whether a usable `customRoleArn` stands beside
/// it, which is a credential of its own.
#[tokio::test]
async fn a_credential_secret_that_holds_no_credential_makes_the_rule_invalid() {
    const MISSING_SECRET: &str =
        "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/absent-AbCdEf";
    const DELETED_SECRET: &str =
        "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/deleted-AbCdEf";

    let rows = vec![
        SecretRow {
            case: "a secret the account holds, storing the upstream credential",
            prefix: "secret-held",
            credential_arn: SECRET,
            secret_exists: true,
            secret_deleted: false,
            custom_role_arn: None,
            valid: true,
            failure_code: None,
        },
        SecretRow {
            case: "a credentialArn naming a secret that was never created",
            prefix: "secret-absent",
            credential_arn: MISSING_SECRET,
            secret_exists: false,
            secret_deleted: false,
            custom_role_arn: None,
            valid: false,
            failure_code: Some("UNABLE_TO_ACCESS_SECRET"),
        },
        SecretRow {
            case: "a credentialArn whose secret has been deleted",
            prefix: "secret-deleted",
            credential_arn: DELETED_SECRET,
            secret_exists: true,
            secret_deleted: true,
            custom_role_arn: None,
            valid: false,
            failure_code: Some("UNABLE_TO_ACCESS_SECRET"),
        },
        SecretRow {
            case: "a deleted secret beside a customRoleArn, which authenticates on its own",
            prefix: "secret-deleted-with-role",
            credential_arn: DELETED_SECRET,
            secret_exists: true,
            secret_deleted: true,
            custom_role_arn: Some(ROLE),
            valid: true,
            failure_code: None,
        },
    ];

    let (router, engine) = emulator_with_engine();
    for row in &rows {
        if row.secret_exists {
            seed_credential_secret(&engine, row.credential_arn, row.secret_deleted);
        }
        // The upstream authenticates its callers, so the credential is what the rule's
        // validity turns on.
        seed_pull_through_cache_rule(
            &engine,
            row.prefix,
            Some("docker-hub"),
            "registry-1.docker.io",
            Some(row.credential_arn),
            row.custom_role_arn,
        );
        let validated = ok(
            &router,
            row.case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": row.prefix}),
        )
        .await;
        assert_verdict(row.case, row.valid, None, &validated);
        assert_eq!(
            failure_code(row.case, &validated).as_deref(),
            row.failure_code,
            "case {:?}: expected the failure code {:?}, got {:?}",
            row.case,
            row.failure_code,
            failure_code(row.case, &validated)
        );
    }

    // A secret whose value states none of the documented credential fields holds no
    // credential either, whatever else it holds: the secret is READ through Secrets
    // Manager's own API, so the rule's verdict follows the value a client wrote.
    let unusable =
        "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/blank-AbCdEf";
    crate::wire::seed_secret_value(&engine, unusable, r#"{"user":"x","token":"y"}"#);
    seed_pull_through_cache_rule(
        &engine,
        "secret-unusable",
        Some("docker-hub"),
        "registry-1.docker.io",
        Some(unusable),
        None,
    );
    let validated = ok(
        &router,
        "a secret storing none of the credential fields",
        "ValidatePullThroughCacheRule",
        &json!({"ecrRepositoryPrefix": "secret-unusable"}),
    )
    .await;
    assert_verdict(
        "a secret storing none of the credential fields",
        false,
        None,
        &validated,
    );
    assert_eq!(
        failure_code("a secret storing none of the credential fields", &validated).as_deref(),
        Some("INVALID_SECRET_FORMAT"),
        "a secret that stores no credential field is reported as the format it is not, got \
         {validated:?}"
    );
}

/// The `failureCode` the `failure` member's document carries, or `None` for a rule that
/// validated and so reports an empty `failure`.
///
/// The member is a string carrying a failure document, so a client parses it: a non-empty
/// `failure` that is not that document would be a defect, and the panic says so.
fn failure_code(case: &str, body: &Value) -> Option<String> {
    let failure = at(body, "/failure").as_str().unwrap_or_default().to_string();
    if failure.is_empty() {
        return None;
    }
    let document: Value = serde_json::from_str(&failure).unwrap_or_else(|error| {
        panic!("case {case:?}: the failure must be a document ({error}): {failure:?}")
    });
    let code = at(&document, "/failureCode")
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the failure document must carry a failureCode: {failure:?}")
        })
        .to_string();
    assert!(
        !at(&document, "/failureReason")
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "case {case:?}: the failure document must state a failureReason: {failure:?}"
    );
    Some(code)
}

#[tokio::test]
async fn a_rule_is_validated_by_either_spelling_of_its_prefix() {
    let (router, _engine) = emulator_with_engine();
    // "There is always an assumed / applied to the end of the prefix", so both spellings
    // address the one rule — and a prefix no rule answers to is a not-found rather than an
    // invalid rule.
    for (case, created, asked) in [
        ("the same spelling", "slashless-cache", "slashless-cache"),
        ("a trailing slash added", "added-slash-cache", "added-slash-cache/"),
        ("a trailing slash dropped", "dropped-slash-cache/", "dropped-slash-cache"),
    ] {
        ok(
            &router,
            case,
            "CreatePullThroughCacheRule",
            &json!({
                "ecrRepositoryPrefix": created,
                "upstreamRegistryUrl": "public.ecr.aws",
            }),
        )
        .await;
        let validated = ok(
            &router,
            case,
            "ValidatePullThroughCacheRule",
            &json!({"ecrRepositoryPrefix": asked}),
        )
        .await;
        assert_verdict(case, true, None, &validated);
        assert_eq!(
            at(&validated, "/ecrRepositoryPrefix"),
            json!(created),
            "case {case:?}: the rule answers with the prefix it was created under, got {:?}",
            at(&validated, "/ecrRepositoryPrefix")
        );
    }

    let answer = call(
        &router,
        "ValidatePullThroughCacheRule",
        &json!({"ecrRepositoryPrefix": "no-rule-cache"}),
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "a prefix no rule answers to is refused with HTTP 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer
            .error_code()
            .ends_with("PullThroughCacheRuleNotFoundException"),
        "expected PullThroughCacheRuleNotFoundException, got {:?}",
        answer.error_code()
    );
}
