// SPDX-License-Identifier: CC0-1.0

//! The service-linked role an Auto Scaling group calls other Amazon Web Services under.
//!
//! "The Amazon Resource Name (ARN) of the service-linked role that the Auto Scaling group
//! uses to call other Amazon Web Services service on your behalf. By default, Amazon EC2
//! Auto Scaling uses a service-linked role named `AWSServiceRoleForAutoScaling`, which it
//! creates if it does not exist" (`CreateAutoScalingGroupType$ServiceLinkedRoleARN`): a
//! group is BOUND to a role when it is created, whether the request named one or not, so
//! the role in force is a value this service decides at create time and every read of the
//! group reports — never an absent member.
//!
//! The ARN is built from IAM's OWN spelling of a service-linked role — the path IAM
//! reserves for one, followed by the service principal and the role name IAM publishes for
//! that principal — so the role the group names is the role IAM itself reports under that
//! name rather than a second spelling of it.

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;

/// The service principal Amazon EC2 Auto Scaling acts under, which the role a group is
/// bound to by default is linked to.
pub const SERVICE_PRINCIPAL: &str = "autoscaling.amazonaws.com";

/// The name of that role, as this service's own model publishes it: "a service-linked role
/// named `AWSServiceRoleForAutoScaling`" (`CreateAutoScalingGroupType$ServiceLinkedRoleARN`).
///
/// Only the fallback: the name is looked up in IAM's own catalog of published
/// service-linked role names first, so the two cannot disagree.
pub const DEFAULT_ROLE_NAME: &str = "AWSServiceRoleForAutoScaling";

/// The ARN of the service-linked role a group is bound to when the request that created it
/// named none.
///
/// A pure function of the account and the principal — IAM keeps this role provisioned for
/// the account, so there is nothing to look up in a store and nothing to create.
pub fn default_arn() -> String {
    let role_name =
        iam::published_service_linked_role_name(SERVICE_PRINCIPAL).unwrap_or(DEFAULT_ROLE_NAME);
    iam::role_arn(&iam::service_linked_role_path(SERVICE_PRINCIPAL), role_name)
}

/// The role a group is bound to for a request that named `requested`: the one it named,
/// or the account's default service-linked role ([`default_arn`]) when it named none.
pub fn bound_arn(requested: Option<String>) -> String {
    requested.unwrap_or_else(default_arn)
}
