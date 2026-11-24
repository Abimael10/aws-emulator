// SPDX-License-Identifier: CC0-1.0

//! Whether a DOMAIN identity's Easy DKIM records are PUBLISHED, resolved against the
//! account's own DNS — and what finding them does to the verification the identity is
//! waiting on.
//!
//! "Starts the process of verifying an email identity": for a domain, `CreateEmailIdentity`
//! answers the Easy DKIM tokens its owner has to publish as CNAME records in that domain's
//! DNS, and Amazon SES completes the verification only once it FINDS those records there —
//! it keeps looking for them for the documented [`DKIM_RECORD_SEARCH_WINDOW`] and reports
//! the identity `PENDING`, with `VerifiedForSendingStatus` false, until a search succeeds.
//! A domain whose owner publishes nothing therefore reads back unverified however many
//! reads are made, which is what makes the flag carry information at all.
//!
//! Whether the records are published is the DNS service's fact, not this one's, so it is
//! READ from the Amazon Route 53 store in the shared engine: the zone that answers for the
//! record name has to carry a CNAME at `<token>._domainkey.<domain>` whose value is the
//! `<token>.dkim.amazonses.com` SES publishes the key at, for EVERY token of the setup.
//! Nothing is copied onto the identity except WHEN the search first found them, so a zone
//! that stops publishing a token stops proving the domain. This is the same shape
//! Certificate Manager resolves a certificate's DNS validation in
//! (`service_com_amazonaws_acm_certificatemanager::state::dns_validation`), for the same
//! reason: the evidence belongs to the account's DNS.
//!
//! The transition itself is OWED, never slept on, exactly as every other transition this
//! store carries: the pass that first observes the published records brings the identity's
//! verification due instant forward to [`super::VERIFICATION_SETTLE`] out, and whichever
//! call next touches the store materialises it ([`super::EmailIdentityRecord::settle`]).
//! That span is never zero, so a caller that publishes the records and reads straight
//! afterwards still sees `PENDING` and the DKIM state it is waiting on.
//!
//! WHY a read may write this: the search is SES's own, re-run on a schedule of its own
//! ([`super::VERIFICATION_PROBE_INTERVAL`], whose instant every identity already carries),
//! so a search whose records were there was owed at that instant whether or not anybody
//! called. Resolving it before an operation reads the table therefore changes nothing a
//! later caller would otherwise have seen — it only materialises what the clock had already
//! made due — which is the same reason this store settles its other owed transitions on a
//! read.
//!
//! LOCK ORDER: this service's own store first, the DNS store under it — the order every
//! other reader of another service's store in this crate takes.
//!
//! Split out of the state module root so this one concern lives in its own file; every
//! item is re-exported from [`crate::state`], so a caller still reaches it as
//! `crate::state::…`.

use super::*;

use ::service_com_amazonaws_route53_awsdnsv20130401::state as dns_store;

/// The DNS service's store, which holds the account's hosted zones.
type Zones = dns_store::ComAmazonawsRoute53Awsdnsv20130401State;

/// How long Amazon SES keeps looking for the DKIM records a domain identity's owner was
/// told to publish, and so how long after the create the verification move the clock owes
/// such an identity falls due while they have NOT been found.
///
/// The span is the documented one: "if the required DNS records aren't detected within 72
/// hours", the setup is not complete and the owner starts over. Those records are the
/// account's to publish and this emulator's to OBSERVE ([`dkim_records_published`]), never
/// to invent, so while they are not there the move stays owed at the far end of that window
/// rather than falling due after the short span a transition nothing is waiting on falls due
/// after. The instant is STORED on the record, so a publication brings it forward
/// ([`resolve_dkim_publication`]) and nothing ever sleeps for it.
pub const DKIM_RECORD_SEARCH_WINDOW: Duration = Duration::from_secs(72 * 60 * 60);

/// The record type an Easy DKIM token is published as: a `CNAME`, as the setup instructions
/// state ("add the three CNAME records to your domain's DNS configuration").
const DKIM_RECORD_TYPE: &str = "CNAME";

/// The label the DKIM key records of a domain sit under, from the DKIM standard's
/// `_domainkey` subdomain, which is where a verifier looks a selector up.
const DKIM_KEY_LABEL: &str = "_domainkey";

/// The host an Easy DKIM token's CNAME points AT: SES publishes the public key for the
/// token under this name, so the record's value is `<token>.dkim.amazonses.com`.
const DKIM_PUBLICATION_HOST: &str = "dkim.amazonses.com";

/// Bring every identity waiting on its published DKIM records into agreement with the
/// account's DNS at `now`, and carry a verification this surface has moved into the
/// account's identity registry.
///
/// Called by [`super::service_state`], so every operation of this service — a read, a
/// write and a send alike — resolves the evidence before it reads the identity table, and
/// no two of them can disagree about whether a domain is verified.
///
/// An engine carrying no DNS store resolves nothing: no zone exists there, so no record is
/// published and every waiting identity keeps the verification instant it already owed.
///
/// The observation moves in BOTH directions, because the records are read rather than
/// remembered: a setup whose records are all published is verified
/// [`super::VERIFICATION_SETTLE`] later, and records withdrawn before that instant falls
/// due put the identity back to waiting the window out. Only an identity whose records this
/// pass had FOUND is moved back, so a verification the account completed another way is
/// left exactly as it stands.
pub(crate) fn resolve_dkim_publication<Eng: Send + Sync + 'static>(
    state: &::engine::ServiceState<ComAmazonawsSesv2SimpleemailserviceV2State>,
    engine: &Eng,
    now: SystemTime,
) {
    observe_published_records(state, engine, now);
    publish_verification_moves(state, engine);
}

/// Bring the verification instant of every identity waiting on its DKIM records into
/// agreement with what the account's DNS carries.
fn observe_published_records<Eng: Send + Sync + 'static>(
    state: &::engine::ServiceState<ComAmazonawsSesv2SimpleemailserviceV2State>,
    engine: &Eng,
    now: SystemTime,
) {
    {
        // Nothing is waiting on DNS, so there is nothing to read it for: the common case
        // costs one pass over the identity table and no second lock.
        let store = state.lock();
        if !store
            .identities
            .values()
            .any(EmailIdentityRecord::awaits_dkim_publication)
        {
            return;
        }
    }
    let Some(zones) = dns_store::service_state(engine) else {
        return;
    };
    let mut store = state.lock();
    let zones = zones.lock();
    for record in store.identities.values_mut() {
        if !record.awaits_dkim_publication() {
            continue;
        }
        let published = dkim_records_published(&zones, record);
        match (published, record.dkim_records_found_at) {
            (true, None) => {
                record.dkim_records_found_at = Some(now);
                record.verification_due_at = now.checked_add(VERIFICATION_SETTLE).unwrap_or(now);
            }
            (false, Some(_)) => {
                record.dkim_records_found_at = None;
                record.verification_due_at = record
                    .created_at
                    .checked_add(DKIM_RECORD_SEARCH_WINDOW)
                    .unwrap_or(now);
            }
            _ => {}
        }
    }
}

/// Publish to the account's identity registry every identity whose VERIFICATION this
/// service's table holds differently from the registry's own record of it.
///
/// The registry is the account's one set of identities, and both SES surfaces decide a
/// verification from the instant it carries — so an identity whose move this surface has
/// brought FORWARD (its records are published) or has moved out (they are gone) has to
/// carry the new instant there as well, or the other surface would go on reporting the
/// state the registry still held and the two would disagree about one identity of the
/// account. Nothing else of the record travels: the registry's publication keeps the
/// spelling, the class and the verification token it was recorded with.
///
/// Idempotent and allocation-free while the two sides agree: an identity the registry does
/// not carry yet is published by the reconciliation instead ([`super::reconcile_identities`]).
///
/// LOCK ORDER: this service's own store, then the registry — the order every writer of the
/// registry in this crate takes.
fn publish_verification_moves<Eng: Send + Sync + 'static>(
    state: &::engine::ServiceState<ComAmazonawsSesv2SimpleemailserviceV2State>,
    engine: &Eng,
) {
    let Some(registry) = identity_registry(engine) else {
        return;
    };
    let store = state.lock();
    let mut registry = registry.lock();
    let moved: Vec<::service_common::ses_identities::SesIdentityRecord> = store
        .identities
        .values()
        .filter(|record| record.registered)
        .map(registry_record)
        .filter(|published| {
            registry.get(&published.name).is_some_and(|held| {
                held.verification != published.verification
                    || held.verification_due != published.verification_due
            })
        })
        .collect();
    for record in moved {
        registry.publish(record);
    }
}

/// Whether EVERY Easy DKIM token of `record` is published in the account's DNS as the
/// record SES told its owner to add.
///
/// "You must publish all three CNAME records": one token missing leaves the setup
/// incomplete, so the domain is not proven. The name, the type and the value all have to
/// match — a CNAME at the right name pointing somewhere else proves nothing — and names
/// are compared in the DNS service's own canonical form, so the trailing dot and the letter
/// case a publisher wrote them with do not decide it.
fn dkim_records_published(zones: &Zones, record: &EmailIdentityRecord) -> bool {
    let domain = domain_key(&record.name);
    record.dkim.tokens.iter().all(|token| {
        let name = format!("{token}.{DKIM_KEY_LABEL}.{domain}");
        let value = dns_store::canonical_dns_name(&format!("{token}.{DKIM_PUBLICATION_HOST}"));
        authoritative_zone_for(&name, zones).is_some_and(|zone| {
            zone.records_at(&name, DKIM_RECORD_TYPE)
                .iter()
                .any(|published| {
                    published
                        .resource_records
                        .iter()
                        .flatten()
                        .any(|data| dns_store::canonical_dns_name(&data.value) == value)
                })
        })
    })
}

/// The hosted zone of the account a reader of `name` is answered from: the MOST SPECIFIC
/// public zone covering the name.
///
/// A private zone is "visible only in the VPCs that you specify"
/// (`HostedZoneConfig$PrivateZone`), so nothing SES looks up could be answered from one.
/// Among the public zones the deepest one covering the name is the authority for it, which
/// is where its records are published; which names one zone covers is the DNS service's own
/// rule (`name_is_in_zone`), so the comparison of a name to a zone apex stays single-homed
/// in the crate that owns it.
fn authoritative_zone_for<'zones>(
    name: &str,
    zones: &'zones Zones,
) -> Option<&'zones dns_store::StoredZone> {
    zones
        .zones
        .values()
        .filter(|zone| !zone.private_zone && dns_store::name_is_in_zone(name, &zone.name))
        .max_by_key(|zone| labels_of(&zone.name))
}

/// How many labels `name` has, in the canonical form the DNS service compares names in, so
/// the trailing dot and the letter case a zone was created with do not change its depth.
fn labels_of(name: &str) -> usize {
    dns_store::canonical_dns_name(name)
        .split('.')
        .filter(|label| !label.is_empty())
        .count()
}

impl EmailIdentityRecord {
    /// Whether this identity's verification is waiting on DKIM records that the account's
    /// DNS has to carry before SES can complete it.
    ///
    /// A DOMAIN identity whose Easy DKIM setup SES generated ([`DkimOrigin::AwsSes`] with a
    /// token set) and whose verification is still owed its move: only such an identity is
    /// proven by a published record. An address identity is verified by the recipient of
    /// the message SES sends to it, a domain whose keys the CALLER owns (BYODKIM) publishes
    /// a record this service holds no public key to compare, and a verification that has
    /// completed or that nothing owes a move to is waiting on nothing at all.
    pub fn awaits_dkim_publication(&self) -> bool {
        self.identity_kind == IdentityKind::Domain
            && self.verification_status == SesStatus::Pending
            && self.verification_move_owed
            && self.dkim.origin == DkimOrigin::AwsSes
            && !self.dkim.tokens.is_empty()
    }
}
