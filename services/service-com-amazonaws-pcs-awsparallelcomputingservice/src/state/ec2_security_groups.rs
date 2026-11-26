// SPDX-License-Identifier: CC0-1.0

//! The account's security groups, a fact this service RESOLVES rather than holds.
//!
//! A security group is not a resource of this service: it is Amazon EC2's. "The subnets and
//! security groups for the cluster must already exist before you use this API action"
//! (`CreateCluster`'s own documentation) — the same sentence [`super::ec2_subnets`] answers
//! the first half of — and `NetworkingRequest$securityGroupIds` is "a list of security group
//! IDs associated with the Elastic Network Interface (ENI) created in subnets", so a create
//! that would attach the control-plane ENI to caller-named groups resolves what it was given
//! BEFORE it mints anything, and EC2 is the one answer to "does this account hold that
//! security group".
//!
//! The hop is the service's one EC2 hop ([`super::ec2_client`]), so nothing leaves the
//! process and the Region a request was signed for is read as a Region LABEL before it can
//! reach an endpoint authority. Nothing is cached and nothing is stored here: a group created
//! a moment ago resolves at once, and one that was deleted stops resolving at once.
//!
//! Every refusal built here is grounded in what EC2 ANSWERED. A hop that could not be made
//! at all — an emulator with no EC2 mounted, an answer this emulator could not read — is
//! this service failing to ask rather than EC2 reporting a group the account does not have,
//! so it refuses nothing: this gate keeps nothing of the group it resolves, so a refusal
//! assumed from silence would cost a create that EC2 would have resolved.
//!
//! What the CALLER is told is PCS's own fixed sentence ([`SECURITY_GROUPS_ABSENT_MESSAGE`]):
//! it names no security group id of the request and carries nothing of EC2's own answer,
//! because PCS answers for the request it was given rather than reporting the call it made on
//! the caller's behalf.

use super::{service_validation, ValidationException};

/// What PCS answers a create whose security groups it could not resolve against the account's
/// network.
///
/// A CONSTANT of the service, worded the way it words the other half of the same sentence
/// ([`super::ec2_subnets::SUBNETS_ABSENT_MESSAGE`]): the whole sentence is fixed text, naming
/// neither the ids the request carried nor how many of them failed to resolve — "1 or more"
/// is the service's own wording, not a count.
pub const SECURITY_GROUPS_ABSENT_MESSAGE: &str =
    "AWS PCS couldn't find 1 or more of the security groups you provided.";

/// Every EC2 error code that says a value names no security group of the account:
/// `InvalidGroup.NotFound` for an id no group carries, `InvalidGroupId.Malformed` for a value
/// that is not a security group id at all. Both say the same thing about an ENI's membership.
///
/// The two codes are named rather than matched by prefix, because EC2's other `InvalidGroup*`
/// codes are about a group that DOES exist (`InvalidGroup.InUse`, `InvalidGroup.Duplicate`)
/// and say nothing about whether the account holds it.
const GROUP_ID_REFUSAL_CODES: [&str; 2] = ["InvalidGroup.NotFound", "InvalidGroupId.Malformed"];

/// The modelled `ValidationException` (HTTP 400) a request naming `security_group_ids` is
/// refused with, or `None` when every group resolves and the call may proceed.
///
/// `Some` only where EC2 ANSWERED that an id names no security group of this account in this
/// Region: it reports one of [`GROUP_ID_REFUSAL_CODES`] for such an id, and an id it reported
/// no group for says the same thing. `None` for an empty list — there is nothing to resolve — and for every
/// answer that is not such a report, which is this emulator failing to ask (see the module
/// docs).
///
/// The caller must have bounded `security_group_ids` already: it is cloned into the call
/// below, so an unbounded list would size this work from the request. Every handler that
/// reaches here has put it through [`super::stored_list_violation`] first, because it is a
/// list the create would also PERSIST — and that ceiling is well inside the largest page EC2
/// answers a `DescribeSecurityGroups` that names its ids with, so the reported set below is
/// never a partial page.
pub async fn violation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    security_group_ids: &[String],
) -> Option<ValidationException> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    if security_group_ids.is_empty() {
        return None;
    }
    // No Region label, so no endpoint to ask EC2 at: the same answer an unreachable EC2
    // gives, which grounds no refusal.
    let client = super::ec2_client::ec2_client(ctx, region)?;
    let answer = client
        .describe_security_groups()
        .set_group_ids(Some(security_group_ids.to_vec()))
        .send()
        .await;
    let error = match answer {
        Ok(output) => {
            // EC2 reports one group per id it resolved, so an id it reported nothing for
            // names no security group of the account — the same thing the refusal below
            // says.
            let reported: Vec<&str> = output
                .security_groups()
                .iter()
                .filter_map(::aws_sdk_ec2::types::SecurityGroup::group_id)
                .collect();
            if security_group_ids
                .iter()
                .all(|id| reported.contains(&id.as_str()))
            {
                return None;
            }
            return Some(service_validation(
                SECURITY_GROUPS_ABSENT_MESSAGE.to_string(),
            ));
        }
        Err(error) => error,
    };
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default();
    if !GROUP_ID_REFUSAL_CODES.contains(&code) {
        // At `warn`: a deployment where this hop fails is one where this gate does not run
        // at all, which is worth seeing, and it IS why a membership was not resolved.
        ::tracing::warn!(
            %region,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the security groups a PCS create named could not be resolved against EC2"
        );
        return None;
    }
    Some(service_validation(
        SECURITY_GROUPS_ABSENT_MESSAGE.to_string(),
    ))
}
