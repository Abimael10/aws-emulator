// SPDX-License-Identifier: CC0-1.0

//! The REGION OPT-IN seam: whether the regional endpoint a request reached serves this
//! account at all.
//!
//! An account reaches a Region only once that Region is turned on for it — the Regions AWS
//! turns on for every account, plus the opt-in Regions the account has enabled. A Region
//! the account has NOT opted into serves it nothing: every request to that endpoint is
//! refused, rather than answered with an empty result, which is what lets a client tell
//! "this Region is not available to me" from "this Region holds nothing of mine". Answering
//! the second where the first is true is what makes a walk over the partition's Regions
//! report a Region the account cannot use as one it can — and then create resources there.
//!
//! Which Regions are on for an account is NOT this service's state: the Account API owns it
//! (`EnableRegion`, `DisableRegion`, `GetRegionOptStatus`), so the status is read from that
//! service's store on the shared engine — the same place `GetRegionOptStatus` answers from —
//! exactly as a table's streaming destination is resolved against the Kinesis registry
//! ([`super::kinesis_stream_exists`]). A Region enabled through that API therefore serves
//! this service's requests from the moment its opt-in settles, and one disabled again stops
//! serving them, without a second copy of the opt-in being kept here.

use super::*;

/// Whether a request addressed to `region` reaches this account's DynamoDB at all, as of
/// `at`.
///
/// A Region is open to the account when the Account API reports it `ENABLED_BY_DEFAULT` or
/// `ENABLED`: those are the two statuses of a Region an account can use. An opt-in whose
/// transition has not settled yet (`ENABLING`), an opt-out in flight (`DISABLING`) and a
/// Region never opted into (`DISABLED`) are all Regions the account cannot reach yet or any
/// more, so the endpoint refuses them.
///
/// Opt-in is a fact of a REGION, and only a Region the Account API's published catalog holds
/// carries one: a well-formed code naming no Region of the catalog has no opt-in status to
/// refuse on, so it is not closed here — the credential scope and the endpoint host are
/// caller-controlled text ([`super::request_region`]), and this gate answers what AWS
/// publishes about Regions rather than reading authorization out of that text.
///
/// `false` for an opt-in Region whose status cannot be read at all (the account store holds
/// no record for this account, or the engine is not the shared in-memory one): nothing has
/// opted the Region in, which is the status the catalog itself gives it.
///
/// A READ throughout: the status is settled from the clock by a pure function of the stored
/// record ([`service_com_amazonaws_account_account::state::RegionOptState::settled`]), so
/// gating a request writes nothing and a later reader of that store sees exactly what it
/// would have seen.
///
/// LOCKING: this takes the ACCOUNT store's lock, and nothing of this service's own, so it is
/// called before a handler takes ours. The work under that lock is one lookup.
pub fn region_is_enabled_for_account<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    at: SystemTime,
) -> bool {
    use ::service_com_amazonaws_account_account as account;
    use account::state::{RegionKind, RegionOptState};

    let Some(kind) = account::state::region_kind(region) else {
        return true;
    };
    if kind == RegionKind::EnabledByDefault {
        return true;
    }
    let settled = account::state::service_state(engine).and_then(|store| {
        let store = store.lock();
        store
            .account(::engine::emulator_account_id())
            .map(|record| record.region_opt_state(region, kind, at))
    });
    matches!(
        settled,
        Some(RegionOptState::EnabledByDefault | RegionOptState::Enabled)
    )
}
