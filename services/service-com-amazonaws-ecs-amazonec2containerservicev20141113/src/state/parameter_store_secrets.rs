// SPDX-License-Identifier: CC0-1.0

//! The half of a container's `secrets` that names a SYSTEMS MANAGER PARAMETER: whether the
//! task's execution role may retrieve it, and the value the container is given.
//!
//! # The rule this module implements
//!
//! `Secret$valueFrom` is "the secret to expose to the container. The supported values are
//! either the full ARN of the Secrets Manager secret or the full ARN of the parameter in the
//! SSM Parameter Store", and the parameter form may also be written as the parameter's NAME:
//! "If the SSM Parameter Store parameter exists in the same Region as the task you're
//! launching, then you can use either the full ARN or name of the parameter. If the parameter
//! exists in a different Region, then the full ARN must be specified"
//! (`ContainerDefinition$secrets`). Either way the value is read BEFORE the container starts,
//! as the task's EXECUTION role, and exposed to the container as the environment variable the
//! entry's `name` spells — the same step and the same role the Secrets Manager half of a
//! revision's references is retrieved under ([`super::container_secrets`]).
//!
//! The agent reads it WITH DECRYPTION, because what a container is given is the parameter's
//! value rather than its ciphertext. So a `SecureString` reference turns on the KMS key its
//! value is sealed under as well as on Parameter Store's own permission: a customer managed
//! key's policy and the documents in force on the EXECUTION role decide the decrypt, and the
//! key's own KEY STATE decides it after that — KMS tells a principal the key does not permit
//! nothing about the key's state, so a role holding no grant is refused the same way whatever
//! state the key is in, while a role the key permits is told what about the state refuses the
//! retrieval. A reference whose value cannot be produced for any of those reasons launches
//! nothing: no container starts, nothing is delivered to the log group its configuration
//! names, and the task is stopped at the instant it would otherwise have started, with the
//! stop code of a task that never ran ([`STOP_CODE_TASK_FAILED_TO_START`]) and the
//! `stoppedReason` the agent words for the Parameter Store fetch it could not complete
//! ([`unable_to_fetch_parameter_message`]).
//!
//! Every one of those verdicts is the OWNING service's: whether the role may read the
//! parameter, whether this account holds a value to read and whether the key would open it
//! are answered by Systems Manager's own gate over its own store and the key's own policy
//! ([`::service_com_amazonaws_ssm_amazonssm::state::parameter_retrieval`]), and the VALUE is
//! answered by that service's own `GetParameter` over the emulator's in-process callback — so
//! what is injected is what a client reads back from that service, and nothing is fetched for
//! a reference it would refuse.
//!
//! # Where each half happens
//!
//! Exactly where the Secrets Manager half happens, for the same reason: the authorization
//! verdict is recorded by the pass that fetches the task's images
//! ([`evaluate_image_pulls`]), because the registry authentication and the secrets are one
//! step of the agent's, and the VALUE is read by the retrieval pass
//! ([`settle_secret_retrievals`]), which is off the store's lock because it calls another
//! emulated service. The value is read again where the container's program starts
//! ([`super::container_runtime`]), since nothing of it is kept in this service's store.
//!
//! LOCK ORDER: the cross-service decision takes the Systems Manager store's lock, then IAM's
//! and KMS's, each holding no other while it does, and hands back owned values — so it is
//! called with NO guard of this service's store held, the same contract
//! [`evaluate_image_pulls`] has.

use ::service_com_amazonaws_ssm_amazonssm::state::parameter_retrieval;

use super::*;

/// What a `Secret$valueFrom` naming a Systems Manager parameter references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterStoreReference {
    /// The reference AS THE REVISION WROTE IT — the parameter's full ARN or its name, either
    /// optionally carrying a `:selector` — which is what the owning service resolves and what
    /// a refusal quotes back.
    pub reference: String,
    /// The Region the parameter is resolved at: the one a full ARN names, and the Region the
    /// TASK runs in for a reference written as a bare name, which is the only Region such a
    /// reference can name.
    pub region: String,
}

impl ParameterStoreReference {
    /// The reference `value_from` names when it addresses a Systems Manager parameter, and
    /// `None` for every other value — an ARN of another service, or of another resource type
    /// of this one.
    ///
    /// `task_region` is the Region the task runs in, which is where a reference written as a
    /// bare name is resolved.
    pub fn parse(value_from: &str, task_region: &str) -> Option<Self> {
        if !value_from.starts_with(ARN_PREFIX) {
            // Not an ARN at all: the parameter form written as a name, which names the
            // Region the task itself runs in. A name carries no `arn:` prefix, so there is
            // nothing else it could be.
            return (!value_from.is_empty()).then(|| Self {
                reference: value_from.to_string(),
                region: task_region.to_string(),
            });
        }
        let parsed = ::service_common::arn::parse(value_from)?;
        if parsed.vendor != PARAMETER_STORE_VENDOR
            || !parsed.resource.starts_with(PARAMETER_RESOURCE_PREFIX)
        {
            return None;
        }
        // An ARN field left EMPTY names no Region at all, which resolves through the endpoint
        // the retrieval is made at — the Region the task runs in.
        let region = if parsed.region.is_empty() {
            task_region
        } else {
            parsed.region
        };
        Some(Self {
            reference: value_from.to_string(),
            region: region.to_string(),
        })
    }
}

/// The prefix every ARN carries, which is what tells a `valueFrom` written as a parameter
/// NAME from one written as an ARN.
const ARN_PREFIX: &str = "arn:";

/// The ARN vendor segment of a Systems Manager parameter, which is what tells a `valueFrom`
/// naming one from a `valueFrom` naming a secret of another store.
const PARAMETER_STORE_VENDOR: &str = "ssm";

/// The resource type a parameter's ARN names
/// (`arn:aws:ssm:<region>:<account>:parameter/<name>`).
const PARAMETER_RESOURCE_PREFIX: &str = "parameter/";

/// The `Task$stoppedReason` of a task whose containers could not be launched because a
/// Systems Manager parameter one of their `secrets` names could not be fetched.
///
/// The one wording of that stop, filled with what the agent calls the resource it fetched
/// from Parameter Store ([`unable_to_retrieve_resource_message`]): the agent retrieves the
/// values of a revision in one batch read, so the resource it names is the `secrets` it could
/// not get `from ssm`. `failure` is the answer that service gave, which carries the
/// authorization refusal, the refusal of the key the value is sealed under, or the names it
/// could not resolve.
pub fn unable_to_fetch_parameter_message(failure: &str) -> String {
    unable_to_retrieve_resource_message(SSM_SECRETS_RESOURCE, failure)
}

/// What the agent calls the resource it fetched from Parameter Store: the `secrets` of the
/// revision, retrieved from `ssm` in one batch read.
const SSM_SECRETS_RESOURCE: &str = "secrets from ssm";

/// The agent's account of a reference the batch read it makes reported among the names it
/// could not resolve, which is how it words the `InvalidParameters` that read answers with.
///
/// A `SecureString` whose key cannot open it is one of those names rather than an error class
/// of its own ([`parameter_retrieval::RetrievalRefusal::Unresolved`]): the batch read reports
/// every value it could not produce that way.
fn invalid_parameters_message(reference: &str) -> String {
    format!("invalid ssm parameters: {reference}")
}

/// Why the Systems Manager parameter `reference` names could not be retrieved as the
/// execution role — the `stoppedReason` the task's stop then carries — and `None` when it
/// can be.
///
/// `role_arn` is the task's execution role and `session_arn` the session Amazon ECS assumed
/// it in ([`execution_role_session_arn`]); a launch that names no execution role states no
/// principal for a policy to be read against, and the retrieval is then decided on what the
/// account holds and on the key's state alone.
///
/// Called with no guard of this service's store held: the verdict is the owning service's,
/// taken under that service's own lock and the locks of the stores it consults.
pub fn unretrievable_parameter<Eng: Send + Sync + 'static>(
    engine: &Eng,
    reference: &ParameterStoreReference,
    role_arn: Option<&str>,
    session_arn: Option<&str>,
) -> Option<String> {
    let principal = role_arn.map(|role_arn| parameter_retrieval::RetrievalPrincipal {
        role_arn,
        session_arn,
    });
    let failure = match parameter_retrieval::serve_parameter_retrieval(
        engine,
        &reference.region,
        &reference.reference,
        principal,
    ) {
        Ok(()) => return None,
        Err(parameter_retrieval::RetrievalRefusal::NotAuthorized { message, .. })
        | Err(parameter_retrieval::RetrievalRefusal::NotAuthorizedByKey { message }) => {
            format!("{ACCESS_DENIED_CLASS}: {message}")
        }
        Err(parameter_retrieval::RetrievalRefusal::Unresolved { name }) => {
            invalid_parameters_message(&name)
        }
    };
    Some(unable_to_fetch_parameter_message(&failure))
}

/// The VALUE the parameter `reference` names exposes, read through Systems Manager's OWN
/// `GetParameter` with decryption over the emulator's in-process callback — so what a
/// container is given is what a client reads back from the service that holds it.
///
/// `Err` carries the agent's account of why there was no value to expose, which is what the
/// `stoppedReason` of the task it was retrieved for reports
/// ([`unable_to_fetch_parameter_message`]). The value is never logged, and nothing of it is
/// kept in this service's store.
pub async fn retrieved_parameter_value(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    reference: &ParameterStoreReference,
) -> Result<String, String> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    // The ONE construction of the in-process Systems Manager client an emulated service
    // drives: the transport routes in process, so no socket is opened and no name is
    // resolved, and the credentials it carries are the emulator's own — WHICH role may read
    // the parameter is decided ahead of the retrieval, against that role's own documents
    // ([`unretrievable_parameter`]).
    let answer = ::service_common::ssm_client::ssm_client(callback, &reference.region)
        .get_parameter()
        .name(&reference.reference)
        .with_decryption(true)
        .send()
        .await;
    let answer = match answer {
        Ok(answer) => answer,
        Err(error) => {
            // Visible to an operator: this is why the container was not started. The
            // reference is named; nothing of any value is.
            ::tracing::warn!(
                parameter = %reference.reference,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a parameter a container takes could not be retrieved, so its program is not run"
            );
            let message = match (error.code(), error.message()) {
                (Some(class), Some(message)) => format!("{class}: {message}"),
                (Some(class), None) => class.to_string(),
                // A read that resolved the reference to nothing is the answer a batch read
                // reports among its unresolvable names rather than an error class of its own.
                (None, _) => invalid_parameters_message(&reference.reference),
            };
            return Err(message);
        }
    };
    answer
        .parameter()
        .and_then(|parameter| parameter.value())
        .map(ToString::to_string)
        .ok_or_else(|| invalid_parameters_message(&reference.reference))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: the case, the `valueFrom` it is written with, and the
    /// `(reference, region)` the parse answers for it — `None` for a value that names no
    /// parameter of this store.
    type Parsed = (&'static str, &'static str, Option<(&'static str, &'static str)>);

    /// RULE: a `valueFrom` naming a Systems Manager parameter is that parameter's ARN or its
    /// name, resolved in the Region the ARN names and in the task's own Region for a name;
    /// every other value names no parameter of that store.
    #[test]
    fn a_value_from_parses_into_the_parameter_and_its_region() {
        // (case, valueFrom, the reference it names in a task running in us-west-2)
        let rows: &[Parsed] = &[
            (
                "the parameter's own ARN",
                "arn:aws:ssm:us-west-2:000000000000:parameter/prod/db",
                Some((
                    "arn:aws:ssm:us-west-2:000000000000:parameter/prod/db",
                    "us-west-2",
                )),
            ),
            (
                "an ARN of a parameter in another Region",
                "arn:aws:ssm:eu-central-1:000000000000:parameter/prod/db",
                Some((
                    "arn:aws:ssm:eu-central-1:000000000000:parameter/prod/db",
                    "eu-central-1",
                )),
            ),
            (
                "an ARN of another partition",
                "arn:aws-us-gov:ssm:us-gov-west-1:000000000000:parameter/prod/db",
                Some((
                    "arn:aws-us-gov:ssm:us-gov-west-1:000000000000:parameter/prod/db",
                    "us-gov-west-1",
                )),
            ),
            (
                "an ARN whose Region field is empty",
                "arn:aws:ssm::000000000000:parameter/prod/db",
                Some(("arn:aws:ssm::000000000000:parameter/prod/db", "us-west-2")),
            ),
            (
                "an ARN carrying a version selector",
                "arn:aws:ssm:us-west-2:000000000000:parameter/prod/db:3",
                Some((
                    "arn:aws:ssm:us-west-2:000000000000:parameter/prod/db:3",
                    "us-west-2",
                )),
            ),
            (
                "a hierarchical parameter name",
                "/prod/db/password",
                Some(("/prod/db/password", "us-west-2")),
            ),
            ("a flat parameter name", "dbpassword", Some(("dbpassword", "us-west-2"))),
            (
                "a name carrying a label selector",
                "/prod/db/password:release",
                Some(("/prod/db/password:release", "us-west-2")),
            ),
            (
                "a Secrets Manager secret",
                "arn:aws:secretsmanager:us-west-2:000000000000:secret:injected-AbCdEf",
                None,
            ),
            (
                "an ARN of another resource type of this service",
                "arn:aws:ssm:us-west-2:000000000000:document/runbook",
                None,
            ),
            ("an empty value", "", None),
        ];
        for (case, value_from, expected) in rows {
            let actual = ParameterStoreReference::parse(value_from, "us-west-2");
            let expected = expected.map(|(reference, region)| ParameterStoreReference {
                reference: reference.to_string(),
                region: region.to_string(),
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the stop of a task whose Parameter Store fetch failed carries the agent's
    /// `ResourceInitializationError`, the resource it could not get from that store, and the
    /// answer the owning service gave.
    #[test]
    fn the_stop_reason_names_the_fetch_and_the_failure() {
        let message = unable_to_fetch_parameter_message(&invalid_parameters_message("/prod/db"));
        assert_eq!(
            message,
            "ResourceInitializationError: unable to pull secrets or registry auth: execution \
             resource retrieval failed: unable to retrieve secrets from ssm: service call has \
             been retried 1 time(s): invalid ssm parameters: /prod/db",
            "case \"a reference nothing resolved\": got {message:?}"
        );
    }
}
