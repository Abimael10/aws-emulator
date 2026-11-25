// SPDX-License-Identifier: CC0-1.0

//! WHETHER a step's work holds a credential at all: what the cluster's Amazon EC2 instances
//! obtain from the instance profile the launch named.
//!
//! # The rule this module implements
//!
//! "Also called instance profile and Amazon EC2 role. An IAM role for an Amazon EMR
//! cluster. The Amazon EC2 instances of the cluster assume this role"
//! (`RunJobFlow$JobFlowRole` doc) — and an instance assumes a role through its instance
//! profile only where that role's trust policy admits the Amazon EC2 service principal
//! ([`INSTANCE_PROFILE_PRINCIPAL`], read through this tree's ONE reading of a trust
//! document, [`::service_common::iam_trust_policy::admits_service_principal`]). So an
//! instance whose profile carries no role that admits Amazon EC2 obtains NO credential from
//! its own instance metadata, and the work of a step that needs one does not get it: that
//! step FAILS, and the calls it would have made are made by nothing.
//!
//! That is where a trust mismatch surfaces. Naming such a profile does not stop the launch
//! ([`super::instance_profile_violation`]): the cluster starts, and what fails is the step
//! whose work needed the credential the node could not obtain — never a step reported
//! complete for work this emulator made the calls of on a cluster that holds no credential
//! to make them with.
//!
//! A step carrying a runtime role of its own ("the Amazon Resource Name (ARN) of the
//! runtime role for a step on the cluster", `Step$ExecutionRoleArn`) runs as THAT role
//! whatever the cluster's instances obtain, which is what a runtime role is for.
//!
//! WHICH role a cluster's work runs as is also what its AWS calls are AUTHORIZED against —
//! the permissions in force on that role decide what the work may really do, and a call they
//! refuse is not made ([`work_role_arn`], which resolves the role out of the value that named
//! it so every such reading asks about the same principal).
//!
//! So what a step's work holds is a credential OF that role, named by the role's own ARN
//! rather than by the value the launch wrote: the calls the work makes are then signed as a
//! session of it ([`super::step_command_transfers`]) and judged by the service that serves
//! them against that role's identity policies, with an explicit `Deny` final — which is why a
//! prefix the role is denied `s3:GetObject` on is not read by a step's command however the
//! command asks for it, the command exits non-zero and the step ends FAILED with nothing its
//! later stages would have written. A credential of the PROFILE would be a credential of no
//! principal, and the calls it signed would be bounded by nothing.
//!
//! That holds of a role that STATES what it may do. A role this emulator holds no
//! identity-based policy for says nothing about what its nodes may do — the routine state of a
//! role provisioned out of band, read exactly as every other reading in this tree reads it
//! ([`::service_common::iam_role::role_states_permissions`]) — so its work is not signed as it
//! and keeps the standing below rather than having an empty policy set refuse every call it
//! makes.
//!
//! IAM is reached the way any emulated service reaches another, through the ONE in-process
//! IAM client this tree shares ([`::service_common::iam_client::iam_client`]), so nothing
//! here names another service crate. Where IAM answers nothing about the profile — a
//! deployment with no IAM mounted, a trust document this emulator cannot read — nothing is
//! known about the credential, and the step keeps the standing every other in-process call
//! between two emulated services has rather than being failed on an assumption.

use super::*;

/// The service principal a role has to trust for an Amazon EC2 instance to obtain a
/// credential for it through its instance profile. See the module docs.
const INSTANCE_PROFILE_PRINCIPAL: &str = "ec2.amazonaws.com";

/// What the work of ONE step holds for the AWS calls it makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepCredentials {
    /// What the work SIGNS AS: a session of that role where this emulator can mint one, and
    /// otherwise the credential the framework hands this emulator's handlers, which is what
    /// every in-process call between two emulated services presents.
    ///
    /// It is the role's own ARN exactly where that role STATES what it may do, so its calls
    /// are bounded by the permissions in force on it ([`bounding_role`]); where it states
    /// none it is the value the step or its cluster named, which mints no session.
    Role(String),
    /// NO credential at all: the cluster's instances obtain none for their instance profile,
    /// so a call that needs one is made by nothing.
    Absent,
}

/// Whether the instances of a cluster obtain a credential from their instance profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObtainedCredential {
    /// The profile carries a role those instances may assume.
    Yes,
    /// It carries no such role, so they obtain none.
    No,
    /// IAM answered nothing this can read an answer off.
    Unknown,
}

/// What a step of the cluster launched with `job_flow_role` holds for its own AWS calls when
/// it carries no runtime role of its own.
///
/// Resolved against IAM where the step's work is performed, which is when the node needs the
/// credential: a profile carrying no role Amazon EC2 may assume gives
/// [`StepCredentials::Absent`], and every other answer is what the work signs as — the ROLE
/// IAM holds in that profile where that role states what it may do ([`bounding_role`]),
/// because a credential is a credential OF a role and what the work may do is what that
/// role's permissions allow.
///
/// Where nothing is known — a value naming no profile of this account, a profile IAM holds no
/// record of, a role it holds no permissions for, an emulator with no IAM mounted — the value
/// the launch named is left as it was written, which is the standing the module docs give
/// every such non-answer.
pub async fn cluster_step_credentials(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    job_flow_role: &str,
) -> StepCredentials {
    // A value that names a ROLE is that role already, exactly as [`work_role_arn`] reads one,
    // so there is no profile to resolve it against — and that role is the ONLY name the work
    // has, whatever its policy set states: there is no other value to sign as, so
    // [`bounding_role`]'s question (which of two names to take) does not arise here.
    if ::service_common::iam_role::role_name(job_flow_role).is_some() {
        return StepCredentials::Role(job_flow_role.to_string());
    }
    let Some(roles) = profile_roles(ctx, region, job_flow_role).await else {
        return StepCredentials::Role(job_flow_role.to_string());
    };
    match obtained_from(&roles) {
        ObtainedCredential::No => StepCredentials::Absent,
        ObtainedCredential::Yes | ObtainedCredential::Unknown => StepCredentials::Role(
            bounding_role(ctx, region, &roles)
                .await
                .unwrap_or_else(|| job_flow_role.to_string()),
        ),
    }
}

/// The ROLE a profile's work signs as: the one IAM holds in the profile, where that role
/// STATES what it may do — an identity-based policy of its own, inline or attached, read
/// through this tree's one reading of that question
/// ([`::service_common::iam_role::role_states_permissions`]).
///
/// `None` where it states none, which is the routine state of a role provisioned out of band:
/// such a policy set says nothing about what the cluster's nodes may do, so there is nothing
/// for the work's calls to be bounded by, and signing as that role would turn an empty set
/// into a refusal of every call the work makes. A role that DOES state permissions is the
/// authority on what its work may do, and signing as it is what puts every call the work makes
/// — whatever service serves it — under those permissions, an explicit `Deny` final.
async fn bounding_role(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    roles: &[::aws_sdk_iam::types::Role],
) -> Option<String> {
    let arn = held_role_arn(roles)?;
    ::service_common::iam_role::role_states_permissions(ctx, region, &arn)
        .await
        .then_some(arn)
}

/// The roles IAM holds in the instance profile `job_flow_role` names.
///
/// `None` where nothing was learned: a value naming no instance profile of this account, an
/// answer this emulator could not read, and any failure of the hop at all, including an
/// emulator with no IAM mounted — the non-answer every reading here takes of those.
async fn profile_roles(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    job_flow_role: &str,
) -> Option<Vec<::aws_sdk_iam::types::Role>> {
    let name = instance_profile_name(job_flow_role)?;
    let held = ::service_common::iam_client::iam_client(ctx, region)
        .get_instance_profile()
        .instance_profile_name(name)
        .send()
        .await
        .ok()?;
    Some(held.instance_profile()?.roles().to_vec())
}

/// The ROLE whose permissions an AWS call made by a cluster's work is authorized against,
/// out of the value that NAMED it.
///
/// A step carrying a runtime role of its own runs as THAT role ("the Amazon Resource Name
/// (ARN) of the runtime role for a step on the cluster", `Step$ExecutionRoleArn`), so such a
/// value names the role itself. Every other value is the cluster's instance profile, and
/// what the nodes run as is the role IAM holds in it — "An instance profile can contain only
/// one IAM role" (`AddRoleToInstanceProfile`), so that one role is the answer.
///
/// `None` where nothing is known: a value naming neither, a profile IAM holds no record of
/// or that carries no role, and every failure [`profile_roles`] answers nothing for.
pub async fn work_role_arn(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    named: &str,
) -> Option<String> {
    if ::service_common::iam_role::role_name(named).is_some() {
        return Some(named.to_string());
    }
    held_role_arn(&profile_roles(ctx, region, named).await?)
}

/// The ARN of the role an instance profile carries — "an instance profile can contain only one
/// IAM role" (`AddRoleToInstanceProfile`), so the first one IAM reported is the answer, and a
/// profile carrying none answers nothing.
fn held_role_arn(roles: &[::aws_sdk_iam::types::Role]) -> Option<String> {
    roles
        .first()
        .and_then(|role| (!role.arn().is_empty()).then(|| role.arn().to_string()))
}

/// Whether the roles a profile carries answer an instance asking for a credential, which is
/// the reading of those records alone.
fn obtained_from(roles: &[::aws_sdk_iam::types::Role]) -> ObtainedCredential {
    // A trust document IAM holds and this cannot read is no evidence either way, so it leaves
    // the answer unknown rather than reporting a credential the instances do not obtain.
    let mut unreadable = false;
    for role in roles {
        let admits = role.assume_role_policy_document().and_then(|document| {
            ::service_common::iam_trust_policy::admits_service_principal(
                document,
                INSTANCE_PROFILE_PRINCIPAL,
            )
        });
        match admits {
            Some(true) => return ObtainedCredential::Yes,
            Some(false) => {}
            None => unreadable = true,
        }
    }
    if unreadable {
        ObtainedCredential::Unknown
    } else {
        // Every role the profile carries trusts another principal — or it carries none at all
        // — so nothing in it answers an instance asking for a credential.
        ObtainedCredential::No
    }
}
