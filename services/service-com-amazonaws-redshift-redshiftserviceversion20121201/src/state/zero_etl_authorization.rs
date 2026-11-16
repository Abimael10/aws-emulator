// SPDX-License-Identifier: CC0-1.0

//! Whether the data warehouse a zero-ETL `CreateIntegration` replicates INTO
//! AUTHORIZES that integration — the decision the target's own resource policy
//! makes.
//!
//! A zero-ETL integration is written by the caller but READ by the target data
//! warehouse, so the target is what grants it: the account attaches a resource
//! policy to the data warehouse (`PutResourcePolicy`, whose `ResourceArn` is the
//! warehouse's namespace ARN) that names the principal allowed to create an
//! inbound integration into it and the source it may be created from. The create
//! is therefore an authorization decision taken SYNCHRONOUSLY, before anything is
//! recorded, and a warehouse that does not grant it is refused with the one
//! sentence that says nothing about which half was missing — the warehouse may
//! not exist, or it may exist and grant the caller nothing
//! ([`super::ZERO_ETL_DATA_WAREHOUSE_ACCESS`]). A warehouse carrying NO resource
//! policy grants nothing, so it authorizes no integration either.
//!
//! The grant is a PAIR, because two different principals act: the CALLER asks for
//! the integration ([`CREATE_INBOUND_INTEGRATION`]) and Amazon Redshift itself
//! then admits the inbound flow from that source on the caller's behalf
//! ([`AUTHORIZE_INBOUND_INTEGRATION`], granted to
//! [`REDSHIFT_SERVICE_PRINCIPAL`]). Both halves have to be stated for the
//! integration to be authorized, and each is narrowed by the source the warehouse
//! accepts a flow from — which a policy names through the global source condition
//! keys ([`SOURCE_ARN_KEY`], [`SOURCE_ACCOUNT_KEY`]), so both are put in the
//! request's condition context and a policy that admits only another table or
//! bucket decides against this request rather than being left undecided.
//!
//! The documents are evaluated by the workspace's single IAM evaluation
//! ([`::service_common::iam_authorization`]), so this service answers about a
//! policy the way every other emulated service answers about one, and nothing
//! about IAM's rules is restated here.

/// The IAM action the CALLER performs on the target data warehouse when it asks
/// for a zero-ETL integration into it, as IAM publishes it under the Amazon
/// Redshift action namespace.
pub const CREATE_INBOUND_INTEGRATION: &str = "redshift:CreateInboundIntegration";

/// The IAM action Amazon Redshift performs on the target data warehouse to admit
/// the inbound flow from one source, which is why a target policy grants it to
/// [`REDSHIFT_SERVICE_PRINCIPAL`] rather than to a user.
pub const AUTHORIZE_INBOUND_INTEGRATION: &str = "redshift:AuthorizeInboundIntegration";

/// The service principal that second grant names: Amazon Redshift acting for the
/// caller, the same spelling a service principal takes everywhere in IAM.
pub const REDSHIFT_SERVICE_PRINCIPAL: &str = "redshift.amazonaws.com";

/// The global condition key a target policy names the ONE source it accepts an
/// inbound integration from under — its value is the request's own `SourceArn`.
pub const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The global condition key a target policy names the account that OWNS that
/// source under — the companion of [`SOURCE_ARN_KEY`], and the one a policy uses
/// when it accepts any source of one account.
pub const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";

/// The account that owns the resource `source_arn` names.
///
/// Read off the ARN's own account field, and this emulator's account when the ARN
/// carries none: an Amazon S3 bucket ARN has an empty account field
/// (`arn:aws:s3:::bucket`), and the bucket it names is one this emulator's single
/// account holds.
fn source_account(source_arn: &str) -> String {
    ::service_common::arn::parse(source_arn)
        .map(|arn| arn.account.to_string())
        .filter(|account| !account.is_empty())
        .unwrap_or_else(|| ::engine::emulator_account_id().to_string())
}

/// Whether the target data warehouse authorizes this account's caller to create a
/// zero-ETL integration into `target_arn` from `source_arn`.
///
/// `policy` is the resource policy attached to that warehouse, or `None` when it
/// carries none — which authorizes nothing, because the grant a zero-ETL
/// integration needs can only come from there.
///
/// Both halves of the grant have to be stated (see the module docs), and each is
/// decided by the one shared IAM evaluation: a resource policy grants only what an
/// `Allow` about the request says, an explicit `Deny` about either half refuses,
/// and a policy that says nothing about the request grants nothing.
pub fn integration_is_authorized(
    policy: Option<&str>,
    target_arn: &str,
    source_arn: &str,
) -> bool {
    use ::service_common::iam_authorization::{resource_decision, Decision, Request};

    let Some(policy) = policy else {
        return false;
    };

    let caller = super::identity_center::caller_principal();
    // Every spelling of this account's caller a resource policy may name: the
    // principal's own ARN, and the account spellings
    // ([`::service_common::iam_authorization::account_principals`]) a policy
    // written for the whole account uses.
    let mut caller_principals = vec![caller.clone()];
    caller_principals.extend(::service_common::iam_authorization::account_principals(
        &caller,
    ));
    // The source the integration would read from, under both keys a target policy
    // narrows its grants with, so a clause over either of them is DECIDED rather
    // than left undecided — and a policy that names another source decides
    // against this request.
    let context = [
        (SOURCE_ARN_KEY.to_string(), source_arn.to_string()),
        (SOURCE_ACCOUNT_KEY.to_string(), source_account(source_arn)),
    ];
    let keys = [SOURCE_ARN_KEY, SOURCE_ACCOUNT_KEY];

    let create = Request::new(CREATE_INBOUND_INTEGRATION, target_arn, &caller_principals)
        .with_context(&context)
        .with_published_keys(&keys);
    let service_principal = [REDSHIFT_SERVICE_PRINCIPAL.to_string()];
    let authorize = Request::new(
        AUTHORIZE_INBOUND_INTEGRATION,
        target_arn,
        &service_principal,
    )
    .with_context(&context)
    .with_published_keys(&keys);

    resource_decision(policy, &create) == Decision::Allowed
        && resource_decision(policy, &authorize) == Decision::Allowed
}
