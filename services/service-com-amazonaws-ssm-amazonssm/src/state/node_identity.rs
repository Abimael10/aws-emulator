// SPDX-License-Identifier: CC0-1.0

//! Whether a request was made BY the managed node it names — the question the operations
//! Systems Manager serves to a node's own agent ask, and nobody else can answer for them.
//!
//! # Why an operation asks
//!
//! A few of this service's calls are not made by a client at all: they are made by the agent
//! inside a managed node, with that node's own credentials, about that node. The patch
//! snapshot read says so outright — "This API is primarily used by the AWS-RunPatchBaseline
//! Systems Manager document … If you run the command locally, such as with the Command Line
//! Interface (CLI), the system attempts to use your local Amazon Web Services credentials and
//! the operation fails" (`GetDeployablePatchSnapshotForInstance`) — so a caller presenting any
//! other identity is refused rather than served another node's snapshot.
//!
//! # How a node's own identity is recognised
//!
//! A node acts as the IAM role it registered with, in a session NAMED FOR THE NODE: the
//! credentials published to an Amazon EC2 instance's guest are a session of its instance
//! profile's role whose name is the instance id
//! (`service_com_amazonaws_ec2_amazonec2::state::instance_role_credentials`), and a hybrid
//! machine registers under the role its activation names
//! (`CreateActivationRequest$IamRole`) with the `mi-` id it was given. So the request is the
//! node's when the principal it speaks for is an assumed-role session whose SESSION NAME is
//! that node's id and whose ROLE is the role the node reports registering as
//! (`InstanceInformation$IamRole`) — both facts of the stored node and of the caller, never of
//! the request body, so naming a node in the body proves nothing.
//!
//! This decision is NOT the IAM one: whether the caller's policies allow the action is
//! [`super::identity_authorization`], asked first and about every caller. What is asked here
//! is who the caller IS, which no policy can grant.

use ::service_common::iam_role;

use super::ManagedNodeRecord;

/// Whether the request this `caller_arn` speaks for was made by the node `node`, as the module
/// docs read that: a session of the node's registration role, named for the node.
///
/// The comparison is against the session ARN THIS account would publish to that node
/// ([`::service_common::iam_role::assumed_role_session_arn`] of that role of this account,
/// [`super::activation_role::role_arn`]), so a principal of another partition or another
/// account is not that node whatever it is named, and neither is one that is no assumed-role
/// session at all.
///
/// A node that reports no `IamRole` at all — one registered before this endpoint could read
/// which role it runs as — is identified by the session NAME alone: that is every fact the
/// store holds about its identity, and refusing on evidence it does not have would turn a
/// node's own call away.
#[must_use]
pub fn request_is_from_node(caller_arn: &str, node: &ManagedNodeRecord) -> bool {
    let Some(node_id) = node.node_id() else {
        return false;
    };
    // The role the caller's own session is of, which is also the role the session ARN this
    // account publishes for it is built from.
    let Some(session_role) = iam_role::assumed_role_session_role_name(caller_arn) else {
        return false;
    };
    let published =
        iam_role::assumed_role_session_arn(&super::activation_role::role_arn(session_role), node_id);
    if caller_arn != published {
        return false;
    }
    match node.properties.iam_role.as_deref() {
        Some(role) if !role.is_empty() => entity_name(role) == session_role,
        _ => true,
    }
}

/// The NAME of the role a value names, with any path dropped: a role reported as
/// `service-role/AmazonSSMRoleForInstances` and a session of `AmazonSSMRoleForInstances` are
/// the same role, because the ARN of an assumed-role session carries the role's name without
/// its path.
fn entity_name(role: &str) -> &str {
    role.rsplit('/').next().unwrap_or(role)
}

/// How a caller that is not the node it named is refused: IAM's own sentence for a request no
/// grant covers ([`::service_common::iam_authorization::not_authorized_to_perform`]), naming
/// the principal that signed, the action it asked for and the node it asked about.
///
/// NO reason clause follows it, which is the form that sentence takes where no rule of the
/// caller's own documents withheld the grant: those documents are asked first
/// ([`super::identity_authorization`]) and had already allowed this call. What refused it is
/// the identity itself, which is a fact about the caller rather than about a policy, so there
/// is no rule to name.
#[must_use]
pub fn not_the_node_message(caller_arn: &str, operation: &str, resource: &str) -> String {
    ::service_common::iam_authorization::not_authorized_to_perform(
        caller_arn,
        &super::identity_authorization::action_for(operation),
        resource,
    )
}
