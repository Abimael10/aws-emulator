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
//! answer for an access that is NO LONGER granted — the resolved finding — the finding an
//! archive rule has archived ([`archive_reported_findings`]), and the two instants no policy
//! can state: when the record was created, and when an analysis last moved it between
//! granted and resolved.
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

/// The `FindingStatus` of a finding an archive rule has archived, from the same enum.
///
/// An archived finding is not gone and not resolved: the access it reports is still granted,
/// and the analyzer has been told to stop treating it as something to act on — so it keeps
/// its id and everything it says about the access, with this status in place of `ACTIVE`.
pub const FINDING_STATUS_ARCHIVED: &str = "ARCHIVED";

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
    /// The instant an archive rule archived this finding, and `None` for a finding no rule
    /// has archived.
    ///
    /// Which findings are archived is no more readable out of today's policies than which
    /// ones were reported: an archive rule archives the findings its criteria select, and
    /// that decision outlives the call that made it — so it lives here, beside the rest of a
    /// finding's lifecycle, and the analysis keeps deriving the access itself.
    pub archived_at: Option<SystemTime>,
}

impl ReportedFinding {
    /// The finding as the analyzer reports it now.
    ///
    /// While the access is granted this is the derived finding itself. Once it is not, the
    /// record reports the access it reported before with `status` `RESOLVED` and the instant
    /// of the analysis that resolved it as the instant the finding was analyzed and updated:
    /// that analysis is both what last looked at the resource and what last changed the
    /// finding.
    ///
    /// A finding an archive rule archived reports `ARCHIVED` instead of the `ACTIVE` the
    /// analysis derives, with everything else it says about the access unchanged — the
    /// archive is a statement about the FINDING, not about the grant. An access that is no
    /// longer granted is reported resolved whether or not it was archived: the analyzer's
    /// last word about a grant it can no longer find is that the grant is gone.
    pub fn reported(&self) -> FindingSummary {
        let mut finding = self.finding.clone();
        // An archive only speaks for a finding the analyzer still reports.
        let archived_at = self.archived_at.filter(|_| self.granted);
        // The finding was last updated by whichever came later: the analysis that moved it
        // between granted and resolved, or the rule that archived it.
        if let Some(updated_at) = [self.changed_at, archived_at].into_iter().flatten().max() {
            finding.updated_at = ::aws_smithy_types::DateTime::from(updated_at);
        }
        if !self.granted {
            if let Some(resolved_at) = self.changed_at {
                finding.status = FINDING_STATUS_RESOLVED.to_string();
                finding.analyzed_at = ::aws_smithy_types::DateTime::from(resolved_at);
            }
        } else if archived_at.is_some() {
            finding.status = FINDING_STATUS_ARCHIVED.to_string();
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
                        archived_at: None,
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

/// Record that an archive rule has archived the findings `keys` names — the derivation keys
/// of the findings its criteria selected — as of `now`, and answer how many findings that
/// archived.
///
/// What an apply changes is the STATUS the analyzer reports for a finding it already
/// reported: the finding keeps its id and everything it says about the access, so the caller
/// that reads it by that id is told it is archived rather than told it is gone. A key no
/// record of this analyzer is about is left alone, so a rule selecting a finding of another
/// analyzer archives nothing here.
///
/// Idempotent: a finding the rule already archived keeps the instant the first apply
/// archived it, so applying the same rule twice reports the same finding both times. A
/// finding whose access the account's policies no longer grant is not archived at all — it
/// is `RESOLVED`, which is the analyzer's last word about a grant it can no longer find.
pub fn archive_reported_findings(
    reported: &mut BTreeMap<String, ReportedFinding>,
    keys: impl IntoIterator<Item = String>,
    now: SystemTime,
) -> usize {
    let mut archived = 0;
    for key in keys {
        let Some(record) = reported.get_mut(&key) else {
            continue;
        };
        if !record.granted || record.archived_at.is_some() {
            continue;
        }
        record.archived_at = Some(now);
        archived += 1;
    }
    archived
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
