// SPDX-License-Identifier: CC0-1.0

//! Whether the calling account belongs to an AWS ORGANIZATION — read out of the service that
//! owns that fact.
//!
//! `UpdateCapacityManagerOrganizationsAccess` "controls whether Capacity Manager can aggregate
//! data from all accounts in your Amazon Web Services Organization or only from the current
//! account", so turning that setting on is a statement about an organization. An organization
//! is not EC2's resource: "the account whose user is calling the CreateOrganization operation
//! automatically becomes the management account of the new organization", so the fact has ONE
//! owner and one writer, AWS Organizations.
//!
//! This service therefore keeps NO copy of it and no flag of its own: it asks AWS Organizations
//! over the in-process callback ([`smithy_server_runtime::OperationContext::callback`]) the
//! framework hands every handler for calling another emulated service, through the one
//! construction of that client this workspace has
//! ([`::service_common::organizations_client`]) — the way
//! [`super::acm_certificates`] asks Certificate Manager about a certificate an EC2 request
//! named. The transport is what makes the call in-process: no socket is opened, no name is
//! resolved, and the endpoint URL is addressing for that transport rather than a host this
//! process connects to. Nothing is cached, so an organization a client creates through
//! Organizations is immediately what this gate sees, and no state of EC2's has to be seeded for
//! an account to be in one.
//!
//! An answer that is not an organization means the account belongs to none: Organizations
//! reports `AWSOrganizationsNotInUseException` for an account that belongs to no organization,
//! and an emulator serving no Organizations control plane at all holds no organization the
//! account could belong to either. Both are the standalone-account case, where there is no
//! organization for Capacity Manager to aggregate over, so the update is refused
//! ([`super::capacity_manager_organization_trusted_access_disabled`]) rather than recording a
//! scope nothing could aggregate across.

use smithy_server_runtime::OperationContext;

/// The Region AWS Organizations' single global endpoint is addressed at.
///
/// Organizations is partition-global: it holds one organization per management account
/// whichever Region it is asked in, so this is this service's own constant rather than anything
/// read off a request — no part of a caller's request becomes a destination.
const ORGANIZATIONS_HOME_REGION: &str = super::DEFAULT_REGION;

/// Whether the calling account belongs to an AWS Organization.
///
/// A READ of another service: it asks `DescribeOrganization`, which "retrieves information
/// about the organization that the user's account belongs to" and writes nothing. `false` for
/// an account that belongs to none, and for an emulator whose Organizations control plane did
/// not answer at all — both the standalone-account case this module documents.
pub async fn account_is_in_an_organization(ctx: &OperationContext<'_>) -> bool {
    let organizations = ::service_common::organizations_client::organizations_client(
        ctx.callback(),
        ORGANIZATIONS_HOME_REGION,
    );
    organizations
        .describe_organization()
        .send()
        .await
        .is_ok_and(|answer| answer.organization.is_some())
}
