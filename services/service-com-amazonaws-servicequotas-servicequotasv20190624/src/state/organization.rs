// SPDX-License-Identifier: CC0-1.0

//! Whether the account this emulator serves belongs to an AWS Organization, and with
//! which features — read out of the service that owns that fact.
//!
//! A quota request template is an ORGANIZATION-wide object: `AssociateServiceQuotaTemplate`
//! documents it as "Associates your quota request template with your organization. When a
//! new Amazon Web Services account is created in your organization, the quota increase
//! requests in the template are automatically applied to the account." So whether the
//! template plane exists for a caller at all is decided by the caller's organization
//! membership — and membership is AWS Organizations' own fact, not Service Quotas'.
//!
//! This service therefore keeps NO copy of it: it asks Organizations over the in-process
//! callback ([`smithy_server_runtime::OperationContext::callback`]) the framework gives a
//! handler for calling another emulated service, the same seam [`super::usage`] counts the
//! account's resources through. One read, one owner, so an organization created through
//! Organizations is immediately what the template gate sees.
//!
//! An answer that is not an organization means the account is in none: Organizations
//! reports `AWSOrganizationsNotInUseException` for an account that belongs to no
//! organization, and an emulator serving no Organizations at all holds no organization
//! the account could belong to either. Both are [`Membership::None`], which is the
//! standalone-account case the template gate refuses.
//!
//! An organization the caller belongs to but does not MANAGE is the third case: the
//! template is the organization's own object, written for the accounts created in it, so
//! it belongs to the account that manages the organization — "the account whose user is
//! calling the `CreateOrganization` operation automatically becomes the management
//! account of the new organization" — and a member account that is not that one is
//! [`Membership::NotManagementAccount`]. Which account that is comes from the same read:
//! the organization's own `MasterAccountId`.

use ::aws_sdk_organizations::types::OrganizationFeatureSet;

/// The Region AWS Organizations' single global endpoint is addressed at.
///
/// Organizations is a global service: its endpoint is `organizations.us-east-1.amazonaws.com`
/// for the commercial partition, and it holds ONE organization per account whichever
/// endpoint reads it.
const ORGANIZATIONS_HOME_REGION: &str = "us-east-1";

/// What the account's organization membership is, as far as the template plane cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// The account belongs to no organization.
    None,
    /// The account belongs to an organization it does not MANAGE: the organization's
    /// management account is another account, so the organization-wide object is not
    /// this caller's to read or write.
    NotManagementAccount,
    /// The account MANAGES an organization with ALL features enabled — the mode the
    /// organization-wide features of other services are served behind.
    AllFeatures,
    /// The account MANAGES an organization that enables only part of the feature set
    /// (consolidated billing).
    PartialFeatures,
}

/// The account's organization membership, as AWS Organizations reports it right now.
pub async fn membership(ctx: &smithy_server_runtime::OperationContext<'_>) -> Membership {
    let organizations = organizations_client(ctx);
    let Ok(answer) = organizations.describe_organization().send().await else {
        // Not an organization: Organizations reports the calling account as belonging to
        // none, or this emulator serves no Organizations for it to belong to one through.
        // Either way there is no organization, which is the standalone-account case.
        return Membership::None;
    };
    let Some(organization) = answer.organization else {
        return Membership::None;
    };
    // The organization-wide object belongs to the account that MANAGES the organization,
    // which is the one the organization itself records as its management account. A
    // member account that is not that one is in an organization it does not manage; an
    // organization reporting no management account at all names nobody who manages it,
    // so no caller is that account either.
    if organization.master_account_id.as_deref() != Some(caller_account_id()) {
        return Membership::NotManagementAccount;
    }
    // A membership whose feature set is not ALL is a partial one, including an
    // organization that reports no feature set: the all-features mode is only what
    // Organizations positively says it is.
    match organization.feature_set {
        Some(OrganizationFeatureSet::All) => Membership::AllFeatures,
        _ => Membership::PartialFeatures,
    }
}

/// The account every request this emulator serves is made by, which is the account a
/// membership is judged for.
fn caller_account_id() -> &'static str {
    ::engine::emulator_account_id()
}

/// An AWS Organizations client over the in-process callback.
///
/// Every request this client makes is served by the same router that is serving the call
/// being handled: the callback IS the transport, so nothing here opens a connection,
/// whatever the endpoint URL spells. The transport being in process is also why there is
/// nothing to retry or time out, and why the credentials the callback hands over are used
/// directly.
fn organizations_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
) -> ::aws_sdk_organizations::Client {
    let config = ::aws_sdk_organizations::Config::builder()
        .behavior_version(::aws_sdk_organizations::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_organizations::config::Region::new(
            ORGANIZATIONS_HOME_REGION,
        ))
        .endpoint_url(format!(
            "https://organizations.{ORGANIZATIONS_HOME_REGION}.amazonaws.com"
        ))
        .retry_config(::aws_sdk_organizations::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_organizations::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_organizations::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_organizations::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_organizations::Client::from_conf(config)
}
