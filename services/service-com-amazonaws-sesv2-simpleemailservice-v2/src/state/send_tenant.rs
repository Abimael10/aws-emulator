// SPDX-License-Identifier: CC0-1.0

//! The tenant a SEND names, resolved against the account's tenants.
//!
//! `SendEmailRequest$TenantName` and `SendBulkEmailRequest$TenantName` are "the name of
//! the tenant through which this email will be sent": the member NAMES a resource of the
//! account, so a send carrying one is weighed against the account's tenant table, and a
//! value naming no tenant of it addresses nothing to send through. That resolution is
//! part of reading the REQUEST's own resources, so it happens before the message is
//! weighed against the account's sending identities: a send through a tenant that is not
//! there is refused for the tenant, whatever the state of the identity it would have sent
//! from.
//!
//! Stated once here for both send paths, so the single and the bulk send cannot come to
//! disagree about which tenant a name resolves to or how an unresolvable one reads; each
//! handler wraps the sentence in the `NotFoundException` its own model binds. Every item
//! is re-exported from [`crate::state`], so a handler reaches it as `crate::state::…`.

use super::ComAmazonawsSesv2SimpleemailserviceV2State;

/// How the SEND path words `NotFoundException` for a tenant the account does not hold.
///
/// The send path names the tenant AND the account it was looked for in, because a tenant
/// name is unique within one account and a send is scoped to the caller's — the same two
/// facts the create of an already-taken name reports ([`super::tenant_already_exists`]).
/// So this is a sentence of its own rather than the one the tenant's own surface answers
/// with ([`super::tenant_not_found`]): the fixed text is `Tenant ` + ` for AwsAccountId `
/// + ` not found.`, and the variable parts are the `TenantName` as the request spelled it
/// and the caller's account id.
pub fn send_tenant_not_found(tenant_name: &str, account_id: &str) -> String {
    format!("Tenant {tenant_name} for AwsAccountId {account_id} not found.")
}

/// The refusal a send that names the tenant `tenant_name` has to answer while the account
/// `account_id` holds no tenant under that name, and `None` when it holds one — or when
/// the send names no tenant at all, which is a send of the account itself.
///
/// Reporting only: the tenant table is read and nothing is written, so the resolution
/// leaves every later caller seeing exactly what it saw.
pub fn unresolved_send_tenant(
    store: &ComAmazonawsSesv2SimpleemailserviceV2State,
    tenant_name: Option<&str>,
    account_id: &str,
) -> Option<String> {
    let name = tenant_name?;
    (!store.tenants.contains_key(name)).then(|| send_tenant_not_found(name, account_id))
}
