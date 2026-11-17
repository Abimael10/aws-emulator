// SPDX-License-Identifier: CC0-1.0

//! The build environment's variables that name a value ANOTHER service holds: what a
//! `SECRETS_MANAGER` environment variable is worth inside the build.
//!
//! # The rule this module implements
//!
//! `EnvironmentVariable$type` `SECRETS_MANAGER` is "an environment variable stored in Secrets
//! Manager. For environment variables of this type, specify the name of the secret as the
//! `value` of the EnvironmentVariable. The secret value will be substituted for the name at
//! runtime" — so the build's shell sees the secret's PLAINTEXT under that variable's name,
//! never the reference, and the reference's selectors (`<secret-id>:<json-key>:
//! <version-stage>:<version-id>`) say which part of which version is substituted.
//!
//! The substitution is made for the build, not by it: it happens as the build environment is
//! provisioned, before the first command of any phase runs. The read is made AS the project's
//! service role (`Project$serviceRole` is "the ARN of the IAM role that enables CodeBuild to
//! interact with dependent Amazon Web Services services on behalf of the Amazon Web Services
//! account"), so a build whose role may not read the secret is a build whose environment
//! cannot be provisioned: it FAILS in that phase, reports what the owning service answered,
//! and runs no command — there is no `BUILD` phase, nothing is exported, and no plaintext is
//! anywhere in what this service stores or reports.
//!
//! Both halves are the OWNING service's to answer, which is why neither is decided here:
//! whether the role may read the secret and whether there is a value to read are answered by
//! Secrets Manager's own gate over its own store
//! ([`::service_com_amazonaws_secretsmanager_secretsmanager::state::secret_retrieval`]), and
//! the value itself by that service's own `GetSecretValue` over the emulator's in-process
//! transport. What the build sees is therefore what a client reads back from the service that
//! holds it.
//!
//! The reference stays the reference: nothing resolved here is written into this service's
//! store, so `BatchGetProjects` and `BatchGetBuilds` keep reporting the typed reference the
//! caller gave, and no value reaches a response or a log.
//!
//! A variable of type `PARAMETER_STORE` is left as it was — the session sets nothing for it —
//! because the service that owns a parameter has no retrieval-on-behalf-of-a-role gate in this
//! emulator yet: substituting it would report a value without the grant that entitles the role
//! to it.
//!
//! LOCK ORDER: the gate below takes the Secrets Manager store's lock (which takes the IAM
//! store's) and the read is a call to another service, so this is called with NO guard of this
//! service's store held — the contract [`super::build_run`] already keeps for every step of a
//! build's run.

use std::collections::BTreeMap;

use ::service_com_amazonaws_secretsmanager_secretsmanager::state::secret_retrieval;
use ::smithy_server_runtime::EmbeddedEmulator;

use super::build_source::client_error;
use crate::ops::types::{Build, PhaseContext};

/// The `EnvironmentVariableType` whose value names a secret of Secrets Manager.
pub const SECRETS_MANAGER_VARIABLE_TYPE: &str = "SECRETS_MANAGER";

/// How many of one build's variables are resolved: the ceiling this crate already holds a
/// build's environment variables to ([`super::MAX_ENVIRONMENT_VARIABLES`]), since every
/// reference is one in-process retrieval made for every build of the project. A build whose
/// environment references MORE than that is one whose environment cannot be provisioned — it is
/// refused rather than run with the references past the ceiling silently unresolved, which is
/// the empty-value shape this module exists to prevent.
///
/// That ceiling is reachable: it is applied to the OVERRIDES one `StartBuild` carries
/// (`StartBuild`, `StartBuildBatch`), while a project's own `environment.environmentVariables`
/// is stored without it, so a build's merged environment can arrive with more references than
/// one request may override.
const MAX_RESOLVED_VARIABLES: usize = super::MAX_ENVIRONMENT_VARIABLES;

/// What a `SECRETS_MANAGER` variable's value names.
///
/// The form is the one `EnvironmentVariable$value` documents for that type, and the CodeBuild
/// User Guide's `env/secrets-manager` states the selectors written after it:
/// `<secret-id>:<json-key>:<version-stage>:<version-id>`, each of the three optional. The
/// secret id itself is a name or an ARN of the secret, and an ARN carries colons of its own,
/// so a value that IS one is split after its last ARN field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretReference {
    /// The secret itself: the name or the ARN the reference opens with, which is what the
    /// owning service resolves.
    pub secret_id: String,
    /// The key of the JSON document held in the secret whose value is substituted, and `None`
    /// for a reference that substitutes the whole value.
    pub json_key: Option<String>,
    /// The staging label the version is selected by, and `None` for the version the owning
    /// service serves a read that names none.
    pub version_stage: Option<String>,
    /// The version id the version is selected by, and `None` for a reference naming none.
    pub version_id: Option<String>,
}

/// How many fields an ARN carries before the resource it names: `arn`, the partition, the
/// vendor, the Region and the account.
const ARN_FIELDS_BEFORE_RESOURCE: usize = 5;

/// The prefix that tells a reference opening with a secret's ARN from one opening with its
/// name.
const ARN_PREFIX: &str = "arn:";

/// The resource type a secret's ARN names, which is the field after the account.
const SECRET_RESOURCE_TYPE: &str = "secret";

impl SecretReference {
    /// The reference `value` names, or `None` when it names no secret at all — a value with no
    /// secret id in it.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let fields: Vec<&str> = value.split(':').collect();
        // The secret id is the FIRST field for a reference written with a name, and the whole
        // ARN — through the secret's own name field — for one written with an ARN.
        let id_fields = if value.starts_with(ARN_PREFIX) {
            // `arn:<partition>:secretsmanager:<region>:<account>:secret:<name>`: the resource
            // type and the name are two fields of their own.
            if fields.get(ARN_FIELDS_BEFORE_RESOURCE) != Some(&SECRET_RESOURCE_TYPE) {
                return None;
            }
            ARN_FIELDS_BEFORE_RESOURCE + 2
        } else {
            1
        };
        if fields.len() < id_fields {
            return None;
        }
        let secret_id = fields[..id_fields].join(":");
        if secret_id.is_empty() || fields[id_fields - 1].is_empty() {
            return None;
        }
        let selector = |index: usize| {
            fields
                .get(index)
                .filter(|value| !value.is_empty())
                .map(|value| (*value).to_string())
        };
        Some(Self {
            secret_id,
            json_key: selector(id_fields),
            version_stage: selector(id_fields + 1),
            version_id: selector(id_fields + 2),
        })
    }
}

/// What resolving one build's environment came to.
#[derive(Debug, Clone)]
pub enum ResolvedEnvironment {
    /// The value the environment holds for each variable that names another service's value,
    /// by the variable's own name — empty for a build that references none.
    Resolved(BTreeMap<String, String>),
    /// The environment could not be provisioned, and this is what the phase that provisions it
    /// reports.
    Refused(PhaseContext),
}

/// Resolve every `SECRETS_MANAGER` variable of `build`'s environment, as the project's service
/// role, before any command of the build runs.
///
/// Called with no guard of this service's store held (see the module docs). `region` is the
/// Region the build is running in, which is where its secrets are looked for.
pub async fn resolve_environment<Eng: Send + Sync + 'static>(
    callback: &EmbeddedEmulator,
    engine: &Eng,
    region: &str,
    build: &Build,
) -> ResolvedEnvironment {
    let referenced: Vec<(String, String)> = build
        .environment
        .as_ref()
        .and_then(|environment| environment.environment_variables.as_deref())
        .unwrap_or_default()
        .iter()
        .filter(|variable| variable.r#type.as_deref() == Some(SECRETS_MANAGER_VARIABLE_TYPE))
        .map(|variable| (variable.name.clone(), variable.value.clone()))
        .collect();
    let mut resolved = BTreeMap::new();
    if referenced.is_empty() {
        return ResolvedEnvironment::Resolved(resolved);
    }
    if referenced.len() > MAX_RESOLVED_VARIABLES {
        // The ceiling is this emulator's, so a build it stops is TOLD rather than run with
        // variables nothing resolved.
        ::tracing::warn!(
            referenced = referenced.len(),
            limit = MAX_RESOLVED_VARIABLES,
            "this build's environment references more secrets than this emulator resolves for \
             one build, so the build is not provisioned"
        );
        return ResolvedEnvironment::Refused(client_error(&too_many_references_message()));
    }
    let session_arn = build
        .service_role
        .as_deref()
        .zip(build.id.as_deref())
        .and_then(|(role_arn, build_id)| role_session_arn(role_arn, build_id));
    let principal = build
        .service_role
        .as_deref()
        .map(|role_arn| secret_retrieval::RetrievalPrincipal {
            role_arn,
            session_arn: session_arn.as_deref(),
        });
    for (name, reference) in referenced {
        let Some(parsed) = SecretReference::parse(&reference) else {
            return ResolvedEnvironment::Refused(client_error(&no_secret_named_message(&name)));
        };
        // WHETHER the role may read it, and whether there is a value to read, are the owning
        // service's answers.
        if let Err(refusal) =
            secret_retrieval::serve_secret_retrieval(engine, region, &parsed.secret_id, principal)
        {
            return ResolvedEnvironment::Refused(client_error(&refusal_message(&refusal)));
        }
        match retrieved_value(callback, region, &parsed).await {
            Some(value) => {
                resolved.insert(name, value);
            }
            None => {
                return ResolvedEnvironment::Refused(client_error(&unresolved_secret_message(
                    &parsed,
                )))
            }
        }
    }
    ResolvedEnvironment::Resolved(resolved)
}

/// The ARN of the SESSION a build ACTS IN: CodeBuild assumes the project's service role for
/// the build, so the principal a call made for the build is made by is an assumed-role session
/// of that role named after the build it is for
/// (`arn:<partition>:sts::<account>:assumed-role/<role name>/AWSCodeBuild-<build uuid>`).
///
/// It is the principal of every call a build makes — the secret reads here, and the calls its
/// own commands make ([`super::build_tools`]) — so there is one answer to "as whom does this
/// build act".
///
/// `None` when `role_arn` is no IAM role ARN — there is no session to name for it.
pub fn role_session_arn(role_arn: &str, build_id: &str) -> Option<String> {
    let parsed = ::service_common::arn::parse(role_arn)?;
    let role_name = parsed
        .resource
        .split('/')
        .next_back()
        .filter(|name| !name.is_empty())?;
    // The build's own id is `<projectName>:<uuid>`, and the uuid half is what names the
    // session.
    let uuid = super::uuid_half(build_id);
    Some(format!(
        "arn:{}:sts::{}:assumed-role/{role_name}/{SESSION_NAME_PREFIX}{uuid}",
        parsed.partition, parsed.account
    ))
}

/// What CodeBuild names the session it assumes a project's service role in.
const SESSION_NAME_PREFIX: &str = "AWSCodeBuild-";

/// The error class Secrets Manager refuses an unauthorized read with, which is the AWS common
/// `AccessDeniedException`.
const ACCESS_DENIED_CLASS: &str = "AccessDeniedException";

/// The error class Secrets Manager answers a read of a secret it holds nothing for with
/// (`ResourceNotFoundException`, its own model).
const RESOURCE_NOT_FOUND_CLASS: &str = "ResourceNotFoundException";

/// What the provisioning phase reports for a retrieval the owning service did not serve: that
/// service's own answer, as its error class and the sentence it refuses with — so the withheld
/// action (`secretsmanager:GetSecretValue`) and the resource it was withheld on are named
/// exactly as Secrets Manager names them.
fn refusal_message(refusal: &secret_retrieval::RetrievalRefusal) -> String {
    match refusal {
        secret_retrieval::RetrievalRefusal::NotAuthorized { message, .. } => {
            format!("{ACCESS_DENIED_CLASS}: {message}")
        }
        secret_retrieval::RetrievalRefusal::NotFound { message } => {
            format!("{RESOURCE_NOT_FOUND_CLASS}: {message}")
        }
    }
}

/// What is reported for a variable whose value names no secret, and for a build that
/// references more than [`MAX_RESOLVED_VARIABLES`]: there is nothing to substitute, or too much
/// to, so the environment cannot be provisioned.
///
/// AWS publishes no sentence for either condition — the first is a reference no caller of real
/// CodeBuild can store (a `SECRETS_MANAGER` variable with no secret id in it) and the second is
/// this emulator's own ceiling — so each states the condition in this service's own terms, with
/// the variable, the type and the limit taken from the request and from the stated ceiling.
/// Where the OWNING service has an answer, that answer is what is reported instead
/// ([`refusal_message`], [`unresolved_secret_message`]).
fn no_secret_named_message(name: &str) -> String {
    format!(
        "The environment variable {name} of type {SECRETS_MANAGER_VARIABLE_TYPE} names no \
         secret to substitute"
    )
}

/// See [`no_secret_named_message`].
fn too_many_references_message() -> String {
    format!(
        "A build environment may reference at most {MAX_RESOLVED_VARIABLES} values of type \
         {SECRETS_MANAGER_VARIABLE_TYPE}"
    )
}

/// What is reported for a secret the owning service SERVED that still holds nothing to
/// substitute: no `SecretString` at all, or no such key in the document it holds.
///
/// A read whose selector resolves to no value is what Secrets Manager words "Secrets Manager
/// can't find the specified secret value for <selector>: <value>" — the sentence family its own
/// state module carries for a `VersionId` and for a staging label — so a `json-key` that
/// resolves to nothing is reported the same way, with the key the reference named. For a secret
/// that holds no `SecretString` the condition is the secret's own content rather than a
/// selector, and AWS publishes no sentence for it, so it is stated as this tree states it
/// elsewhere (CloudFormation's dynamic-reference resolution: "the secret … holds no
/// SecretString").
fn unresolved_secret_message(reference: &SecretReference) -> String {
    match &reference.json_key {
        Some(json_key) => format!(
            "Secrets Manager can't find the specified secret value for JSON key: {json_key}"
        ),
        None => format!(
            "The secret {} holds no SecretString to substitute",
            reference.secret_id
        ),
    }
}

/// The value `reference` substitutes: the secret's string value, or the value of the JSON key
/// the reference selects within it.
///
/// `None` when the read did not serve one — the secret holds no string value, or the key the
/// reference selects is not in the document — which is a build environment that cannot be
/// provisioned.
async fn retrieved_value(
    callback: &EmbeddedEmulator,
    region: &str,
    reference: &SecretReference,
) -> Option<String> {
    let mut request = secrets_manager_client(callback, region)
        .get_secret_value()
        .secret_id(&reference.secret_id);
    if let Some(stage) = &reference.version_stage {
        request = request.version_stage(stage);
    }
    if let Some(version_id) = &reference.version_id {
        request = request.version_id(version_id);
    }
    let answer = match request.send().await {
        Ok(answer) => answer,
        Err(error) => {
            // Visible to an operator: this is why the build was not provisioned. The reference
            // is named; no value is.
            ::tracing::warn!(
                secret_id = %reference.secret_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a secret a build's environment references could not be read, so the build \
                 runs nothing"
            );
            return None;
        }
    };
    let held = answer.secret_string()?;
    let Some(json_key) = &reference.json_key else {
        return Some(held.to_string());
    };
    // The reference selects "a specific JSON key within the secret": the value is read as the
    // key/value document a secret of that shape holds, and the key's own value is what the
    // build is given — a document that states no such key states no value for it.
    let document: ::serde_json::Value = ::serde_json::from_str(held).ok()?;
    let selected = document.get(json_key)?;
    Some(match selected {
        // A string field is substituted as its characters; every other JSON value as the text
        // the document writes it with.
        ::serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}

/// A Secrets Manager client over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// routes in process, so no socket is opened and no name is resolved. The configuration is the
/// one every callback client in this workspace uses: nothing to retry, time out or watch for a
/// stalled stream, because the transport is in process, and the credentials the callback hands
/// over are the emulator's own — WHICH role may read the secret is decided ahead of the read,
/// against that role's own documents ([`resolve_environment`]).
fn secrets_manager_client(
    callback: &EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_secretsmanager::Client {
    let config = ::aws_sdk_secretsmanager::Config::builder()
        .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(::aws_sdk_secretsmanager::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_secretsmanager::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a `SECRETS_MANAGER` value parses into the secret it names and the selectors
    /// written after it, whether the secret is named by name or by ARN.
    ///
    /// The rows differ along what the rule generalises over: which selectors the reference
    /// carries, which are left empty, and whether the secret is named by name or by ARN.
    #[test]
    fn a_reference_parses_into_the_secret_and_its_selectors() {
        let arn = "arn:aws:secretsmanager:us-west-2:000000000000:secret:build-AbCdEf";
        // (case, the value, the reference it names)
        let rows: Vec<(&str, String, Option<SecretReference>)> = vec![
            (
                "a secret's name alone",
                "build-secret".to_string(),
                Some(SecretReference {
                    secret_id: "build-secret".to_string(),
                    json_key: None,
                    version_stage: None,
                    version_id: None,
                }),
            ),
            (
                "a name and a JSON key",
                "build-secret:token".to_string(),
                Some(SecretReference {
                    secret_id: "build-secret".to_string(),
                    json_key: Some("token".to_string()),
                    version_stage: None,
                    version_id: None,
                }),
            ),
            (
                "a staging label with no JSON key",
                "build-secret::AWSPREVIOUS".to_string(),
                Some(SecretReference {
                    secret_id: "build-secret".to_string(),
                    json_key: None,
                    version_stage: Some("AWSPREVIOUS".to_string()),
                    version_id: None,
                }),
            ),
            (
                "every selector at once",
                "build-secret:token:AWSCURRENT:v1".to_string(),
                Some(SecretReference {
                    secret_id: "build-secret".to_string(),
                    json_key: Some("token".to_string()),
                    version_stage: Some("AWSCURRENT".to_string()),
                    version_id: Some("v1".to_string()),
                }),
            ),
            (
                "the secret's own ARN",
                arn.to_string(),
                Some(SecretReference {
                    secret_id: arn.to_string(),
                    json_key: None,
                    version_stage: None,
                    version_id: None,
                }),
            ),
            (
                "an ARN and a JSON key",
                format!("{arn}:token"),
                Some(SecretReference {
                    secret_id: arn.to_string(),
                    json_key: Some("token".to_string()),
                    version_stage: None,
                    version_id: None,
                }),
            ),
            ("no value at all", String::new(), None),
            (
                "an ARN of another resource type of the same service",
                "arn:aws:secretsmanager:us-west-2:000000000000:other:build".to_string(),
                None,
            ),
            (
                "an ARN naming no secret",
                "arn:aws:secretsmanager:us-west-2:000000000000:secret:".to_string(),
                None,
            ),
        ];
        for (case, value, expected) in rows {
            let parsed = SecretReference::parse(&value);
            assert_eq!(
                parsed, expected,
                "case {case:?}: expected {expected:?}, got {parsed:?}"
            );
        }
    }

    /// RULE: the principal a build's secret read is made by is an assumed-role session of the
    /// project's service role, named after the build it is made for.
    #[test]
    fn the_read_is_made_as_a_session_of_the_service_role() {
        let uuid = "11111111-2222-3333-4444-555555555555";
        // (case, the role ARN, the build id, the session ARN it names)
        let rows: Vec<(&str, &str, String, Option<String>)> = vec![
            (
                "a role of this account",
                "arn:aws:iam::000000000000:role/codebuild",
                format!("project:{uuid}"),
                Some(format!(
                    "arn:aws:sts::000000000000:assumed-role/codebuild/AWSCodeBuild-{uuid}"
                )),
            ),
            (
                "a role under a path",
                "arn:aws:iam::000000000000:role/service-role/codebuild",
                format!("project:{uuid}"),
                Some(format!(
                    "arn:aws:sts::000000000000:assumed-role/codebuild/AWSCodeBuild-{uuid}"
                )),
            ),
            (
                "a role of another partition",
                "arn:aws-us-gov:iam::000000000000:role/codebuild",
                format!("project:{uuid}"),
                Some(format!(
                    "arn:aws-us-gov:sts::000000000000:assumed-role/codebuild/AWSCodeBuild-{uuid}"
                )),
            ),
            (
                "a bare role name",
                "codebuild",
                format!("project:{uuid}"),
                None,
            ),
        ];
        for (case, role_arn, build_id, expected) in rows {
            let session = role_session_arn(role_arn, &build_id);
            assert_eq!(
                session, expected,
                "case {case:?}: expected {expected:?}, got {session:?}"
            );
        }
    }
}
