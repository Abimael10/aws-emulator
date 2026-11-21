// SPDX-License-Identifier: CC0-1.0

//! WHETHER an instance gets the credentials of the role its instance profile carries: the
//! TRUST that role has to publish for an instance to assume it at all.
//!
//! # The rule this module implements
//!
//! An instance obtains the credentials of its instance profile's role by ASSUMING that role,
//! and a role may be assumed only by a principal its own trust policy admits — "the IAM role
//! … must trust the Amazon EC2 service to assume it" ([`INSTANCE_PROFILE_PRINCIPAL`]). So a
//! profile carrying a role that trusts another service entirely gives its instances NOTHING:
//! the machine's metadata service publishes no credentials under that role's name, because no
//! session of it could have been vended, and a client that attached the wrong trust policy
//! sees that rather than a credential nothing could have minted.
//!
//! The question is asked WHERE the session is minted
//! ([`super::instance_role_credentials`]) and nowhere else: the role's NAME is still what the
//! guest's `iam/security-credentials/` listing answers, because which role the profile carries
//! is a fact whatever that role trusts.
//!
//! This is the same rule this workspace already applies to the Amazon EC2 instances of an
//! Amazon EMR cluster (`service-com-amazonaws-emr-elasticmapreduce`'s own
//! `state::instance_profile_credentials`, whose step FAILS where the cluster's instances can
//! obtain no credential), and the document is read through the ONE reading of a trust policy
//! this workspace has ([`::service_common::iam_trust_policy::admits_service_principal`]) — so
//! nothing here parses a policy itself and two services cannot answer differently about one
//! document.

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;

/// The service principal a role has to trust for an instance to assume it through its instance
/// profile. See the module docs.
pub const INSTANCE_PROFILE_PRINCIPAL: &str = "ec2.amazonaws.com";

/// Whether an instance may assume `role` through its instance profile, so that a session of it
/// can be minted for the machine.
///
/// A trust document IAM holds that this emulator cannot READ is no evidence either way, and
/// leaves the role assumable: withholding the credentials of a role only because this emulator
/// could not read its document would deny calls AWS permits. A document it CAN read that
/// withholds the grant — it says nothing about the Amazon EC2 service principal, or it denies
/// it explicitly — is a role no instance may assume.
#[must_use]
pub fn role_is_assumable_by_an_instance(role: &Role) -> bool {
    role.assume_role_policy_document
        .as_deref()
        .and_then(|document| {
            ::service_common::iam_trust_policy::admits_service_principal(
                document,
                INSTANCE_PROFILE_PRINCIPAL,
            )
        })
        .unwrap_or(true)
}
