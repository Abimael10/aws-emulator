// SPDX-License-Identifier: CC0-1.0

//! Whether the ACCOUNT may select a Job engine version at all.
//!
//! Job engine version selection is an account CAPABILITY, not a resource: an account has
//! access to it or it has not, and access is granted out of band — no operation of this
//! API carries it, and nothing a caller sends can turn it on. So the capability is one
//! flag on the account's state, and the door onto the version catalogue reads it before
//! it reads the catalogue: an account without access is answered the modelled
//! `ForbiddenException` ("You don't have permissions for this action with the credentials
//! you sent") with the fixed sentence below, so the feature detection a client does by
//! calling that door — a 200 means "this account may pin a Job engine version" — gets the
//! account's real answer rather than an unconditional success.
//!
//! An account comes up WITHOUT the capability ([`super::ComAmazonawsMediaconvertMediaconvertState::default`]),
//! because that is the state of an account nobody has been granted it for; a deployment
//! or a test that needs an account that has it records that through
//! [`ComAmazonawsMediaconvertMediaconvertState::set_version_selection_access`], the one
//! way the flag moves.

use super::ComAmazonawsMediaconvertMediaconvertState;

/// How MediaConvert refuses a request onto Job engine version selection from an account
/// that does not have access to it.
///
/// ENTIRELY fixed text: it names no request value and no stored value — which account
/// asked and under which request id is what the error envelope reports — so nothing about
/// the request can change a byte of it. It states the remediation the capability actually
/// has, which is to ask AWS Support for access.
pub const VERSION_SELECTION_FORBIDDEN_MESSAGE: &str =
    "Your account does not have access to MediaConvert version selection. For more \
     information, or to request access, contact AWS Support.";

impl ComAmazonawsMediaconvertMediaconvertState {
    /// Whether this account has access to Job engine version selection.
    ///
    /// What every door onto the Job engine version catalogue is gated on, so the answer
    /// cannot differ between two of them.
    pub fn version_selection_access(&self) -> bool {
        self.version_selection_access
    }

    /// Record whether this account has access to Job engine version selection.
    ///
    /// The grant is made out of band — no modelled operation of this service carries it —
    /// so this is the one way the flag moves, exactly as it is the one way the account's
    /// settle spans are pinned.
    pub fn set_version_selection_access(&mut self, allowed: bool) {
        self.version_selection_access = allowed;
    }
}
