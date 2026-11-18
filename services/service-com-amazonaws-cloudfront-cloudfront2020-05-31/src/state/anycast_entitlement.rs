// SPDX-License-Identifier: CC0-1.0

//! The account's ENTITLEMENT for Anycast static IP lists.
//!
//! "Anycast static IP list" is a resource an account holds a Service Quotas quota for, and
//! that quota is what `CreateAnycastIpList` allocates against: a create that would take the
//! account past the value in force is refused with the modelled `EntityLimitExceeded`, whose
//! sentence is the instruction to have the quota RAISED
//! ([`super::ANYCAST_IP_LIST_LIMIT_EXCEEDED_MESSAGE`]) rather than anything from the request.
//!
//! The value is the ACCOUNT's, so it is read out of the store Service Quotas serves
//! `GetServiceQuota` from and records an approved `RequestServiceQuotaIncrease` into — never
//! a number this service decides. Its AWS default is ZERO
//! ([`ANYCAST_IP_LIST_DEFAULT_ENTITLEMENT`]): an account is entitled to no Anycast static IP
//! list until an increase is granted, which is exactly what the refusal tells the caller to
//! ask for. An engine with no Service Quotas store mounted publishes no quota for the
//! account to be read at, so the entitlement is that published default there too.

use std::time::SystemTime;

/// The Service Quotas `ServiceCode` the account's CloudFront quotas are published under.
const QUOTA_SERVICE_CODE: &str = "cloudfront";

/// The `QuotaCode` of the quota that bounds how many Anycast static IP lists an account may
/// hold.
///
/// SOURCE: the Service Quotas console page CloudFront's own refusal of a create directs the
/// caller to ([`super::ANYCAST_IP_LIST_LIMIT_EXCEEDED_MESSAGE`]) — the quota in that page's
/// path is the quota the refusal is about.
const QUOTA_CODE: &str = "L-6A19EDFD";

/// The Region the account's CloudFront quotas are read at. CloudFront is a global service
/// whose quotas count account-wide, so the value in force is the same wherever it is read;
/// this is the Region the console page in the refusal's sentence lives in.
const QUOTA_REGION: &str = "us-east-1";

/// The value the quota is in force at where the account cannot be asked: AWS's published
/// DEFAULT for it, which is ZERO — an account holds the entitlement only once an increase has
/// been approved.
pub const ANYCAST_IP_LIST_DEFAULT_ENTITLEMENT: usize = 0;

/// How many Anycast static IP lists this account is entitled to hold: the value its
/// "Anycast static IP lists" quota is in force at.
///
/// Read from the Service Quotas store — the one `GetServiceQuota` answers from, where an
/// approved increase is recorded — so an account that has had the quota raised may allocate
/// up to the raised value and one that has not may allocate none. The decision the clock has
/// already made on an open increase request is materialised by that service's own settle
/// first, so the entitlement read here is the one that was owed at this instant even if
/// nothing has called Service Quotas since the request came due.
///
/// A value that is not a usable count — not finite, or below zero — is no entitlement to
/// allocate against, so it reads as the published default.
pub fn anycast_ip_list_entitlement<Eng: Send + Sync + 'static>(engine: &Eng, now: SystemTime) -> usize {
    let Some(quotas) =
        ::service_com_amazonaws_servicequotas_servicequotasv20190624::state::service_state(engine)
    else {
        return ANYCAST_IP_LIST_DEFAULT_ENTITLEMENT;
    };
    let mut quotas = quotas.lock();
    quotas.settle_due_requests(now);
    let in_force = quotas.applied_value(QUOTA_REGION, QUOTA_SERVICE_CODE, QUOTA_CODE, None);
    in_force
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or(ANYCAST_IP_LIST_DEFAULT_ENTITLEMENT, |value| value as usize)
}

/// The modelled `EntityLimitExceeded` (HTTP 400) for a create the account's Anycast static IP
/// list entitlement cannot serve.
///
/// The same sentence every refusal of this entitlement carries
/// ([`super::ANYCAST_IP_LIST_LIMIT_EXCEEDED_MESSAGE`]): the entitlement is a Service Quotas
/// quota, so what the refusal states is how to have it raised rather than any value from the
/// request or from the store.
pub fn anycast_ip_list_entitlement_exceeded() -> crate::ops::types::EntityLimitExceeded {
    crate::ops::types::EntityLimitExceeded {
        message: Some(super::ANYCAST_IP_LIST_LIMIT_EXCEEDED_MESSAGE.to_string()),
    }
}
