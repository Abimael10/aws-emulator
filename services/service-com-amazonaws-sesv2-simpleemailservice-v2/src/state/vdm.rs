// SPDX-License-Identifier: CC0-1.0

//! The account's Virtual Deliverability Manager configuration as a GATE: whether the
//! feature is on, and how a call that can only be answered from that feature's data is
//! refused while it is off.
//!
//! Virtual Deliverability Manager is a facility the ACCOUNT switches on
//! (`PutAccountVdmAttributes`), and the data it collects exists only while it is on. An
//! operation whose answer is that data therefore has nothing to report for an account
//! that never switched it on, and one that would start work on that data has nothing for
//! that work to draw on: the condition is a missing FEATURE rather than a malformed
//! request, so it is answered with one fixed sentence telling the caller which feature
//! to enable, carried by the error each operation's own model binds for the condition —
//! the error for a resource that does not exist where the call is a read of that data,
//! and the operation's input error where the call asks for work on it.
//!
//! Stated once here, keyed by neither operation nor member, so every call this gate
//! guards reports the same condition the same way — and so the next one to need it reads
//! the switch through the same function rather than comparing the stored status itself.
//!
//! Split out of the state module root so this one concern lives in its own file; every
//! item is re-exported from [`crate::state`], so a caller still reaches it as
//! `crate::state::…`.

use super::*;

/// Whether the account's Virtual Deliverability Manager configuration is on, which is
/// what `VdmAttributes$VdmEnabled` carries.
pub fn vdm_is_enabled(account: &AccountState) -> bool {
    account.vdm_enabled == FEATURE_ENABLED
}

/// How this service words the refusal of a call it can only answer from Virtual
/// Deliverability Manager data while that feature is off.
///
/// Wholly FIXED text: what is wrong is that the feature is not on, which is a fact about
/// the account and not about anything the request carried, so the sentence names neither
/// a member nor a value. ONE wording for every operation this gate guards, whichever
/// error shape carries it, because it is one condition.
pub fn enable_vdm_to_use_this_feature() -> String {
    "To use this feature you must enable Virtual Deliverability Manager".to_string()
}
