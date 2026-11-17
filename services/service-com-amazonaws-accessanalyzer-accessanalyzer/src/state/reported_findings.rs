// SPDX-License-Identifier: CC0-1.0

//! What an external access analyzer HAS REPORTED: the record each of its findings is, kept
//! so a grant the account's policies no longer make is reported as a resolved finding rather
//! than forgotten.
//!
//! A finding is not purely a projection of today's policies. It has a LIFECYCLE: the
//! analyzer reports a finding for an access it found, and when a later analysis of the same
//! resource no longer finds that access, the finding it already reported becomes `RESOLVED`
//! — it keeps its id and what it said about the access, and the caller that asks for it by
//! id is told the access was resolved rather than told no such finding exists. That history
//! cannot be read back out of the policy, because the policy no longer mentions the access:
//! it is the one thing about a finding this service has to HOLD.
//!
//! So each analyzer carries a ledger of the findings it has reported, keyed by the position
//! in the analysis the finding is about (resource, then statement, then principal) — the same
//! key the finding's id is derived from ([`super::derived_uuid`]), which is what lets a
//! record be matched with the finding a later analysis derives for the same access.
//!
//! ## What the ledger adds to the analysis, and what it leaves alone
//!
//! The analysis itself is unchanged: [`super::external_access_findings`] derives every
//! finding the account's stored policies grant, and a finding whose access is still granted
//! is reported exactly as that analysis derives it. What the ledger contributes is the
//! answer for an access that is NO LONGER granted — the resolved finding — and the two
//! instants no policy can state: when the record was created, and when an analysis last
//! moved it between granted and resolved.
//!
//! ## When it is written
//!
//! An analyzer analyzes continuously from the moment it is created; the comparison of its
//! last analysis with the account's current policies is therefore owed whatever any caller
//! does, and [`reconcile_reported_findings`] is where a call that asks the analyzer what it
//! found materialises it. A call changes no finding of its own: reconciling twice over the
//! same policies leaves the same records, so two reads report the same findings with the same
//! ids, statuses and instants.
//!
//! The analysis is materialised when it is asked for rather than on a timer, so the instant a
//! resolved finding carries is the instant of the first call that saw the access gone, not of
//! an analysis pass nobody observed — and an access that was granted and removed again between
//! two such calls leaves no record, where a continuously running analyzer would have reported
//! one. That is the one place this ledger differs from the service it stands in for; what it
//! buys is that no call writes anything a later call cannot derive the same way.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use crate::ops::types::FindingSummary;

/// The `FindingStatus` of a finding whose access the account's policies no longer grant, from
/// the model's [`crate::ops::types::FINDING_STATUS_VALUES`].
pub const FINDING_STATUS_RESOLVED: &str = "RESOLVED";

/// How many RESOLVED findings one analyzer's ledger keeps.
///
/// The findings that are still granted are bounded by the analysis itself (it refuses an
/// account whose policies produce more than it derives for one call), but a resolved record
/// is kept after the access is gone, so the ledger is bounded here as well: the least
/// recently resolved record is dropped once this many are held, the way an analyzer's scan
/// records are bounded by [`super::MAX_SCANNED_RESOURCES_PER_ANALYZER`]. The emulator's own
/// ceiling, generous beside the ten full pages ([`super::MAX_PAGE_SIZE`]) a realistic listing
/// walks, so a caller cannot grow this process's memory without bound by rewriting one
/// policy.
pub const MAX_RESOLVED_FINDINGS_PER_ANALYZER: usize = 1_000;

/// One finding the analyzer has reported, as its ledger holds it.
#[derive(Debug, Clone)]
pub struct ReportedFinding {
    /// The finding as the analysis last derived it — what the record keeps saying about the
    /// access once that access is gone and no policy states it any more.
    pub finding: FindingSummary,
    /// Whether the analyzer's last analysis still found the access this record is about.
    /// `false` is a resolved finding.
    pub granted: bool,
    /// The instant of the analysis that last moved this record between granted and resolved,
    /// and `None` for one no analysis has moved since it was created.
    ///
    /// The one instant of a finding that no stored policy can state: it is when the access
    /// stopped (or started again) being granted, which is only observable by comparing an
    /// analysis with the one before it.
    pub changed_at: Option<SystemTime>,
}

impl ReportedFinding {
    /// The finding as the analyzer reports it now.
    ///
    /// While the access is granted this is the derived finding itself. Once it is not, the
    /// record reports the access it reported before with `status` `RESOLVED` and the instant
    /// of the analysis that resolved it as the instant the finding was analyzed and updated:
    /// that analysis is both what last looked at the resource and what last changed the
    /// finding.
    pub fn reported(&self) -> FindingSummary {
        let mut finding = self.finding.clone();
        if let Some(changed_at) = self.changed_at {
            let at = ::aws_smithy_types::DateTime::from(changed_at);
            finding.updated_at = at;
            if !self.granted {
                finding.status = FINDING_STATUS_RESOLVED.to_string();
                finding.analyzed_at = at;
            }
        }
        finding
    }
}

/// Bring `reported` up to date with `derived` — the findings the account's policies grant
/// right now — and answer the findings the analyzer reports as of `now`, each paired with its
/// derivation key.
///
/// This is the analysis the analyzer owes, materialised: a derived access the ledger does not
/// hold is a finding the analyzer has now reported, one it holds is the same finding again
/// (keeping the instant that finding was created at), and a record whose access is NOT
/// derived any more is resolved as of `now`. Idempotent in the policies: reconciling twice
/// over the same derivation changes nothing, so a read reports what the read before it did.
///
/// The answer is ordered by derivation key, so a resolved finding sits where the access it
/// reported sat and the order a listing pages over stays stable across calls.
pub fn reconcile_reported_findings(
    reported: &mut BTreeMap<String, ReportedFinding>,
    derived: Vec<(String, FindingSummary)>,
    now: SystemTime,
) -> Vec<(String, FindingSummary)> {
    let mut granted_now: BTreeSet<String> = BTreeSet::new();
    for (key, finding) in derived {
        match reported.get_mut(&key) {
            // The same access as before: the finding is the one the analysis just derived,
            // and the record keeps the instant it was created at. An access that had been
            // resolved and is granted again CHANGED at this analysis.
            Some(record) => {
                if !record.granted {
                    record.changed_at = Some(now);
                    record.granted = true;
                }
                record.finding = FindingSummary {
                    created_at: record.finding.created_at,
                    ..finding
                };
            }
            // An access no record of this analyzer is about: a finding it reports for the
            // first time, created by the analysis that derived it.
            None => {
                reported.insert(
                    key.clone(),
                    ReportedFinding {
                        finding,
                        granted: true,
                        changed_at: None,
                    },
                );
            }
        }
        granted_now.insert(key);
    }

    // Every record whose access this analysis no longer found: resolved as of `now`, keeping
    // its id and what it reported.
    for (key, record) in reported.iter_mut() {
        if record.granted && !granted_now.contains(key) {
            record.granted = false;
            record.changed_at = Some(now);
        }
    }
    bound_resolved_records(reported);

    reported
        .iter()
        .map(|(key, record)| (key.clone(), record.reported()))
        .collect()
}

/// Drop the least recently resolved records once an analyzer holds more than
/// [`MAX_RESOLVED_FINDINGS_PER_ANALYZER`] of them, so the ledger of one analyzer is bounded
/// however many times the account's policies are rewritten.
///
/// Only resolved records are dropped: a record whose access is still granted is re-derived by
/// every analysis, so dropping it would hide a finding the account's policies do grant.
fn bound_resolved_records(reported: &mut BTreeMap<String, ReportedFinding>) {
    let mut resolved: Vec<(SystemTime, String)> = reported
        .iter()
        .filter(|(_, record)| !record.granted)
        .map(|(key, record)| {
            (
                // A resolved record always carries the instant it was resolved at.
                record.changed_at.unwrap_or(SystemTime::UNIX_EPOCH),
                key.clone(),
            )
        })
        .collect();
    if resolved.len() <= MAX_RESOLVED_FINDINGS_PER_ANALYZER {
        return;
    }
    // Oldest resolution first, and the derivation key breaks a tie, so which records are
    // dropped does not depend on iteration luck.
    resolved.sort();
    let doomed = resolved.len() - MAX_RESOLVED_FINDINGS_PER_ANALYZER;
    for (_, key) in resolved.into_iter().take(doomed) {
        reported.remove(&key);
    }
}
