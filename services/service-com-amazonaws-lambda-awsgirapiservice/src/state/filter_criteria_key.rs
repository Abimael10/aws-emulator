// SPDX-License-Identifier: CC0-1.0

//! The customer managed key an event source mapping's FILTER CRITERIA are sealed under, and
//! whether that key's own policy lets THIS service open them.
//!
//! "The ARN of the Key Management Service (KMS) customer managed key that Lambda uses to
//! encrypt your function's filter criteria. By default, Lambda does not encrypt your filter
//! criteria object" (`CreateEventSourceMappingRequest$KMSKeyArn`), so a mapping that names one
//! has its criteria sealed under that key and opened again under it whenever a record is
//! matched against them or they are reported in plaintext — by LAMBDA itself, as its own
//! service principal acting for that mapping, and not by the caller that configured it. Such a
//! principal is no identity of the account and holds no identity-based policies, so the KEY's
//! own policy is the whole authority over the use: a key whose policy does not admit this
//! service admits it nothing, whatever the configuring caller may do.
//!
//! That is why a configuration naming such a key is REFUSED rather than stored. Criteria sealed
//! under a key this service cannot open could never be matched against a record, so the failure
//! would surface as silent non-delivery later — or never — instead of at the call that asked for
//! it.
//!
//! The decision is KMS's OWN ([`kms_authorization::authorize_use`]) over the key record that
//! service's handlers wrote, so `PutKeyPolicy` moves it with nothing else changed and the two
//! services cannot disagree about what a key policy says. It is the same reading of a key
//! policy every other service in this tree takes of a key it uses for one of its own resources;
//! what differs is the key, the resource it is used for and the moment the question is asked.
//!
//! The emulator refuses only what it can SHOW, so each of these establishes nothing and lets
//! the configuration through: a mapping whose criteria carry no filter at all, which seals
//! nothing under the key; an engine carrying no KMS store beside this service; an identifier
//! that store holds no key for, which may name a key of another account or Region; a key the
//! account does not manage itself, whose policy AWS manages for the service that owns it; and a
//! key that cannot be used in a cryptographic operation at all, which is a different failure
//! from a permission the key withholds.

use ::service_com_amazonaws_kms_trentservice::state as kms_store;
use ::service_com_amazonaws_kms_trentservice::state::authorization as kms_authorization;

use crate::ops::types::FilterCriteria;

/// This service's own principal, as AWS publishes it and as a key policy's `Service` element
/// names it.
pub const SERVICE_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The DNS zone every service principal is written under, which is what the REGIONAL spelling
/// of a principal carries its Region in front of ([`regional_service_principal`]).
const SERVICE_PRINCIPAL_ZONE: &str = "amazonaws.com";

/// The KMS operation this service performs on such a key, and the one the refusal below names:
/// the criteria are read back out of their sealed form before a record can be matched against
/// them, which is the `kms:Decrypt` the plaintext of an encrypted mapping's criteria is
/// reported "if you have kms:Decrypt permissions for the correct KMS key"
/// (`EventSourceMappingConfiguration$FilterCriteria`).
const FILTER_CRITERIA_KEY_OPERATION: &str = "Decrypt";

/// How Lambda words the refusal of a configuration whose key withholds that use from this
/// service: that the service principal was denied the decrypt of the caller's own filter
/// criteria, and which document the caller fixes it in.
///
/// Fixed text with NO variable part — neither the key, nor the mapping, nor the function is in
/// it — because the document that has to change is the key policy the caller already named the
/// key by.
pub const FILTER_CRITERIA_KEY_DENIED_MESSAGE: &str =
    "Lambda service principal was denied access to decrypt your filter criteria. Please check \
     your KMS key policy";

/// The regional spelling of this service's principal for `region`:
/// `lambda.<region>.amazonaws.com`.
///
/// AWS publishes a service's principal both without a Region and with the Region the use is
/// made in, and both name the same service, so a key policy written in either spelling names
/// the principal a use of the key is made for.
#[must_use]
pub fn regional_service_principal(region: &str) -> String {
    format!("lambda.{region}.{SERVICE_PRINCIPAL_ZONE}")
}

/// The endpoint of this service in `region`, as a key policy's `kms:ViaService` key spells it —
/// the value a statement scoped to this service's use of the key is written against.
#[must_use]
fn via_service(region: &str) -> String {
    regional_service_principal(region)
}

/// Why the key `kms_key_arn` names does not let this service decrypt the filter criteria of the
/// mapping `mapping_arn`, or `None` when it does — and for every case the module docs record as
/// establishing nothing.
///
/// `criteria` are the criteria in force after the call: a mapping whose criteria carry no filter
/// seals nothing under the key, so there is nothing to decrypt and nothing to ask about — the
/// same reading of criteria that carry no filter every other writer of them makes
/// ([`super::event_source_filter::patterns_of`]).
///
/// The use is one this service makes for its own resource rather than one the configuring caller
/// asked KMS for, so it is judged FOR this service's own principal acting for the mapping
/// ([`kms_authorization::Caller::service_principal`], which is what gives a statement scoped by
/// `aws:SourceArn` or `aws:SourceAccount` its values) and as arriving THROUGH this service
/// ([`kms_authorization::RequestedUse::through_service`], which is what a statement scoped by
/// `kms:ViaService` is written against).
///
/// A READ of the key store: it decides, and writes nothing — in KMS or here.
#[must_use]
pub fn filter_criteria_key_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    request_region: &str,
    kms_key_arn: Option<&str>,
    criteria: Option<&FilterCriteria>,
    mapping_arn: &str,
) -> Option<&'static str> {
    // `KMSKeyArn`'s own pattern admits the empty string beside a key ARN, and a mapping
    // carrying it is a mapping whose criteria are sealed under no customer managed key: there
    // is nothing to ask the key service about.
    let key_arn = kms_key_arn.filter(|arn| !arn.is_empty())?;
    if super::event_source_filter::patterns_of(criteria).is_empty() {
        return None;
    }
    let Some(kms) = kms_store::service_state(engine) else {
        // Visible to an operator: a lookup nothing answered lets the configuration through, so
        // without this line it would be indistinguishable from a key whose policy KMS said
        // admits this service.
        ::tracing::warn!(
            "no KMS service is mounted beside Lambda; a mapping's filter-criteria key is accepted"
        );
        return None;
    };
    // A key named by an ARN lives in the Region that ARN names, because that is the endpoint
    // holding it; the Region the call was signed for is where anything else would live. The
    // value is read as a Region LABEL first, so caller-supplied text cannot name something
    // else.
    let key_region = ::service_common::arn::parse(key_arn)
        .and_then(|arn| ::service_common::arn::region_label(arn.region))
        .unwrap_or(request_region);
    let mut caller = kms_authorization::Caller::service_principal(SERVICE_PRINCIPAL, mapping_arn);
    // Both published spellings of this service's principal name it (see
    // [`regional_service_principal`]), so a statement written in either is about this use.
    caller
        .principals
        .push(regional_service_principal(request_region));
    let via_service = via_service(request_region);
    let store = kms.lock();
    let kms_store::KeyRef::Found(key) = store.resolve(key_region, key_arn) else {
        return None;
    };
    if key.key_manager != kms_store::KEY_MANAGER_CUSTOMER || !key.is_usable() {
        return None;
    }
    // The key's own refusal is not relayed as KMS worded it: the DECISION is what KMS answers,
    // and the sentence is this service's own ([`FILTER_CRITERIA_KEY_DENIED_MESSAGE`]), because
    // the caller asked Lambda for a mapping rather than asking KMS for a decrypt.
    kms_authorization::authorize_use(
        &caller,
        &store,
        key_region,
        key,
        kms_authorization::RequestedUse::through_service(
            FILTER_CRITERIA_KEY_OPERATION,
            &via_service,
        ),
        // Lambda presents no encryption context for a mapping's criteria, so a grant
        // constrained to one does not permit this use: the authority it carries is not claimed
        // rather than assumed.
        None,
        None,
    )
    .err()
    .map(|_| FILTER_CRITERIA_KEY_DENIED_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: both published spellings of this service's principal name this service, so the
    /// regional one is the Region's label between the service name and the zone AWS publishes
    /// every service principal under.
    #[test]
    fn the_regional_principal_carries_the_region_of_the_use() {
        // One row per Region shape a published principal is written for: a commercial Region,
        // a GovCloud Region and an ISO Region (source: `service_common::arn::region_label`,
        // the tree's one reading of a Region label).
        let rows = ["us-west-2", "eu-central-1", "us-gov-east-1", "us-iso-east-1"];
        for region in rows {
            let spelled = regional_service_principal(region);
            let expected = format!("lambda.{region}.amazonaws.com");
            assert_eq!(
                spelled, expected,
                "case {region:?}: expected {expected:?}, got {spelled:?}"
            );
            // The via-service value a key policy scopes this service's use by is that same
            // regional endpoint name.
            let arrival = via_service(region);
            assert_eq!(
                arrival, expected,
                "case {region:?}: expected the via-service {expected:?}, got {arrival:?}"
            );
        }
    }
}
