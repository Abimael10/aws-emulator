// SPDX-License-Identifier: CC0-1.0

//! The IAM roles a pipeline configuration names, and whether this service can take them on.
//!
//! A Data Prepper configuration reaches other Amazon Web Services resources under a role it
//! names: the `sts_role_arn` option of a source's, processor's or sink's `aws` container is
//! "the IAM role that the pipeline assumes" to read or write there. The ingestion service
//! can only do that where the named role exists and its trust policy admits the ingestion
//! service principal, so a configuration naming a role it cannot assume describes a pipeline
//! that could never run — which is why the body is validated BEFORE anything is persisted
//! and the call refused with the modelled `ValidationException`.
//!
//! The reading is the crate's one reading of a configuration document
//! ([`super::document_as_json`]); the question about a role is the tree's one question about
//! one ([`::service_common::iam_role::role_is_unassumable`]), so this module states only
//! WHICH roles a configuration names and how the refusal is worded.

use ::serde_json::Value;

/// The service principal the ingestion service assumes a pipeline's roles as.
///
/// A role a pipeline names has to carry a trust relationship with it, and the refusal names
/// it, so the principal is stated once and the check and the message cannot disagree.
pub const INGESTION_SERVICE_PRINCIPAL: &str = "osis-pipelines.amazonaws.com";

/// The configuration option naming the role a source, processor or sink assumes.
const STS_ROLE_ARN_OPTION: &str = "sts_role_arn";

/// How many DISTINCT roles one configuration body has resolved against IAM.
///
/// EMULATOR RESOURCE GUARD, not an AWS limit: `PipelineConfigurationBody` is
/// `@length(1, 100000)`, which bounds the document's size but not how many role options it
/// writes, and each distinct role costs one lookup — so the number of lookups one request
/// can ask for is capped here rather than left to the body's size. It is far past any Data
/// Prepper pipeline set, which assumes a role per source and per sink.
const MAX_RESOLVED_ROLES: usize = 32;

/// Every role ARN `configuration_body` names, in document order and without repeats, at most
/// [`MAX_RESOLVED_ROLES`] of them.
///
/// A configuration can name a role in as many places as it has sources, processors and
/// sinks, so the whole document is walked for the option rather than the handful of
/// containers one layout happens to put it in. A body that does not parse as one YAML
/// document names no role: what such a body IS is settled where the body's own syntax is
/// read, not here.
pub fn configuration_role_arns(configuration_body: &str) -> Vec<String> {
    let nesting_bound = super::pipeline_configuration::MAX_CONFIGURATION_NESTING;
    if ::service_common::yaml_nesting::nesting_depth_bound(configuration_body, nesting_bound)
        > nesting_bound
    {
        return Vec::new();
    }
    let Ok(document) = super::document_as_json(configuration_body) else {
        return Vec::new();
    };
    let mut named = Vec::new();
    collect_role_arns(&document, &mut named);
    named
}

/// Add every `sts_role_arn` value at or below `document` to `named`, in document order,
/// skipping one already named and stopping at [`MAX_RESOLVED_ROLES`].
///
/// The recursion is bounded by the nesting bound its one caller takes before the document is
/// parsed at all, so a caller's document cannot choose how deep this goes.
fn collect_role_arns(document: &Value, named: &mut Vec<String>) {
    if named.len() >= MAX_RESOLVED_ROLES {
        return;
    }
    match document {
        Value::Object(members) => {
            for (name, value) in members {
                if name == STS_ROLE_ARN_OPTION {
                    if let Some(role_arn) = value.as_str().map(str::trim) {
                        if !role_arn.is_empty()
                            && !named.iter().any(|held| held == role_arn)
                            && named.len() < MAX_RESOLVED_ROLES
                        {
                            named.push(role_arn.to_string());
                        }
                    }
                }
                collect_role_arns(value, named);
            }
        }
        Value::Array(entries) => {
            for entry in entries {
                collect_role_arns(entry, named);
            }
        }
        _ => {}
    }
}

/// Why one role a configuration names cannot be taken on: the service could not assume it,
/// and what the role would have to carry for it to be able to.
///
/// The only variable part is the role ARN, copied out of the submitted configuration
/// verbatim, so a caller can tell which of the roles its document names was refused.
pub fn unable_to_assume_role(role_arn: &str) -> String {
    format!(
        "Unable to assume the STS role {role_arn}. This role must contain a trust \
         relationship with '{INGESTION_SERVICE_PRINCIPAL}'."
    )
}

/// How the service states that the submitted `PipelineConfigurationBody` did not validate:
/// the member it is about, followed by the per-error validation messages
/// (`com.amazonaws.osis#ValidationMessage`, the same per-error shape `ValidatePipeline`
/// reports) in the order the document raised them.
///
/// The envelope is fixed; the variable part is the list of messages, which comes from the
/// submitted document.
pub fn configuration_body_invalid(messages: &[String]) -> String {
    let listed = messages
        .iter()
        .map(|message| format!("ValidationMessage(message={message})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("pipelineConfigurationBody is invalid with the following errors: [{listed}]")
}

/// The `ValidationException` message for a submitted configuration body naming a role this
/// service cannot assume, or `None` when every role it names is one it can.
///
/// Asked BEFORE a pipeline is minted or updated, by every operation that takes a
/// `PipelineConfigurationBody`, so one submitted document is judged the same way whichever
/// of them was called. What makes a role unassumable is IAM's answer about it
/// ([`::service_common::iam_role::role_is_unassumable`]): this account holds no role under
/// that name, or the trust policy the stored role carries does not admit the ingestion
/// service principal. A value naming no role of this account establishes nothing, so it is
/// not refused.
pub async fn unassumable_configuration_roles(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    configuration_body: &str,
) -> Option<String> {
    let mut refused = Vec::new();
    for role_arn in configuration_role_arns(configuration_body) {
        if ::service_common::iam_role::role_is_unassumable(
            ctx,
            region,
            &role_arn,
            INGESTION_SERVICE_PRINCIPAL,
        )
        .await
        {
            refused.push(unable_to_assume_role(&role_arn));
        }
    }
    (!refused.is_empty()).then(|| configuration_body_invalid(&refused))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the roles a configuration names are the `sts_role_arn` values its document
    /// carries, wherever in the document they sit.
    ///
    /// The rows differ along what the reading generalises over: which container names the
    /// option, how many sub-pipelines and sinks name one, repeats of one role, a value that
    /// is not a string, the two surface syntaxes one document is written in, and a body that
    /// is no document at all.
    #[test]
    fn the_roles_named_are_the_ones_the_configuration_carries() {
        struct Row {
            case: &'static str,
            body: &'static str,
            named: Vec<&'static str>,
        }
        let rows = vec![
            Row {
                case: "a sink's aws container names the role",
                body: "version: \"2\"\np:\n  source:\n    http:\n  sink:\n    - s3:\n        \
                       bucket: \"records\"\n        aws:\n          sts_role_arn: \
                       \"arn:aws:iam::000000000000:role/sink\"\n",
                named: vec!["arn:aws:iam::000000000000:role/sink"],
            },
            Row {
                case: "a source and a sink naming two different roles",
                body: "version: \"2\"\np:\n  source:\n    s3:\n      aws:\n        \
                       sts_role_arn: \"arn:aws:iam::000000000000:role/source\"\n  \
                       sink:\n    - s3:\n        bucket: \"records\"\n        aws:\n          \
                       sts_role_arn: \"arn:aws:iam::000000000000:role/sink\"\n",
                named: vec![
                    "arn:aws:iam::000000000000:role/source",
                    "arn:aws:iam::000000000000:role/sink",
                ],
            },
            Row {
                case: "two sub-pipelines naming the SAME role, which is named once",
                body: "version: \"2\"\na:\n  source:\n    http:\n  sink:\n    - s3:\n        \
                       bucket: \"a\"\n        aws:\n          sts_role_arn: \
                       \"arn:aws:iam::000000000000:role/one\"\nb:\n  source:\n    http:\n  \
                       sink:\n    - s3:\n        bucket: \"b\"\n        aws:\n          \
                       sts_role_arn: \"arn:aws:iam::000000000000:role/one\"\n",
                named: vec!["arn:aws:iam::000000000000:role/one"],
            },
            Row {
                case: "the option written directly on a sink, in JSON syntax",
                body: "{\"version\": \"2\", \"p\": {\"source\": {\"http\": {}}, \
                       \"sink\": [{\"opensearch\": {\"sts_role_arn\": \
                       \"arn:aws:iam::000000000000:role/open\"}}]}}",
                named: vec!["arn:aws:iam::000000000000:role/open"],
            },
            Row {
                case: "an option whose value is not a string names no role",
                body: "version: \"2\"\np:\n  source:\n    http:\n  sink:\n    - s3:\n        \
                       bucket: \"records\"\n        aws:\n          sts_role_arn:\n            \
                       - \"arn:aws:iam::000000000000:role/listed\"\n",
                named: Vec::new(),
            },
            Row {
                case: "an empty option names no role",
                body: "version: \"2\"\np:\n  source:\n    http:\n  sink:\n    - s3:\n        \
                       bucket: \"records\"\n        aws:\n          sts_role_arn: \"  \"\n",
                named: Vec::new(),
            },
            Row {
                case: "a configuration that names no role at all",
                body: "version: \"2\"\np:\n  source:\n    http:\n  sink:\n    - s3:\n        \
                       bucket: \"records\"\n",
                named: Vec::new(),
            },
            Row {
                case: "a body that is not a configuration document",
                body: "not: [a, pipeline\n",
                named: Vec::new(),
            },
        ];
        for Row { case, body, named } in rows {
            let read = configuration_role_arns(body);
            let expected: Vec<String> = named.iter().map(ToString::to_string).collect();
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: however many role options a document writes, the number of DISTINCT roles one
    /// body has resolved is bounded ([`MAX_RESOLVED_ROLES`]), so the lookups one request can
    /// ask for do not grow with the body's size.
    #[test]
    fn the_roles_one_body_names_are_bounded() {
        let mut body = String::from("version: \"2\"\n");
        for index in 0..(MAX_RESOLVED_ROLES * 3) {
            body.push_str(&format!(
                "p{index}:\n  source:\n    s3:\n      aws:\n        sts_role_arn: \
                 \"arn:aws:iam::000000000000:role/r{index}\"\n"
            ));
        }
        let read = configuration_role_arns(&body);
        assert_eq!(
            read.len(),
            MAX_RESOLVED_ROLES,
            "case \"a body naming three times the bound\": expected {MAX_RESOLVED_ROLES} \
             roles, got {}",
            read.len()
        );
    }

    /// RULE: the refusal states the member that did not validate and one validation message
    /// per role the service could not assume, with each role copied from the request.
    #[test]
    fn the_refusal_states_the_member_and_one_message_per_role() {
        let one = configuration_body_invalid(&[unable_to_assume_role(
            "arn:aws:iam::000000000000:role/absent",
        )]);
        assert_eq!(
            one,
            "pipelineConfigurationBody is invalid with the following errors: \
             [ValidationMessage(message=Unable to assume the STS role \
             arn:aws:iam::000000000000:role/absent. This role must contain a trust \
             relationship with 'osis-pipelines.amazonaws.com'.)]",
            "case \"one refused role\": got {one:?}"
        );

        let two = configuration_body_invalid(&[
            unable_to_assume_role("arn:aws:iam::000000000000:role/first"),
            unable_to_assume_role("arn:aws:iam::000000000000:role/second"),
        ]);
        assert!(
            two.starts_with("pipelineConfigurationBody is invalid with the following errors: [")
                && two.ends_with(")]")
                && two.matches("ValidationMessage(message=").count() == 2
                && two.contains("role/first")
                && two.contains("role/second"),
            "case \"two refused roles\": both must be listed in one envelope, got {two:?}"
        );
    }
}
