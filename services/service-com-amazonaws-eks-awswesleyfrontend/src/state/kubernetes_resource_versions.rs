// SPDX-License-Identifier: CC0-1.0

//! The REVISION every object of a cluster's served Kubernetes API carries, and the
//! versioned write a client performs with it.
//!
//! `metadata.resourceVersion` is "an opaque value that represents the internal
//! version of this object that can be used by clients to determine when objects
//! have changed"; it is "populated by the system" and read-only to a client. The
//! API server stamps it when it stores an object and stamps a NEW one at every
//! later write of that object, so a client that read an object and writes it back
//! carries the revision it read — and the server refuses that write when the
//! stored object has moved on since. That refusal is the optimistic concurrency
//! every controller, `kubectl apply`/`replace` and client-go update loop detects a
//! conflict with; without a revision on the object a client either cannot issue a
//! versioned write at all or silently overwrites a change it never saw.
//!
//! The revision is the STORE's rather than the object's: one counter per cluster
//! ([`ResourceVersions`]), which every write of that cluster's objects draws its
//! next value from. So two objects never carry one revision, a client can tell
//! which of two reads is the later one, and the revision a read reports is the one
//! the write that stored the object was stamped with — computed from the record,
//! never from the request.
//!
//! A write carrying NO revision is unconditional: "a resourceVersion of the empty
//! string means unconditional", which is what the API server serves an update that
//! overwrites whatever is stored as.

use std::sync::atomic::{AtomicU64, Ordering};

use super::kubernetes_objects::ApiRefusal;

/// The write counter of one cluster's object store: the revision its last write
/// was stamped with.
///
/// An atomic rather than a plain counter so a write draws its revision while the
/// objects of the store are already borrowed for the write itself, and so the
/// store stays shareable across the threads this service's state is reached from.
#[derive(Debug, Default)]
pub struct ResourceVersions(AtomicU64);

impl ResourceVersions {
    /// The revision to stamp the next write with.
    ///
    /// Drawn as a write is made, so no two writes of one cluster are stamped alike
    /// and a later write always carries a higher revision than an earlier one —
    /// which is what lets a client tell a stale object from a current one. A
    /// revision a refused write drew is carried by no object, as a client treating
    /// the value as opaque expects.
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }
}

/// `version` as `metadata.resourceVersion` carries it.
///
/// The member is a STRING on every Kubernetes object, whatever a server counts its
/// own revisions in, and a client treats it as opaque: it compares it for equality
/// and hands it back, so this is the one place a stored revision becomes the text a
/// client reads and the one a write is matched against.
#[must_use]
pub fn rendered(version: u64) -> String {
    version.to_string()
}

/// The revision a write's manifest asks to be matched against, and `None` for a
/// manifest that carries none — the unconditional write.
#[must_use]
pub fn requested(manifest: &::serde_json::Value) -> Option<&str> {
    manifest
        .get("metadata")
        .and_then(|metadata| metadata.get("resourceVersion"))
        .and_then(::serde_json::Value::as_str)
        .filter(|version| !version.is_empty())
}

/// `Ok(())` when the write `manifest` carries may be stored over the object
/// `resource`/`name` currently at revision `stored`, else the `409 Conflict` the
/// API server refuses a write of a revision that is no longer the current one with.
///
/// `api_group` is the group the resource belongs to, which the refusal names it by
/// ([`ApiRefusal::conflict`]), and `None` the core group.
///
/// The ONE check for every resource of this plane a client may write twice, so a
/// ConfigMap, a workload and an RBAC object cannot disagree about when a write is
/// stale — and it is made before anything is stored, leaving the object exactly as
/// it was.
pub(super) fn admits_write(
    resource: &str,
    api_group: Option<&str>,
    name: &str,
    stored: u64,
    manifest: &::serde_json::Value,
) -> Result<(), ApiRefusal> {
    match requested(manifest) {
        Some(carried) if carried != rendered(stored) => {
            Err(ApiRefusal::conflict(resource, api_group, name))
        }
        _ => Ok(()),
    }
}
