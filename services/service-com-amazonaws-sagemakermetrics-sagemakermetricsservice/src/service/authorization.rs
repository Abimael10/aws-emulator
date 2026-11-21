// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLER of a request to this data plane perform the action it asks
//! for — the decision both operations take BEFORE they read or write a metric.
//!
//! Every request to an AWS service is signed, and the credential it is signed with speaks for
//! a principal: an IAM user's long-term access key, or a temporary session the token service
//! minted for a ROLE — which is what a training job's SDK call presents. AWS decides such a
//! request at the IAM layer, ahead of the service looking at what the request named: a
//! principal whose identity-based policies do not allow the action is told it is not
//! authorized rather than told anything about the resource, and the call has NO effect — a
//! refused `BatchPutMetrics` stores no point, so a later `BatchGetMetrics` for it reports an
//! empty series.
//!
//! The refusal is `AccessDeniedException` ([`crate::scaffolded::ACCESS_DENIED_CODE`]), which
//! is a shape this model declares nowhere (`smithy_query(query="operation",
//! operation="BatchGetMetrics,BatchPutMetrics")` binds no error at all, and the service adds
//! none): the refusal belongs to the authorization layer rather than to an operation, so it
//! travels on each operation's error channel through
//! [`crate::scaffolded::Scaffolded::access_denied`].
//!
//! # Where each piece lives
//!
//! Nothing of the DECISION is written here. The SageMaker CONTROL PLANE — the service that
//! publishes the `sagemaker` action namespace this service's operations are also published
//! under, and that owns the trial components these metrics are written for — holds the one
//! gate for that namespace
//! (`service_com_amazonaws_sagemaker_sagemaker::service::authorization`): who the request's
//! access key id speaks for, the identity-based documents and the permissions boundary the
//! IAM store holds for that principal, the shared evaluation of those documents
//! (`service_common::iam_authorization`), and the sentence a denial carries. So a policy
//! written for SageMaker's actions decides this data plane's calls too, and the two services
//! answer one question one way.
//!
//! What IS this module's own is the pair of things only this service knows: that the action
//! is `sagemaker:<OperationName>` for the operation the request reached, and which resources
//! a request of this service ADDRESSES ([`trial_component_addressed`],
//! [`referenced_resources`]).
//!
//! # The resources a request addresses
//!
//! * `BatchPutMetrics` names the trial component it ingests for by NAME, so the resource is
//!   the trial-component ARN of that name — as the control plane spells one
//!   (`arn:aws:sagemaker:<region>:<account>:experiment-trial-component/<name>`, the form
//!   `CreateTrialComponent$TrialComponentArn`'s own pattern declares) — in the Region the
//!   caller signed for.
//! * `BatchGetMetrics` names a resource per query, by ARN, so the resources are the
//!   references those queries carry, and a query that carries none is judged against
//!   [`ANY_RESOURCE`], the resource IAM writes for a request that addresses no resource of
//!   its own.
//!
//! A request is permitted only when IAM permits the action on EVERY resource it addresses:
//! the decision is taken on the whole request, before the service splits a batch into the
//! per-item work it reports individually, so there is one verdict for the call rather than
//! one per element.
//!
//! Each resource is built from what the REQUEST carried — a name, a reference — never from a
//! resolved record, so the decision is the same whether or not that resource exists here, and
//! a refusal tells an unauthorized caller nothing about what this emulator holds.

use ::service_com_amazonaws_sagemaker_sagemaker::service::authorization as sagemaker_gate;
use ::service_com_amazonaws_sagemaker_sagemaker::state as sagemaker_state;

/// The resource IAM writes for a request that addresses no resource of its own, as the
/// control plane's gate spells it.
pub use sagemaker_gate::ANY_RESOURCE;

/// The ARN segment a trial component is addressed by, from the modelled
/// `TrialComponentArn` pattern
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:experiment-trial-component/`): the
/// same segment the control plane's `CreateTrialComponent` mints one with, which is why it
/// is read from that service rather than spelled again here.
pub use sagemaker_state::TRIAL_COMPONENT_KIND;

/// The ARN of the trial component named `trial_component_name` in `region`, built through
/// the SageMaker control plane's own ARN builder so this ARN and the one
/// `DescribeTrialComponent` reports are one spelling.
#[must_use]
pub fn trial_component_resource(region: &str, trial_component_name: &str) -> String {
    sagemaker_state::resource_arn(region, TRIAL_COMPONENT_KIND, trial_component_name)
}

/// The ARN of the trial component ONE request addresses: [`trial_component_resource`] in the
/// Region the caller signed for.
///
/// The Region comes from the caller's own SigV4 credential scope, read through the control
/// plane's `request_region` — the one reading of it the SageMaker services share, so a scope
/// naming something that is not a Region cannot reshape the ARN's colon-delimited fields.
#[must_use]
pub fn trial_component_addressed(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    trial_component_name: &str,
) -> String {
    trial_component_resource(&sagemaker_state::request_region(ctx), trial_component_name)
}

/// The resources a request addresses by REFERENCE: the value each element carries, and
/// [`ANY_RESOURCE`] for an element that carries none — an element whose reference the request
/// left unset addresses no resource of this service.
///
/// Each reference is taken as the request sent it, before this service reads whether it is a
/// well-formed SageMaker reference at all: IAM decides ahead of the service, and the resource
/// element of a statement is matched against the value the caller submitted.
///
/// Repeats are collapsed, so a batch naming one resource in many elements is one decision.
#[must_use]
pub fn referenced_resources<'a>(references: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut resources: Vec<String> = Vec::new();
    for reference in references {
        let resource = if reference.is_empty() {
            ANY_RESOURCE
        } else {
            reference
        };
        if !resources.iter().any(|held| held == resource) {
            resources.push(resource.to_string());
        }
    }
    resources
}

/// `Some(message)` when IAM does not allow `sagemaker:<operation>` on one of the resources
/// the request addresses — the sentence the refusal carries, naming the FIRST such resource —
/// and `None` when it allows the action on all of them.
///
/// Call it FIRST in a handler, before the metric store is read or written and before this
/// service judges anything about the request: a request IAM refuses never reached the service
/// on AWS, so it ingests nothing and reports nothing.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resources: &[String],
) -> Option<String> {
    resources
        .iter()
        .find_map(|resource| sagemaker_gate::authorize(engine, ctx, operation, resource).err())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the resource a `BatchPutMetrics` decision is taken against is the
    /// trial-component ARN of the name the request carried, in the Region it was signed for —
    /// the spelling the modelled `TrialComponentArn` pattern declares.
    #[test]
    fn the_ingest_resource_is_the_trial_component_arn_of_the_name_the_request_carried() {
        let account = ::engine::emulator_account_id();
        // (case, the Region signed for, the trial component the request named)
        let rows = [
            ("a tracked component", "us-east-1", "run-1"),
            ("another Region", "eu-west-3", "run-1"),
            ("a name no component carries", "us-east-1", "never-created"),
            ("a hyphenated name", "ap-southeast-4", "tc-with-hyphens-1"),
        ];
        for (case, region, name) in rows {
            let actual = trial_component_resource(region, name);
            assert_eq!(
                actual,
                format!("arn:aws:sagemaker:{region}:{account}:experiment-trial-component/{name}"),
                "case {case:?}: expected the trial-component ARN, got {actual:?}"
            );
        }
    }

    /// RULE: the resources a reference-carrying batch is judged against are the references
    /// its elements carry, collapsed to one entry per distinct resource, with the
    /// any-resource wildcard standing for an element that carries none.
    #[test]
    fn the_referenced_resources_are_the_references_the_request_carried() {
        let first = trial_component_resource("us-east-1", "first");
        let second = trial_component_resource("us-east-1", "second");
        // (case, the references the elements carried, the resources judged)
        let rows: Vec<(&str, Vec<&str>, Vec<String>)> = vec![
            ("one reference", vec![first.as_str()], vec![first.clone()]),
            (
                "the same reference twice",
                vec![first.as_str(), first.as_str()],
                vec![first.clone()],
            ),
            (
                "two distinct references",
                vec![first.as_str(), second.as_str()],
                vec![first.clone(), second.clone()],
            ),
            (
                "an element carrying no reference",
                vec![""],
                vec![ANY_RESOURCE.to_string()],
            ),
            (
                "a reference beside an element carrying none",
                vec![first.as_str(), ""],
                vec![first.clone(), ANY_RESOURCE.to_string()],
            ),
            (
                "a reference that is not an ARN, taken as sent",
                vec!["not-an-arn"],
                vec!["not-an-arn".to_string()],
            ),
            ("no element at all", Vec::new(), Vec::new()),
        ];
        for (case, references, expected) in rows {
            let actual = referenced_resources(references.iter().copied());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the judged resources {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the action asked about is `sagemaker:<OperationName>`, so a statement scoped to
    /// one trial component's ARN and one of this service's actions decides exactly that call
    /// and nothing else — not the other operation, not another component, not another Region.
    #[test]
    fn a_grant_scoped_to_one_component_covers_that_components_action_alone() {
        use ::service_com_amazonaws_sagemaker_sagemaker::service::authorization::ACTION_PREFIX;
        use ::service_common::iam_authorization::{self as iam, Decision};

        let granted = trial_component_resource("us-east-1", "granted");
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
            "Action":"{ACTION_PREFIX}:BatchPutMetrics","Resource":"{granted}"}}]}}"#
        );
        // (case, the operation the request reached, the resource it addressed, allowed)
        let rows: &[(&str, &str, String, bool)] = &[
            (
                "the granted action on the granted component",
                "BatchPutMetrics",
                granted.clone(),
                true,
            ),
            (
                "the read side of the same data plane",
                "BatchGetMetrics",
                granted.clone(),
                false,
            ),
            (
                "the granted action on another component",
                "BatchPutMetrics",
                trial_component_resource("us-east-1", "other"),
                false,
            ),
            (
                "the granted action in another Region",
                "BatchPutMetrics",
                trial_component_resource("eu-west-3", "granted"),
                false,
            ),
        ];
        for (case, operation, resource, expected) in rows {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let said = iam::identity_decision(
                ::std::slice::from_ref(&document),
                &iam::Request::new(&action, resource, &[]),
            );
            let actual = iam::is_allowed(said, Decision::NotStated);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected allowed == {expected} for {action} on {resource}, got \
                 {actual} ({said:?})"
            );
        }
    }
}
