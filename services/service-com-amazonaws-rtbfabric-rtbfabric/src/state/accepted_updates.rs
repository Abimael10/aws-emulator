// SPDX-License-Identifier: CC0-1.0

//! The `@idempotencyToken`s a stored resource's accepted configuration changes
//! were presented under.
//!
//! An update mints nothing, so there is no id for
//! [`ComAmazonawsRtbfabricRtbfabricState::idempotency`](super::ComAmazonawsRtbfabricRtbfabricState::idempotency)
//! to answer a replay with: the record of a change lives ON the resource that took
//! it, and the resource's own answer is what a replay reports. A resource keeps
//! EVERY token it has accepted, not only the one it took last, so a retry of an
//! earlier call is still recognised once another change has been accepted in
//! between — which is what makes retrying safe at all.
//!
//! Every record is fixed size — two 32-byte digests — and a resource retains at
//! most [`ACCEPTED_UPDATES_RETAINED`] of them, so neither a caller-chosen
//! `clientToken` nor an endless stream of fresh ones grows the store without end.

use std::collections::{HashMap, VecDeque};

/// How many accepted `@idempotencyToken` records ONE resource retains.
///
/// AWS publishes no retention window for an RTB Fabric `clientToken`, so this
/// bound is the emulator's own: it is what stops a caller growing the store by
/// presenting fresh tokens to one resource for ever. It sits far above the
/// retries of any workflow an emulator serves; once a resource has taken this
/// many further changes, the oldest record is let go of and a call under that
/// token is a request of its own again, exactly as one presented after a service's
/// retention window has passed is.
pub const ACCEPTED_UPDATES_RETAINED: usize = 64;

/// What a call presenting an `@idempotencyToken` to a resource is answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// The resource retains no record of this token: the call is a request of its
    /// own, to be applied.
    Fresh,
    /// The same token carrying the same parameters: the change it asks for has
    /// already been applied, so the caller is answered from the record rather
    /// than the change being applied a second time.
    Replay,
    /// The same token carrying DIFFERENT parameters: refused, rather than
    /// answering for parameters the caller did not send.
    Mismatch,
}

/// Every `@idempotencyToken` one resource's accepted changes were presented
/// under, each with a digest of that call's parameters, oldest first.
///
/// Insertion-ordered and bounded by [`ACCEPTED_UPDATES_RETAINED`]: the hash map
/// answers a retry without scanning, and the queue beside it is what says which
/// record is let go of when the bound is reached.
#[derive(Debug, Default, Clone)]
pub struct AcceptedUpdates {
    /// Keyed by the token digest an operation presents, holding that call's
    /// parameter fingerprint.
    fingerprints: HashMap<[u8; 32], [u8; 32]>,
    /// The same keys in the order they were accepted, oldest first.
    accepted: VecDeque<[u8; 32]>,
}

impl AcceptedUpdates {
    /// How the call presenting `key` with the parameters `fingerprint` digests is
    /// to be answered.
    pub fn classify(&self, key: &[u8; 32], fingerprint: &[u8; 32]) -> Retry {
        match self.fingerprints.get(key) {
            None => Retry::Fresh,
            Some(accepted) if accepted == fingerprint => Retry::Replay,
            Some(_) => Retry::Mismatch,
        }
    }

    /// Remember that a change presented under `key` with the parameters
    /// `fingerprint` digests has been accepted.
    ///
    /// Letting go of the oldest record once the bound is reached is what keeps one
    /// resource's retained records bounded while every token it still retains is
    /// recognised.
    pub fn accept(&mut self, key: [u8; 32], fingerprint: [u8; 32]) {
        if self.fingerprints.insert(key, fingerprint).is_none() {
            self.accepted.push_back(key);
        }
        while self.accepted.len() > ACCEPTED_UPDATES_RETAINED {
            if let Some(oldest) = self.accepted.pop_front() {
                self.fingerprints.remove(&oldest);
            }
        }
    }

    /// How many records this resource retains. Test-only: a handler reads the
    /// records through [`classify`](Self::classify) alone, so this is what pins
    /// the retention bound rather than part of the surface a handler uses.
    #[cfg(test)]
    fn retained(&self) -> usize {
        self.fingerprints.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{AcceptedUpdates, Retry, ACCEPTED_UPDATES_RETAINED};

    /// A token digest, as an operation's key for the `n`th token presented.
    fn key(n: usize) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&(n as u64).to_be_bytes());
        key
    }

    /// A parameter fingerprint, distinct per `n`.
    fn fingerprint(n: usize) -> [u8; 32] {
        let mut fingerprint = [0xffu8; 32];
        fingerprint[..8].copy_from_slice(&(n as u64).to_be_bytes());
        fingerprint
    }

    /// Every token a resource retains is recognised, whichever order the changes
    /// were accepted in and however many were accepted after it.
    #[test]
    fn every_retained_token_is_recognised_not_only_the_one_accepted_last() {
        for (case, accepted) in [
            ("one accepted change", 1usize),
            ("two accepted changes", 2),
            ("several accepted changes", 7),
            ("as many as the resource retains", ACCEPTED_UPDATES_RETAINED),
        ] {
            let mut records = AcceptedUpdates::default();
            for n in 0..accepted {
                records.accept(key(n), fingerprint(n));
            }
            assert_eq!(
                records.retained(),
                accepted,
                "case {case:?}: expected {accepted} retained record(s), got {}",
                records.retained()
            );
            // Asked for in the reverse of the order they were accepted in, so the
            // answer cannot come from a record of the latest change alone.
            for n in (0..accepted).rev() {
                assert_eq!(
                    records.classify(&key(n), &fingerprint(n)),
                    Retry::Replay,
                    "case {case:?}: expected token {n} to be replayed"
                );
                assert_eq!(
                    records.classify(&key(n), &fingerprint(accepted + 1)),
                    Retry::Mismatch,
                    "case {case:?}: expected token {n} with other parameters to be refused"
                );
            }
            assert_eq!(
                records.classify(&key(accepted), &fingerprint(accepted)),
                Retry::Fresh,
                "case {case:?}: expected a token never presented to be a request of its own"
            );
        }
    }

    /// A resource retains at most the bound, letting go of the oldest record
    /// first, and a second accepted change under one token replaces its record
    /// rather than adding one.
    #[test]
    fn the_records_one_resource_retains_are_bounded() {
        let mut records = AcceptedUpdates::default();
        let presented = ACCEPTED_UPDATES_RETAINED + 3;
        for n in 0..presented {
            records.accept(key(n), fingerprint(n));
        }
        assert_eq!(
            records.retained(),
            ACCEPTED_UPDATES_RETAINED,
            "expected the bound to hold, got {} record(s)",
            records.retained()
        );
        for n in 0..3 {
            assert_eq!(
                records.classify(&key(n), &fingerprint(n)),
                Retry::Fresh,
                "expected the oldest record {n} to have been let go of"
            );
        }
        for n in 3..presented {
            assert_eq!(
                records.classify(&key(n), &fingerprint(n)),
                Retry::Replay,
                "expected the retained record {n} to be replayed"
            );
        }

        let mut once = AcceptedUpdates::default();
        assert_eq!(
            once.retained(),
            0,
            "expected a fresh resource to retain none, got {}",
            once.retained()
        );
        once.accept(key(0), fingerprint(0));
        once.accept(key(0), fingerprint(1));
        assert_eq!(
            once.retained(),
            1,
            "expected one token to retain one record, got {}",
            once.retained()
        );
        assert_eq!(
            once.classify(&key(0), &fingerprint(1)),
            Retry::Replay,
            "expected the record the token last accepted to be the one replayed"
        );
    }
}
