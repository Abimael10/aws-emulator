// SPDX-License-Identifier: CC0-1.0

//! Which account is the MANAGEMENT account of the organization the calling account
//! belongs to — read out of the service that owns that fact.
//!
//! Multi-party approval is an AWS Organizations feature: the operations that set an
//! organization's approval teams up are the ones its management account owns, and which
//! account that is belongs to AWS Organizations, not to this service. "The account whose
//! user is calling the CreateOrganization operation automatically becomes the management
//! account of the new organization", so the fact has ONE owner and one writer.
//!
//! This service therefore keeps NO copy of it: it asks AWS Organizations over the
//! in-process callback ([`smithy_server_runtime::OperationContext::callback`]) the
//! framework hands every handler for calling another emulated service, through the one
//! construction of that client this workspace has
//! ([`::service_common::organizations_client`]). The transport is what makes the call
//! in-process: no socket is opened, no name is resolved, and the endpoint URL below is
//! addressing for that transport rather than a host this process connects to. One read,
//! one owner, so an organization a client creates through Organizations is immediately
//! what this service's gate sees, and no state of this service's has to be seeded for a
//! caller to be the management account.
//!
//! An answer that is not an organization means the calling account is the management
//! account of none: Organizations reports `AWSOrganizationsNotInUseException` for an
//! account that belongs to no organization, and an emulator serving no Organizations at
//! all holds no organization the account could belong to either. Both leave every
//! organization-only operation of this service refused ([`super::not_management_account`]),
//! which is the standalone-account case.

use smithy_server_runtime::OperationContext;

/// The Region AWS Organizations' single global endpoint is addressed at.
///
/// Organizations is partition-global: it holds one organization per management account
/// whichever Region it is asked in, so this is this service's own constant rather than
/// anything read off a request.
const ORGANIZATIONS_HOME_REGION: &str = "us-east-1";

/// The account AWS Organizations reports as the management account of the organization the
/// calling account belongs to, or `None` when it belongs to none (or when no Organizations
/// control plane answers at all).
///
/// A READ of another service: it asks `DescribeOrganization`, which "retrieves information
/// about the organization that the user's account belongs to" and writes nothing.
pub async fn management_account_id(ctx: &OperationContext<'_>) -> Option<String> {
    let organizations = ::service_common::organizations_client::organizations_client(
        ctx.callback(),
        ORGANIZATIONS_HOME_REGION,
    );
    let answer = organizations.describe_organization().send().await.ok()?;
    answer.organization?.master_account_id
}

/// Whether the calling account is the management account of the organization it belongs
/// to — the one authorization decision the operations this service restricts to that
/// account are made on.
///
/// False whenever the account belongs to no organization, which is a standalone account:
/// an organization-only operation is then available to nobody.
pub async fn caller_is_management_account(ctx: &OperationContext<'_>) -> bool {
    management_account_id(ctx).await.as_deref() == Some(super::caller_account_id())
}
