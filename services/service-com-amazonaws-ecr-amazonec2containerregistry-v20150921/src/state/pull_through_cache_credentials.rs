// SPDX-License-Identifier: CC0-1.0

//! The credential a pull through cache rule authenticates to its upstream registry with,
//! and the verdict `ValidatePullThroughCacheRule` reports about a stored rule.
//!
//! Three questions live here, and every operation that writes or reports a rule answers
//! them through this one module, so no two of them can disagree:
//!
//! 1. does the rule's upstream authenticate its callers at all
//!    ([`upstream_needs_credential`]), and does the rule store something to authenticate
//!    with ([`stores_upstream_credential`]);
//! 2. is the value it stores the kind of ARN its member documents — a Secrets Manager
//!    `ecr-pullthroughcache/` secret for `credentialArn`
//!    ([`is_pull_through_cache_credential_arn`]), an IAM role for `customRoleArn`
//!    ([`is_iam_role_arn`]);
//! 3. can the secret a `credentialArn` names be READ — "Amazon ECR will attempt to
//!    retrieve the contents of the Secrets Manager secret, verify the syntax" — which is
//!    Secrets Manager's fact to state, so it is asked of that service's OWN store
//!    ([`credential_secret_defect`]), the way this crate's signing jobs ask AWS Signer
//!    about a signing profile.
//!
//! Nothing here depends on reaching the upstream registry: this emulator opens no outbound
//! connection, and that is a property of the emulator rather than of the rule, so it is
//! never reported as a rule's verdict.

use super::*;

/// The wording ECR uses when a rule would cache from an upstream that authenticates its
/// callers while storing no credential to authenticate with — the other state
/// `UnsupportedUpstreamRegistryException` reports. It names the Secrets Manager secret a
/// `credentialArn` points at, and has no variable part.
pub const UPSTREAM_REQUIRES_AUTHENTICATION_MESSAGE: &str =
    "The specified upstream registry requires authentication. Specify a valid Secrets \
     Manager ARN containing the upstream registry credentials and try again.";

/// Whether the pair of credential references a pull through cache rule would store leaves
/// it with something to authenticate to its upstream with: a `credentialArn` naming the
/// Secrets Manager secret the upstream's credentials live in, or the `customRoleArn` ECR
/// assumes to reach an upstream registry of its own.
///
/// An empty string stores neither: `customRoleArn` is modelled `@length(0..2048)`, so the
/// empty string is a legal value that names no role, which is the same reading
/// [`pull_through_cache_rule_failure`] takes of a stored record. A value of nothing but
/// whitespace names no role either — no ARN is spelled out of spaces — so the two are read
/// the same way, which is what lets an incoming request and a stored record be judged by
/// this one function.
pub fn stores_upstream_credential(
    credential_arn: Option<&str>,
    custom_role_arn: Option<&str>,
) -> bool {
    [credential_arn, custom_role_arn]
        .into_iter()
        .flatten()
        .any(|arn| !arn.trim().is_empty())
}

/// The upstream registries ECR caches from WITHOUT a credential on the rule: the entries
/// of the ECR user guide's supported-upstream table that require no authentication —
/// Amazon ECR Public, Kubernetes and Quay, the public registries whose images are pulled
/// anonymously, and the upstreams pull through cache rules were first served for. Every
/// other `UpstreamRegistry` value (listed in `UPSTREAM_REGISTRY_VALUES`) authenticates
/// its callers, which is why a rule naming one carries the credential it is reached with.
///
/// Only the `UpstreamRegistry` value is named here: the host each registry is reached at
/// is [`DOCUMENTED_UPSTREAM_REGISTRY_URLS`]'s, the one table of per-registry URLs, so a
/// rule that names no `upstreamRegistry` is judged by resolving its URL through that same
/// table and the two readings cannot drift apart.
pub const ANONYMOUS_UPSTREAM_REGISTRIES: [&str; 3] = ["ecr-public", "k8s", "quay"];

/// Whether an upstream registry a pull through cache rule points at authenticates its
/// callers, and therefore needs a credential stored on the rule.
///
/// Every upstream registry ECR supports except those in
/// [`ANONYMOUS_UPSTREAM_REGISTRIES`] requires credentials. A rule that names no upstream
/// registry is judged by the registry its URL resolves to
/// ([`upstream_registry_for_url`]), and a URL naming no supported registry at all is
/// treated as one that authenticates: nothing says it serves anonymously.
pub fn upstream_needs_credential(upstream_registry: Option<&str>, upstream_url: &str) -> bool {
    let registry = match upstream_registry {
        Some(registry) => Some(registry),
        None => upstream_registry_for_url(upstream_registry_host(upstream_url)),
    };
    registry.is_none_or(|registry| !ANONYMOUS_UPSTREAM_REGISTRIES.contains(&registry))
}

/// The HOST a rule's `upstreamRegistryUrl` addresses: what is left after an optional
/// scheme, before the path, without the userinfo or the port — so the URL a client wrote
/// with any of those is read as the one registry host it points at (`public.ecr.aws:443`
/// addresses the same registry as `public.ecr.aws`). The case is left as written, because
/// every reader of a host here compares it case-insensitively.
///
/// The reading is [`::service_common::data_plane_addressing`]'s, the tree's one reader of
/// a URL's authority and of a host's name, so this service and every other agree on what
/// host a URL names.
pub(super) fn upstream_registry_host(upstream_url: &str) -> &str {
    use ::service_common::data_plane_addressing::{host_only, url_authority};

    let authority = url_authority(upstream_url.trim());
    host_only(authority.split('/').next().unwrap_or_default())
}

/// The Secrets Manager secret name prefix a pull through cache rule's credential is
/// stored under. `CredentialArn`'s own modelled pattern states it —
/// `^arn:aws(-\w+)*:secretsmanager:[a-zA-Z0-9-:]+:secret:ecr\-pullthroughcache\/[a-zA-Z0-9\/_+=.@-]+$`
/// — so a value naming anything else names no secret ECR reads a rule's upstream
/// credential from.
pub const CREDENTIAL_SECRET_NAME_PREFIX: &str = "ecr-pullthroughcache/";

/// The ARN vendor (service namespace) field of a `credentialArn`: the secret a rule's
/// credential lives in is an Amazon Web Services Secrets Manager secret.
pub(super) const SECRETS_MANAGER_VENDOR: &str = "secretsmanager";

/// The resource field prefix of a Secrets Manager secret ARN (`…:secret:<name>`).
pub(super) const SECRET_RESOURCE_PREFIX: &str = "secret:";

/// Whether `value` is the ARN of the Secrets Manager secret a pull through cache rule's
/// `credentialArn` names, as that member's own modelled pattern spells one:
/// `^arn:aws(-\w+)*:secretsmanager:[a-zA-Z0-9-:]+:secret:ecr\-pullthroughcache\/[a-zA-Z0-9\/_+=.@-]+$`.
///
/// The six fields come from [`::service_common::arn::parse_of`], the tree's ONE ARN parse,
/// which also settles that the value is an ARN of the `secretsmanager` vendor at all. What
/// is checked here is the rest of that pattern, field by field: the partition is `aws`
/// optionally followed by `-`-separated word labels, the region-and-account span is the
/// `[a-zA-Z0-9-:]+` the pattern states, and the secret name after
/// [`CREDENTIAL_SECRET_NAME_PREFIX`] is non-empty and drawn from the pattern's own
/// character class — so a value this accepts is one the member accepts.
pub fn is_pull_through_cache_credential_arn(value: &str) -> bool {
    let Some(arn) = ::service_common::arn::parse_of(value, SECRETS_MANAGER_VENDOR) else {
        return false;
    };
    let Some(name) = arn
        .resource
        .strip_prefix(SECRET_RESOURCE_PREFIX)
        .and_then(|name| name.strip_prefix(CREDENTIAL_SECRET_NAME_PREFIX))
    else {
        return false;
    };
    is_aws_partition(arn.partition)
        // `[a-zA-Z0-9-:]+` spans the region and account fields together, so each is read
        // under that class and the pair must carry something.
        && (!arn.region.is_empty() || !arn.account.is_empty())
        && [arn.region, arn.account]
            .iter()
            .all(|field| field.bytes().all(is_region_or_account_byte))
        && !name.is_empty()
        && name.bytes().all(is_secret_name_byte)
}

/// Whether `partition` is the `aws(-\w+)*` partition label `CredentialArn`'s pattern
/// states: `aws`, optionally followed by `-`-separated word labels (`aws-cn`,
/// `aws-us-gov`).
pub(super) fn is_aws_partition(partition: &str) -> bool {
    let Some(labels) = partition.strip_prefix("aws") else {
        return false;
    };
    if labels.is_empty() {
        return true;
    }
    // `(-\w+)*`: every label after the first is introduced by a hyphen and is a non-empty
    // run of word characters.
    labels.starts_with('-')
        && labels.split('-').skip(1).all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

/// A byte of the `[a-zA-Z0-9-:]+` span `CredentialArn`'s pattern allows between the
/// service namespace and the `secret:` resource.
///
/// The `:` of that class is what lets the span cover the Region and the account TOGETHER
/// (so a secret ARN carrying no Region still matches); the two fields are read one at a
/// time here, each already split on `:`, so only the rest of the class applies to a field.
pub(super) fn is_region_or_account_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

/// A byte of the `[a-zA-Z0-9\/_+=.@-]+` secret name `CredentialArn`'s pattern allows
/// after the `ecr-pullthroughcache/` namespace.
pub(super) fn is_secret_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"/_+=.@-".contains(&byte)
}

/// Whether `value` names an IAM role of this account, which is what a rule's
/// `customRoleArn` carries ("The ARN of the IAM role associated with the pull through
/// cache rule").
///
/// The reading is [`::service_common::iam_role::role_name`], the tree's one IAM role-ARN
/// reader — the `iam` vendor, the empty Region a partition-global service's ARN carries,
/// this account, and a `role/…` resource with a name — so this service and every other
/// service that resolves a role ARN agree on what one is.
pub fn is_iam_role_arn(value: &str) -> bool {
    ::service_common::iam_role::role_name(value).is_some()
}

/// The failure code of a rule whose `credentialArn` names a secret whose contents could
/// not be retrieved: no secret of this account answers to that ARN, or the one that does is
/// on its way out and no retrieval of its value succeeds.
///
/// This is the code the recorded corpus carries for a rule whose credential secret had been
/// deleted. `failure` is modelled as a free `PTCValidateFailure` string rather than an enum,
/// so the codes below are named for the defect they state in the same vocabulary.
pub const FAILURE_UNABLE_TO_ACCESS_SECRET: &str = "UNABLE_TO_ACCESS_SECRET";

/// The failure code of a rule whose credential secret can be read but does not store an
/// upstream credential: its value does not state the fields a pull through cache
/// credential is written as ([`CREDENTIAL_SECRET_FIELDS`]), which is the "verify the
/// syntax" step of validation.
pub const FAILURE_INVALID_SECRET_FORMAT: &str = "INVALID_SECRET_FORMAT";

/// The failure code of a rule that names no upstream registry to cache from.
pub const FAILURE_INVALID_UPSTREAM_REGISTRY_URL: &str = "INVALID_UPSTREAM_REGISTRY_URL";

/// The failure code of a rule whose `credentialArn` is not the ARN of the Secrets Manager
/// secret that member documents.
pub const FAILURE_INVALID_CREDENTIAL_ARN: &str = "INVALID_CREDENTIAL_ARN";

/// The failure code of a rule whose `customRoleArn` is not the ARN of an IAM role.
pub const FAILURE_INVALID_CUSTOM_ROLE_ARN: &str = "INVALID_CUSTOM_ROLE_ARN";

/// The failure code of a rule whose upstream authenticates its callers while the rule
/// stores no credential at all.
pub const FAILURE_UPSTREAM_REQUIRES_AUTHENTICATION: &str = "UPSTREAM_REQUIRES_AUTHENTICATION";

/// The fields the Secrets Manager secret a `credentialArn` names stores the upstream
/// registry's credential in, as the Amazon ECR user guide writes that secret's value:
/// `{"username":"<user>","accessToken":"<token>"}`.
///
/// Only the NAMES are ever asked for, of the store that owns the value
/// (`secrets::Secret::stated_field_names`): what a field holds stays in Secrets Manager.
pub const CREDENTIAL_SECRET_FIELDS: [&str; 2] = ["username", "accessToken"];

/// The verdict `ValidatePullThroughCacheRule` reports about a rule that does not validate:
/// the code naming the defect and the sentence stating it.
///
/// It reaches the wire as the `failure` member, which is a free string the service writes a
/// failure DOCUMENT into ([`PullThroughCacheRuleFailure::document`]). The two members of
/// that document are the ones this service words every failure it reports with — a
/// `failureCode` a client branches on and a `failureReason` stating it (`ImageSigningStatus`,
/// `ImageReplicationStatus`, `RepositoryScanningConfigurationFailure`).
#[derive(Debug, Clone)]
pub struct PullThroughCacheRuleFailure {
    /// The code naming the defect: one of the `FAILURE_*` constants above.
    pub code: &'static str,
    /// The sentence stating it, built from the stored rule and the request.
    pub reason: String,
}

impl PullThroughCacheRuleFailure {
    /// The failure `code` names, stating `reason`.
    #[must_use]
    pub fn new(code: &'static str, reason: String) -> Self {
        Self { code, reason }
    }

    /// The failure document the `failure` member carries: the code and the reason, as the
    /// JSON object a client parses them out of.
    #[must_use]
    pub fn document(&self) -> String {
        ::serde_json::json!({"failureCode": self.code, "failureReason": self.reason}).to_string()
    }
}

/// Why the Secrets Manager secret a rule's `credentialArn` names holds no upstream
/// credential this registry could authenticate with.
#[derive(Debug, Clone)]
pub enum CredentialSecretDefect {
    /// No secret of this account in the Region the ARN names answers to it.
    NotFound,
    /// A secret answers to it, but a `DeleteSecret` has scheduled it for deletion, so no
    /// retrieval of its value succeeds.
    Inaccessible,
    /// Its current value does not state every field of [`CREDENTIAL_SECRET_FIELDS`], so it
    /// stores no upstream credential whatever else it holds.
    MissingFields,
}

impl CredentialSecretDefect {
    /// The code a rule carrying this defect reports as its validation failure.
    #[must_use]
    pub fn failure_code(&self) -> &'static str {
        match self {
            Self::NotFound | Self::Inaccessible => FAILURE_UNABLE_TO_ACCESS_SECRET,
            Self::MissingFields => FAILURE_INVALID_SECRET_FORMAT,
        }
    }

    /// The sentence stating this defect of the secret `credential_arn` names.
    #[must_use]
    pub fn reason(&self, credential_arn: &str) -> String {
        match self {
            Self::NotFound => format!(
                "the credentialArn '{credential_arn}' names no Amazon Web Services Secrets \
                 Manager secret, so the upstream registry credential cannot be retrieved"
            ),
            Self::Inaccessible => format!(
                "the Amazon Web Services Secrets Manager secret the credentialArn \
                 '{credential_arn}' names is scheduled for deletion, so the upstream \
                 registry credential cannot be retrieved"
            ),
            Self::MissingFields => format!(
                "the Amazon Web Services Secrets Manager secret the credentialArn \
                 '{credential_arn}' names does not store the {} fields an upstream registry \
                 credential is written as",
                CREDENTIAL_SECRET_FIELDS.join(" and ")
            ),
        }
    }

    /// This defect as the verdict `ValidatePullThroughCacheRule` reports for the rule.
    #[must_use]
    pub fn failure(&self, credential_arn: &str) -> PullThroughCacheRuleFailure {
        PullThroughCacheRuleFailure::new(self.failure_code(), self.reason(credential_arn))
    }
}

/// Why the Secrets Manager secret `credential_arn` names holds no upstream credential, or
/// `None` when it holds one.
///
/// Whether a secret exists, is still live and states a field is SECRETS MANAGER's fact, so
/// it is asked of that service's OWN store accessor — the way this crate's signing jobs ask
/// AWS Signer about a signing profile — and never of a second copy kept here. The secret is
/// looked up in the Region its own ARN names, because that is the namespace a secret is
/// identified in: the same name in another Region is another secret.
///
/// Nothing is refused on evidence that is not there: a Secrets Manager store this emulator
/// cannot read at all (an engine that is not the in-memory one) answers nothing, and neither
/// does a value sealed under a customer managed key, which only a KMS call opens
/// (`secrets::Secret::stated_field_names`).
///
/// Call it with NO lock of this service's store held: it takes Secrets Manager's, and this
/// crate never holds two services' locks at once.
pub fn credential_secret_defect<Eng: Send + Sync + 'static>(
    engine: &Eng,
    credential_arn: &str,
) -> Option<CredentialSecretDefect> {
    let state = ::service_com_amazonaws_secretsmanager_secretsmanager::state::service_state(
        engine,
    )?;
    let region = ::service_common::arn::parse_of(credential_arn, SECRETS_MANAGER_VENDOR)
        .map(|arn| arn.region.to_string())
        .unwrap_or_default();
    let store = state.lock();
    let Some(secret) = store.get(&region, credential_arn) else {
        return Some(CredentialSecretDefect::NotFound);
    };
    if secret.scheduled_for_deletion() {
        return Some(CredentialSecretDefect::Inaccessible);
    }
    let stated = secret.stated_field_names()?;
    let stores_credential = CREDENTIAL_SECRET_FIELDS
        .iter()
        .all(|field| stated.iter().any(|stated| stated == field));
    (!stores_credential).then_some(CredentialSecretDefect::MissingFields)
}

/// The verdict on the stored pull through cache `rule`: the failure
/// `ValidatePullThroughCacheRule` reports, or `None` for a rule that validates.
///
/// "Validates an existing pull through cache rule for an upstream registry that requires
/// authentication … Amazon ECR will attempt to retrieve the contents of the Secrets Manager
/// secret, verify the syntax, and then validate that authentication to the upstream registry
/// is successful" — so the verdict is the rule's own configuration plus the state of what it
/// POINTS AT, in this order:
///
/// 1. the shape of the stored record ([`pull_through_cache_rule_failure`]): the upstream it
///    caches from, the kind of ARN each credential member carries, and whether an upstream
///    that authenticates its callers was given a credential at all;
/// 2. for a rule whose upstream authenticates its callers, whether the credential it stores
///    can really be used: a `credentialArn` whose Secrets Manager secret holds no retrievable
///    credential ([`credential_secret_defect`]) leaves the rule with nothing to authenticate
///    with, which is the state the recorded corpus reports
///    [`FAILURE_UNABLE_TO_ACCESS_SECRET`] for. A valid `customRoleArn` is a credential of its
///    own — "the IAM role to be assumed by Amazon ECR to authenticate to the ECR upstream
///    registry" — so a rule that carries one is authenticated by it and its secret is not
///    what validation turns on.
///
/// Call it with NO lock of this service's store held (see [`credential_secret_defect`]).
pub fn pull_through_cache_rule_verdict<Eng: Send + Sync + 'static>(
    engine: &Eng,
    rule: &PullThroughCacheRule,
) -> Option<PullThroughCacheRuleFailure> {
    if let Some(failure) = pull_through_cache_rule_failure(rule) {
        return Some(failure);
    }
    let url = rule.upstream_registry_url.as_deref().unwrap_or_default();
    if !upstream_needs_credential(rule.upstream_registry.as_deref(), url) {
        return None;
    }
    // A role the rule carries is already known to be an IAM role ARN (step 1), so it is a
    // credential: the upstream is reached by assuming it.
    if rule
        .custom_role_arn
        .as_deref()
        .is_some_and(|role| !role.trim().is_empty())
    {
        return None;
    }
    let credential = rule
        .credential_arn
        .as_deref()
        .filter(|arn| !arn.trim().is_empty())?;
    credential_secret_defect(engine, credential).map(|defect| defect.failure(credential))
}

/// The failure the SHAPE of the stored `rule` earns, or `None` when its shape is complete.
///
/// The part of a rule's verdict that is read off the record alone:
///
/// * a rule has to name the upstream it caches from;
/// * a credential the rule carries has to be the kind of ARN its member documents, which is
///   the "verify the syntax" step: a `credentialArn` outside the
///   [`CREDENTIAL_SECRET_NAME_PREFIX`] secret namespace, or a `customRoleArn` that is no IAM
///   role, names nothing the upstream could be authenticated with;
/// * an upstream that authenticates its callers needs a credential on the rule — a
///   `credentialArn` or a `customRoleArn` — because without one authentication cannot even
///   be attempted, while an upstream ECR documents as serving anonymously
///   ([`ANONYMOUS_UPSTREAM_REGISTRIES`]) needs none.
///
/// An empty `customRoleArn` is no credential: the member is modelled `@length(0..2048)`,
/// so the empty string is a legal value that stores no role.
///
/// Whether what a well-shaped `credentialArn` POINTS AT can be retrieved is the other half
/// of the verdict, and takes another service's store: see
/// [`pull_through_cache_rule_verdict`], which is what the handler calls.
pub fn pull_through_cache_rule_failure(
    rule: &PullThroughCacheRule,
) -> Option<PullThroughCacheRuleFailure> {
    let prefix = rule.ecr_repository_prefix.as_deref().unwrap_or_default();
    let url = rule.upstream_registry_url.as_deref().unwrap_or_default();
    if let Some(failure) = pull_through_cache_upstream_failure(rule) {
        return Some(failure);
    }

    let credential = rule.credential_arn.as_deref();
    let role = rule.custom_role_arn.as_deref();
    if !stores_upstream_credential(credential, role)
        && upstream_needs_credential(rule.upstream_registry.as_deref(), url)
    {
        return Some(PullThroughCacheRuleFailure::new(
            FAILURE_UPSTREAM_REQUIRES_AUTHENTICATION,
            format!(
                "the upstream registry '{}' requires authentication and the pull through \
                 cache rule for '{prefix}' stores neither a credentialArn nor a customRoleArn",
                rule.upstream_registry.as_deref().unwrap_or(url)
            ),
        ));
    }

    None
}

/// The reason an image cannot be pulled THROUGH the stored `rule` at all, or `None` when it
/// can: the part of a rule's verdict that decides whether the upstream named on it is
/// reachable, which is what an on-demand import reports
/// `UnableToGetUpstreamImageException` for.
///
/// Two things make a rule one no image can be pulled through, and both are read off the
/// stored record:
///
/// * it names no upstream to cache from — a rule with no `upstreamRegistryUrl` addresses
///   no registry at all;
/// * the credential reference it carries cannot be READ — "This is usually caused because
///   of an issue with the Secrets Manager secret containing the credentials for the
///   upstream registry" (`UnableToGetUpstreamImageException`): a `credentialArn` outside
///   the [`CREDENTIAL_SECRET_NAME_PREFIX`] secret namespace names no secret to retrieve a
///   credential from, and a `customRoleArn` that is no IAM role names no role to assume to
///   read one, so the import has nothing to authenticate with.
///
/// Whether an upstream that authenticates its callers was given a credential at all is a
/// verdict on the rule rather than a failure to reach the upstream, so it is
/// [`pull_through_cache_rule_failure`]'s to report and not this one's: ECR attempts the pull
/// either way, and this emulator contacts no upstream registry to be refused by (see
/// [`registry_pull`]).
///
/// An empty `customRoleArn` is no credential: the member is modelled `@length(0..2048)`, so
/// the empty string is a legal value that stores no role.
pub fn pull_through_cache_upstream_failure(
    rule: &PullThroughCacheRule,
) -> Option<PullThroughCacheRuleFailure> {
    let prefix = rule.ecr_repository_prefix.as_deref().unwrap_or_default();
    let url = rule.upstream_registry_url.as_deref().unwrap_or_default();
    if url.trim().is_empty() {
        return Some(PullThroughCacheRuleFailure::new(
            FAILURE_INVALID_UPSTREAM_REGISTRY_URL,
            format!(
                "the pull through cache rule for '{prefix}' stores no upstreamRegistryUrl, \
                 so it names no upstream registry to cache from"
            ),
        ));
    }

    if let Some(credential) = rule.credential_arn.as_deref().filter(|arn| !arn.is_empty()) {
        if !is_pull_through_cache_credential_arn(credential) {
            return Some(PullThroughCacheRuleFailure::new(
                FAILURE_INVALID_CREDENTIAL_ARN,
                format!(
                    "the credentialArn '{credential}' stored on the pull through cache rule \
                     for '{prefix}' is not the ARN of an Amazon Web Services Secrets Manager \
                     secret named '{CREDENTIAL_SECRET_NAME_PREFIX}…', so no upstream \
                     credential can be retrieved from it"
                ),
            ));
        }
    }

    if let Some(role) = rule.custom_role_arn.as_deref().filter(|arn| !arn.is_empty()) {
        if !is_iam_role_arn(role) {
            return Some(PullThroughCacheRuleFailure::new(
                FAILURE_INVALID_CUSTOM_ROLE_ARN,
                format!(
                    "the customRoleArn '{role}' stored on the pull through cache rule for \
                     '{prefix}' is not the ARN of an IAM role, so no role can be assumed to \
                     read the upstream credential"
                ),
            ));
        }
    }

    None
}
