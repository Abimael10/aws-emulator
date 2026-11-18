// SPDX-License-Identifier: CC0-1.0

//! Pull through cache rules, end to end over the wire.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes. The
//! rule under test: a pull through cache rule is whatever `CreatePullThroughCacheRule`
//! stored for one `ecrRepositoryPrefix` — the upstream registry (named by the request,
//! or resolved from the URL syntax the model documents per registry), its URL, the
//! upstream repository prefix (`ROOT` when the request omits it), the credential and
//! role ARNs and the creation instant — and `DescribePullThroughCacheRules` reports
//! exactly those records; a second rule for the same prefix is refused and changes
//! nothing, and so is a rule whose upstream registry ECR cannot cache from — a URL
//! addressing no supported registry, or an upstream that authenticates its callers with no
//! credential on the request to reach it with.

use serde_json::{json, Value};

use crate::wire::{at, call, emulator, ok, refused, REGISTRY_ID};

/// A Secrets Manager secret ARN that satisfies `CredentialArn`'s modelled pattern
/// (`^arn:aws(-\w+)*:secretsmanager:[a-zA-Z0-9-:]+:secret:ecr\-pullthroughcache\/...$`).
const CREDENTIAL_ARN: &str =
    "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/upstream-AbCdEf";

/// A second one, so a rule's own credential is visibly its own.
const OTHER_CREDENTIAL_ARN: &str =
    "arn:aws:secretsmanager:eu-west-1:000000000000:secret:ecr-pullthroughcache/other-ZyXwVu";

/// An IAM role ARN for `customRoleArn`, which the model constrains only by length.
const CUSTOM_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/ecr-pull-through-cache";

/// `upstreamRepositoryPrefix`'s documented default: "When this field isn't specified,
/// Amazon ECR will use the ROOT."
const ROOT: &str = "ROOT";

/// The wire error code ECR refuses an upstream registry it cannot cache from with
/// (smithy_query operation=CreatePullThroughCacheRule: `UnsupportedUpstreamRegistryException`,
/// httpError 400).
const UNSUPPORTED_UPSTREAM_REGISTRY: &str = "UnsupportedUpstreamRegistryException";

/// The sentence ECR refuses an upstream that authenticates its callers with when the
/// request carries no credential to reach it with. It names the Secrets Manager secret a
/// `credentialArn` points at, and has no variable part.
const REQUIRES_AUTHENTICATION: &str =
    "The specified upstream registry requires authentication. Specify a valid Secrets \
     Manager ARN containing the upstream registry credentials and try again.";

/// The sentence ECR refuses an `upstreamRegistryUrl` addressing no registry it caches from
/// with: the URL the request carried is its one variable part, and it carries no trailing
/// period.
fn invalid_upstream_url_message(url: &str) -> String {
    format!(
        "The upstream registry URL {url} is invalid. Specify a valid upstream registry \
         URL and try again"
    )
}

/// One row of the create table: a request to make and what the rule must report back.
struct Case<'a> {
    name: &'a str,
    prefix: &'a str,
    /// `upstreamRegistry` as the request carries it; `None` leaves the member out, so
    /// the handler must resolve it from the URL.
    upstream_registry: Option<&'a str>,
    upstream_registry_url: &'a str,
    upstream_repository_prefix: Option<&'a str>,
    credential_arn: Option<&'a str>,
    custom_role_arn: Option<&'a str>,
    expected_upstream_registry: &'a str,
    expected_upstream_repository_prefix: &'a str,
}

/// The create table: one row per value of the model's `UpstreamRegistry` enum
/// (smithy_query shape=UpstreamRegistry: ecr, ecr-public, quay, k8s, docker-hub,
/// github-container-registry, azure-container-registry, gitlab-container-registry,
/// chainguard), with the URL syntax the `upstreamRegistryUrl` member documents for it,
/// plus a row whose prefix carries the trailing slash the prefix doc allows. Rows differ
/// in which optional members the request carries, so the defaults and the echo are both
/// exercised on registries the issue never cited.
fn create_cases() -> Vec<Case<'static>> {
    vec![
        Case {
            name: "ecr-public, every optional member omitted",
            prefix: "ecr-public-cache",
            upstream_registry: None,
            upstream_registry_url: "public.ecr.aws",
            upstream_repository_prefix: None,
            credential_arn: None,
            custom_role_arn: None,
            expected_upstream_registry: "ecr-public",
            expected_upstream_repository_prefix: ROOT,
        },
        Case {
            name: "docker-hub, named registry with a credential and an upstream prefix",
            prefix: "docker-hub-cache",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            upstream_repository_prefix: Some("library"),
            credential_arn: Some(CREDENTIAL_ARN),
            custom_role_arn: None,
            expected_upstream_registry: "docker-hub",
            expected_upstream_repository_prefix: "library",
        },
        Case {
            name: "github-container-registry, registry resolved from ghcr.io",
            prefix: "ghcr-cache",
            upstream_registry: None,
            upstream_registry_url: "ghcr.io",
            upstream_repository_prefix: None,
            credential_arn: Some(OTHER_CREDENTIAL_ARN),
            custom_role_arn: Some(CUSTOM_ROLE_ARN),
            expected_upstream_registry: "github-container-registry",
            expected_upstream_repository_prefix: ROOT,
        },
        Case {
            name: "gitlab-container-registry, both ARNs carried",
            prefix: "gitlab-cache",
            upstream_registry: Some("gitlab-container-registry"),
            upstream_registry_url: "registry.gitlab.com",
            upstream_repository_prefix: Some("group/project"),
            credential_arn: Some(CREDENTIAL_ARN),
            custom_role_arn: Some(CUSTOM_ROLE_ARN),
            expected_upstream_registry: "gitlab-container-registry",
            expected_upstream_repository_prefix: "group/project",
        },
        Case {
            name: "k8s, registry resolved from registry.k8s.io",
            prefix: "k8s-cache",
            upstream_registry: None,
            upstream_registry_url: "registry.k8s.io",
            upstream_repository_prefix: Some("kube"),
            credential_arn: None,
            custom_role_arn: None,
            expected_upstream_registry: "k8s",
            expected_upstream_repository_prefix: "kube",
        },
        Case {
            name: "quay, named registry and the default upstream prefix",
            prefix: "quay-cache",
            upstream_registry: Some("quay"),
            upstream_registry_url: "quay.io",
            upstream_repository_prefix: None,
            credential_arn: Some(CREDENTIAL_ARN),
            custom_role_arn: None,
            expected_upstream_registry: "quay",
            expected_upstream_repository_prefix: ROOT,
        },
        Case {
            name: "azure-container-registry, registry resolved from <custom>.azurecr.io",
            prefix: "azure-cache",
            upstream_registry: None,
            upstream_registry_url: "contoso.azurecr.io",
            upstream_repository_prefix: None,
            credential_arn: Some(CREDENTIAL_ARN),
            custom_role_arn: None,
            expected_upstream_registry: "azure-container-registry",
            expected_upstream_repository_prefix: ROOT,
        },
        Case {
            name: "ecr, registry resolved from <accountId>.dkr.ecr.<region>.amazonaws.com",
            prefix: "ecr-cache",
            upstream_registry: None,
            upstream_registry_url: "000000000000.dkr.ecr.us-east-2.amazonaws.com",
            upstream_repository_prefix: Some("team"),
            credential_arn: None,
            custom_role_arn: Some(CUSTOM_ROLE_ARN),
            expected_upstream_registry: "ecr",
            expected_upstream_repository_prefix: "team",
        },
        Case {
            name: "chainguard, a registry the model documents no URL for",
            prefix: "chainguard-cache",
            upstream_registry: Some("chainguard"),
            upstream_registry_url: "cgr.dev",
            upstream_repository_prefix: None,
            credential_arn: Some(OTHER_CREDENTIAL_ARN),
            custom_role_arn: None,
            expected_upstream_registry: "chainguard",
            expected_upstream_repository_prefix: ROOT,
        },
        Case {
            name: "a prefix written with the trailing slash the prefix doc allows",
            prefix: "slash-cache/",
            upstream_registry: Some("quay"),
            upstream_registry_url: "quay.io",
            upstream_repository_prefix: None,
            credential_arn: None,
            custom_role_arn: None,
            expected_upstream_registry: "quay",
            expected_upstream_repository_prefix: ROOT,
        },
    ]
}

/// The `CreatePullThroughCacheRule` request one table row describes.
fn create_request(case: &Case<'_>) -> Value {
    let mut request = json!({
        "ecrRepositoryPrefix": case.prefix,
        "upstreamRegistryUrl": case.upstream_registry_url,
    });
    let members = [
        ("upstreamRegistry", case.upstream_registry),
        ("upstreamRepositoryPrefix", case.upstream_repository_prefix),
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

/// Assert that `rule` — a create response or a described rule — reports what the row says.
fn assert_rule(case: &Case<'_>, label: &str, rule: &Value) {
    let expectations = [
        ("/ecrRepositoryPrefix", json!(case.prefix)),
        ("/upstreamRegistryUrl", json!(case.upstream_registry_url)),
        ("/registryId", json!(REGISTRY_ID)),
        (
            "/upstreamRegistry",
            json!(case.expected_upstream_registry),
        ),
        (
            "/upstreamRepositoryPrefix",
            json!(case.expected_upstream_repository_prefix),
        ),
        (
            "/credentialArn",
            case.credential_arn.map_or(Value::Null, |arn| json!(arn)),
        ),
        (
            "/customRoleArn",
            case.custom_role_arn.map_or(Value::Null, |arn| json!(arn)),
        ),
    ];
    for (pointer, expected) in expectations {
        assert_eq!(
            at(rule, pointer),
            expected,
            "case {:?}: {label} {pointer} expected {expected:?}, got {:?}",
            case.name,
            at(rule, pointer)
        );
    }
    assert!(
        at(rule, "/createdAt").as_f64().is_some_and(|t| t > 0.0),
        "case {:?}: {label} createdAt must be the instant of creation, got {:?}",
        case.name,
        at(rule, "/createdAt")
    );
}

/// Every rule the registry currently holds, in the order `DescribePullThroughCacheRules`
/// reports it, read one page at a time so the token is exercised too.
async fn every_rule(
    router: &::smithy_server_runtime::MultiServiceRouter,
    case: &str,
    page: i64,
) -> Vec<Value> {
    let mut rules = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({"maxResults": page});
        if let Some(token) = &token {
            request["nextToken"] = json!(token);
        }
        let body = ok(router, case, "DescribePullThroughCacheRules", &request).await;
        let page_rules = at(&body, "/pullThroughCacheRules");
        let page_rules = page_rules.as_array().unwrap_or_else(|| {
            panic!("case {case:?}: pullThroughCacheRules must be a list, got {page_rules:?}")
        });
        assert!(
            page_rules.len() <= usize::try_from(page).expect("the page size is positive"),
            "case {case:?}: a page of at most {page} rules, got {}",
            page_rules.len()
        );
        rules.extend(page_rules.iter().cloned());
        match at(&body, "/nextToken").as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
        assert!(
            rules.len() < 1_000,
            "case {case:?}: the pagination loop is not terminating"
        );
    }
    rules
}

#[tokio::test]
async fn a_created_rule_is_what_describe_reports() {
    let router = emulator();

    for case in create_cases() {
        let created = ok(
            &router,
            case.name,
            "CreatePullThroughCacheRule",
            &create_request(&case),
        )
        .await;
        assert_rule(&case, "the create response", &created);

        // The round trip: the rule read back through the reporting operation, asked for
        // by the prefix the create was given.
        let described = ok(
            &router,
            case.name,
            "DescribePullThroughCacheRules",
            &json!({"ecrRepositoryPrefixes": [case.prefix]}),
        )
        .await;
        let rules = at(&described, "/pullThroughCacheRules");
        let rules = rules.as_array().expect("a list of rules");
        assert_eq!(
            rules.len(),
            1,
            "case {:?}: exactly the one rule created for this prefix, got {rules:?}",
            case.name
        );
        assert_rule(&case, "the described rule", &rules[0]);
        assert_eq!(
            at(&rules[0], "/createdAt"),
            at(&created, "/createdAt"),
            "case {:?}: the described createdAt is the one the create reported, got {:?}",
            case.name,
            at(&rules[0], "/createdAt")
        );
        // Nothing has changed the rule since it was created.
        assert_eq!(
            at(&rules[0], "/updatedAt"),
            at(&created, "/createdAt"),
            "case {:?}: an untouched rule was last updated when it was created, got {:?}",
            case.name,
            at(&rules[0], "/updatedAt")
        );
    }

    // And a describe that names no prefix reports every rule, exactly once each.
    let all = every_rule(&router, "every rule", 3).await;
    let listed: Vec<String> = all
        .iter()
        .map(|rule| at(rule, "/ecrRepositoryPrefix").as_str().unwrap_or("").to_string())
        .collect();
    for case in create_cases() {
        assert_eq!(
            listed.iter().filter(|prefix| *prefix == case.prefix).count(),
            1,
            "case {:?}: the prefix appears exactly once in {listed:?}",
            case.name
        );
    }
    assert_eq!(
        listed.len(),
        create_cases().len(),
        "the registry reports exactly the rules created: {listed:?}"
    );
}

#[tokio::test]
async fn a_second_rule_for_the_same_prefix_is_refused_and_changes_nothing() {
    let router = emulator();
    // The prefix doc: "There is always an assumed / applied to the end of the prefix",
    // so both spellings of one prefix are the same prefix.
    for (case, first, again) in [
        ("the same spelling", "dup-cache", "dup-cache"),
        ("a trailing slash added", "slash-dup", "slash-dup/"),
        ("a trailing slash dropped", "unslash-dup/", "unslash-dup"),
    ] {
        let created = ok(
            &router,
            case,
            "CreatePullThroughCacheRule",
            &json!({
                "ecrRepositoryPrefix": first,
                "upstreamRegistryUrl": "public.ecr.aws",
                "upstreamRegistry": "ecr-public",
            }),
        )
        .await;

        let answer = call(
            &router,
            "CreatePullThroughCacheRule",
            &json!({
                "ecrRepositoryPrefix": again,
                "upstreamRegistryUrl": "quay.io",
                "upstreamRegistry": "quay",
                "upstreamRepositoryPrefix": "other",
            }),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: a duplicate rule is refused with HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert!(
            answer
                .error_code()
                .ends_with("PullThroughCacheRuleAlreadyExistsException"),
            "case {case:?}: expected PullThroughCacheRuleAlreadyExistsException, got {:?}",
            answer.error_code()
        );

        // The refused create wrote nothing: the first rule still reports its own values.
        let described = ok(
            &router,
            case,
            "DescribePullThroughCacheRules",
            &json!({"ecrRepositoryPrefixes": [first]}),
        )
        .await;
        let rule = at(&described, "/pullThroughCacheRules/0");
        assert_eq!(
            at(&rule, "/upstreamRegistryUrl"),
            at(&created, "/upstreamRegistryUrl"),
            "case {case:?}: the stored rule is still the first one, got {rule:?}"
        );
        assert_eq!(
            at(&rule, "/upstreamRepositoryPrefix"),
            json!(ROOT),
            "case {case:?}: the refused create did not change the upstream prefix, got {rule:?}"
        );
        assert_eq!(
            at(&rule, "/createdAt"),
            at(&created, "/createdAt"),
            "case {case:?}: the stored creation instant is untouched, got {rule:?}"
        );
    }
}

#[tokio::test]
async fn an_upstream_registry_the_url_does_not_name_is_refused() {
    let router = emulator();
    // A URL outside the per-registry syntax the `upstreamRegistryUrl` member documents,
    // with no `upstreamRegistry` to say which registry it is.
    for (case, url) in [
        ("a host of nobody's", "registry.example.com"),
        ("a registry of another vendor", "mcr.microsoft.com"),
        ("another vendor's registry, written as a URL", "https://gcr.io/v2/"),
        ("an almost-ECR host", "000000000000.dkr.ecr.us-east-2.example.com"),
        ("an ECR host with no account id", "dkr.ecr.us-east-2.amazonaws.com"),
        (
            "an ECR host whose account id is short",
            "00000000000.dkr.ecr.us-east-2.amazonaws.com",
        ),
        (
            "an ECR host with a label before the account id",
            "extra.000000000000.dkr.ecr.us-east-2.amazonaws.com",
        ),
        ("an almost-Azure host", "azurecr.io.example.com"),
        ("an Azure host with no label of its own", ".azurecr.io"),
        ("the empty string", ""),
    ] {
        let body = refused(
            &router,
            case,
            "CreatePullThroughCacheRule",
            &json!({
                "ecrRepositoryPrefix": "unsupported-cache",
                "upstreamRegistryUrl": url,
            }),
            UNSUPPORTED_UPSTREAM_REGISTRY,
        )
        .await;
        // The refusal is the error class's own sentence, carrying the URL from the
        // request as its one variable part.
        assert_eq!(
            at(&body, "/message"),
            json!(invalid_upstream_url_message(url)),
            "case {case:?}: expected the registry's invalid-URL wording, got {body:?}"
        );
    }

    // None of those refusals persisted a rule.
    let rules = every_rule(&router, "after the refusals", 10).await;
    assert!(
        rules.is_empty(),
        "a refused create persists nothing, got {rules:?}"
    );

    // The same prefix with a documented URL is accepted, so the refusal was about the
    // URL and nothing else.
    let created = ok(
        &router,
        "a documented URL",
        "CreatePullThroughCacheRule",
        &json!({
            "ecrRepositoryPrefix": "unsupported-cache",
            "upstreamRegistryUrl": "public.ecr.aws",
        }),
    )
    .await;
    assert_eq!(
        at(&created, "/upstreamRegistry"),
        json!("ecr-public"),
        "the registry is resolved from the documented URL, got {created:?}"
    );
}

#[tokio::test]
async fn describe_pages_through_the_rules_in_prefix_order() {
    let router = emulator();
    let prefixes = [
        "page-alpha",
        "page-beta",
        "page-delta",
        "page-gamma",
        "page-omega",
    ];
    for prefix in prefixes {
        ok(
            &router,
            prefix,
            "CreatePullThroughCacheRule",
            &json!({
                "ecrRepositoryPrefix": prefix,
                "upstreamRegistryUrl": "public.ecr.aws",
            }),
        )
        .await;
    }

    // Whatever the page size, the sequence of rules is the same and complete.
    for page in [1_i64, 2, 5, 7] {
        let case = format!("maxResults {page}");
        let listed: Vec<String> = every_rule(&router, &case, page)
            .await
            .iter()
            .map(|rule| {
                at(rule, "/ecrRepositoryPrefix")
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            listed,
            prefixes.map(str::to_string).to_vec(),
            "case {case:?}: every rule once, in prefix order, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn describe_refuses_a_prefix_no_rule_was_created_for() {
    let router = emulator();
    ok(
        &router,
        "the one rule",
        "CreatePullThroughCacheRule",
        &json!({
            "ecrRepositoryPrefix": "known-cache",
            "upstreamRegistryUrl": "public.ecr.aws",
        }),
    )
    .await;

    for (case, prefixes) in [
        ("one unknown prefix", json!(["unknown-cache"])),
        ("a known one beside an unknown one", json!(["known-cache", "unknown-cache"])),
        ("an unknown one first", json!(["unknown-cache", "known-cache"])),
    ] {
        let answer = call(
            &router,
            "DescribePullThroughCacheRules",
            &json!({"ecrRepositoryPrefixes": prefixes}),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: an unregistered prefix is refused with HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert!(
            answer
                .error_code()
                .ends_with("PullThroughCacheRuleNotFoundException"),
            "case {case:?}: expected PullThroughCacheRuleNotFoundException, got {:?}",
            answer.error_code()
        );
    }

    // The known prefix on its own still answers, either spelling, and naming it twice
    // reports it once.
    for prefixes in [
        json!(["known-cache"]),
        json!(["known-cache/"]),
        json!(["known-cache", "known-cache/"]),
    ] {
        let case = prefixes.to_string();
        let described = ok(
            &router,
            &case,
            "DescribePullThroughCacheRules",
            &json!({"ecrRepositoryPrefixes": prefixes}),
        )
        .await;
        let rules = at(&described, "/pullThroughCacheRules");
        let rules = rules.as_array().expect("a list of rules");
        assert_eq!(
            rules.len(),
            1,
            "case {case:?}: the one rule, reported once, got {rules:?}"
        );
        assert_eq!(
            at(&rules[0], "/ecrRepositoryPrefix"),
            json!("known-cache"),
            "case {case:?}: the rule is found by either spelling of its prefix, got {described:?}"
        );
    }
}

#[tokio::test]
async fn another_accounts_registry_is_refused_by_both_operations() {
    let router = emulator();
    let other = "123456789012";
    for (case, operation, body) in [
        (
            "create for another registry",
            "CreatePullThroughCacheRule",
            json!({
                "ecrRepositoryPrefix": "other-cache",
                "upstreamRegistryUrl": "public.ecr.aws",
                "registryId": other,
            }),
        ),
        (
            "describe of another registry",
            "DescribePullThroughCacheRules",
            json!({"registryId": other}),
        ),
    ] {
        // Another account's registry shares nothing with this caller, and a rule names no
        // repository, so the refusal names the wildcard resource.
        crate::wire::refused_cross_account(&router, case, operation, &body, other, None).await;
    }

    // This registry's own id is accepted, and the refusals persisted nothing.
    ok(
        &router,
        "this registry named explicitly",
        "CreatePullThroughCacheRule",
        &json!({
            "ecrRepositoryPrefix": "own-cache",
            "upstreamRegistryUrl": "public.ecr.aws",
            "registryId": REGISTRY_ID,
        }),
    )
    .await;
    let listed: Vec<String> = every_rule(&router, "after the refusals", 10)
        .await
        .iter()
        .map(|rule| {
            at(rule, "/ecrRepositoryPrefix")
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        listed,
        vec!["own-cache".to_string()],
        "only the accepted create persisted a rule, got {listed:?}"
    );
}

/// One row of the credential-requirement table: the upstream the create request names and
/// the credentials it carries, and whether a rule is created for it.
struct CredentialCase<'a> {
    name: &'a str,
    /// `upstreamRegistry` as the request carries it; `None` leaves the member out, so the
    /// upstream is the one the URL resolves to.
    upstream_registry: Option<&'a str>,
    upstream_registry_url: &'a str,
    credential_arn: Option<&'a str>,
    custom_role_arn: Option<&'a str>,
    /// Whether the create is accepted.
    accepted: bool,
}

/// The credential-requirement table. One row per value of the model's `UpstreamRegistry`
/// enum (smithy_query shape=UpstreamRegistry: ecr, ecr-public, quay, k8s, docker-hub,
/// github-container-registry, azure-container-registry, gitlab-container-registry,
/// chainguard) carrying NO credential, so each row's verdict is its upstream's own
/// requirement, plus rows differing in which credential member the request carries: a
/// Secrets Manager secret, the IAM role an Amazon ECR upstream is reached through, an
/// empty role (a legal `customRoleArn` that names none) and both at once. Which upstreams
/// ECR caches from without authenticating is the ECR user guide's supported-upstream
/// table: Amazon ECR Public, Kubernetes and Quay.
fn credential_cases() -> Vec<CredentialCase<'static>> {
    vec![
        CredentialCase {
            name: "ecr-public is cached from anonymously",
            upstream_registry: Some("ecr-public"),
            upstream_registry_url: "public.ecr.aws",
            credential_arn: None,
            custom_role_arn: None,
            accepted: true,
        },
        CredentialCase {
            name: "k8s is cached from anonymously",
            upstream_registry: Some("k8s"),
            upstream_registry_url: "registry.k8s.io",
            credential_arn: None,
            custom_role_arn: None,
            accepted: true,
        },
        CredentialCase {
            name: "quay is cached from anonymously",
            upstream_registry: Some("quay"),
            upstream_registry_url: "quay.io",
            credential_arn: None,
            custom_role_arn: None,
            accepted: true,
        },
        CredentialCase {
            name: "docker-hub authenticates its callers and no credential is carried",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            credential_arn: None,
            custom_role_arn: None,
            accepted: false,
        },
        CredentialCase {
            name: "github-container-registry authenticates and no credential is carried",
            upstream_registry: Some("github-container-registry"),
            upstream_registry_url: "ghcr.io",
            credential_arn: None,
            custom_role_arn: None,
            accepted: false,
        },
        CredentialCase {
            name: "gitlab-container-registry authenticates and no credential is carried",
            upstream_registry: Some("gitlab-container-registry"),
            upstream_registry_url: "registry.gitlab.com",
            credential_arn: None,
            custom_role_arn: None,
            accepted: false,
        },
        CredentialCase {
            name: "azure-container-registry authenticates and no credential is carried",
            upstream_registry: Some("azure-container-registry"),
            upstream_registry_url: "contoso.azurecr.io",
            credential_arn: None,
            custom_role_arn: None,
            accepted: false,
        },
        CredentialCase {
            name: "an ecr upstream authenticates and no credential is carried",
            upstream_registry: Some("ecr"),
            upstream_registry_url: "000000000000.dkr.ecr.us-east-2.amazonaws.com",
            credential_arn: None,
            custom_role_arn: None,
            accepted: false,
        },
        CredentialCase {
            name: "chainguard authenticates and no credential is carried",
            upstream_registry: Some("chainguard"),
            upstream_registry_url: "cgr.dev",
            credential_arn: None,
            custom_role_arn: None,
            accepted: false,
        },
        CredentialCase {
            name: "the requirement follows the upstream the URL alone resolves to",
            upstream_registry: None,
            upstream_registry_url: "ghcr.io",
            credential_arn: None,
            custom_role_arn: None,
            accepted: false,
        },
        CredentialCase {
            name: "an empty customRoleArn names no role, so it is no credential",
            upstream_registry: Some("gitlab-container-registry"),
            upstream_registry_url: "registry.gitlab.com",
            credential_arn: None,
            custom_role_arn: Some(""),
            accepted: false,
        },
        CredentialCase {
            name: "docker-hub with the secret its authentication is read from",
            upstream_registry: Some("docker-hub"),
            upstream_registry_url: "registry-1.docker.io",
            credential_arn: Some(CREDENTIAL_ARN),
            custom_role_arn: None,
            accepted: true,
        },
        CredentialCase {
            name: "an ecr upstream reached through the role ECR assumes",
            upstream_registry: None,
            upstream_registry_url: "000000000000.dkr.ecr.us-east-2.amazonaws.com",
            credential_arn: None,
            custom_role_arn: Some(CUSTOM_ROLE_ARN),
            accepted: true,
        },
        CredentialCase {
            name: "azure-container-registry with both credentials",
            upstream_registry: None,
            upstream_registry_url: "contoso.azurecr.io",
            credential_arn: Some(OTHER_CREDENTIAL_ARN),
            custom_role_arn: Some(CUSTOM_ROLE_ARN),
            accepted: true,
        },
        CredentialCase {
            name: "an anonymous upstream with a credential it does not need",
            upstream_registry: Some("quay"),
            upstream_registry_url: "quay.io",
            credential_arn: Some(CREDENTIAL_ARN),
            custom_role_arn: None,
            accepted: true,
        },
    ]
}

/// A rule is created only when it could authenticate to the upstream it caches from: an
/// upstream that authenticates its callers needs a credential on the request, and one ECR
/// caches from anonymously needs none.
#[tokio::test]
async fn an_upstream_that_authenticates_its_callers_needs_a_credential_to_cache_from() {
    let router = emulator();

    for (index, case) in credential_cases().iter().enumerate() {
        // Each row under its own prefix, so no row reads or blocks another's rule.
        let prefix = format!("cred-cache-{index}");
        let mut request = json!({
            "ecrRepositoryPrefix": prefix.clone(),
            "upstreamRegistryUrl": case.upstream_registry_url,
        });
        for (member, value) in [
            ("upstreamRegistry", case.upstream_registry),
            ("credentialArn", case.credential_arn),
            ("customRoleArn", case.custom_role_arn),
        ] {
            if let Some(value) = value {
                request[member] = json!(value);
            }
        }

        if case.accepted {
            let created = ok(&router, case.name, "CreatePullThroughCacheRule", &request).await;
            assert_eq!(
                at(&created, "/upstreamRegistryUrl"),
                json!(case.upstream_registry_url),
                "case {:?}: the created rule caches from the URL the request named, got {created:?}",
                case.name
            );
            // The round trip: the rule the create stored is the one the reporting
            // operation reports.
            let described = ok(
                &router,
                case.name,
                "DescribePullThroughCacheRules",
                &json!({"ecrRepositoryPrefixes": [prefix.clone()]}),
            )
            .await;
            assert_eq!(
                at(&described, "/pullThroughCacheRules/0/upstreamRegistryUrl"),
                json!(case.upstream_registry_url),
                "case {:?}: the registry reports the rule's own upstream URL, got {described:?}",
                case.name
            );
            continue;
        }

        let body = refused(
            &router,
            case.name,
            "CreatePullThroughCacheRule",
            &request,
            UNSUPPORTED_UPSTREAM_REGISTRY,
        )
        .await;
        assert_eq!(
            at(&body, "/message"),
            json!(REQUIRES_AUTHENTICATION),
            "case {:?}: expected the registry's requires-authentication wording, got {body:?}",
            case.name
        );
        // The refusal happened before the store was touched: the prefix holds no rule.
        let answer = call(
            &router,
            "DescribePullThroughCacheRules",
            &json!({"ecrRepositoryPrefixes": [prefix.clone()]}),
        )
        .await;
        assert_eq!(
            answer.error_code(),
            "PullThroughCacheRuleNotFoundException",
            "case {:?}: a refused create persists nothing, got {} with {:?}",
            case.name,
            answer.status,
            answer.body
        );
    }
}

/// A refused create leaves the prefix free: the next create for the same prefix is the one
/// the registry holds, and reports the upstream URL IT was given.
#[tokio::test]
async fn a_prefix_a_refused_create_left_free_takes_the_next_rule() {
    let router = emulator();
    let prefix = "reclaimed-cache";

    let body = refused(
        &router,
        "an authenticating upstream with no credential",
        "CreatePullThroughCacheRule",
        &json!({
            "ecrRepositoryPrefix": prefix,
            "upstreamRegistryUrl": "registry-1.docker.io",
        }),
        UNSUPPORTED_UPSTREAM_REGISTRY,
    )
    .await;
    assert_eq!(
        at(&body, "/message"),
        json!(REQUIRES_AUTHENTICATION),
        "expected the registry's requires-authentication wording, got {body:?}"
    );

    let created = ok(
        &router,
        "the public ECR upstream",
        "CreatePullThroughCacheRule",
        &json!({
            "ecrRepositoryPrefix": prefix,
            "upstreamRegistryUrl": "public.ecr.aws",
        }),
    )
    .await;
    for (pointer, expected) in [
        ("/ecrRepositoryPrefix", json!(prefix)),
        ("/upstreamRegistryUrl", json!("public.ecr.aws")),
        ("/registryId", json!(REGISTRY_ID)),
        ("/upstreamRegistry", json!("ecr-public")),
    ] {
        assert_eq!(
            at(&created, pointer),
            expected,
            "the create answers {pointer} {expected:?}, got {:?}",
            at(&created, pointer)
        );
    }
    assert!(
        at(&created, "/createdAt").as_f64().is_some_and(|t| t > 0.0),
        "the create reports the instant the rule was created, got {:?}",
        at(&created, "/createdAt")
    );

    let described = ok(
        &router,
        "the rule read back",
        "DescribePullThroughCacheRules",
        &json!({"ecrRepositoryPrefixes": [prefix]}),
    )
    .await;
    let rule = at(&described, "/pullThroughCacheRules/0");
    // Every member the create answered is the one the reporting operation reports: the
    // record is one, so the two cannot disagree.
    for (member, value) in created.as_object().into_iter().flatten() {
        assert_eq!(
            &rule[member], value,
            "the registry reports the rule the create stored: {member} came back {:?}, \
             got {rule:?}",
            rule[member]
        );
    }
}
