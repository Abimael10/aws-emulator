// SPDX-License-Identifier: CC0-1.0

//! The compliance a State Manager association's run brings back: the patching the
//! patch-baseline document performs on the managed nodes one run of an association has
//! applied it to.
//!
//! # What applies the document, and what this adds
//!
//! An association's run holds the nodes it has applied its document on
//! ([`super::associations::AssociationRunRecord`]), and a node that comes to match the
//! association's targets AFTER that run was dispatched — an instance launched by the same
//! template, whose agent came up a moment later — is folded into it by the one settle that
//! owes that application
//! ([`ComAmazonawsSsmAmazonssmState::settle_due_association_targets`]), which is also where
//! an association that runs only on its schedule is left alone. So nothing here decides WHICH
//! nodes a run reached.
//!
//! What this adds is what the run REPORTS. The document Patch Manager publishes to patch a
//! node ([`super::patch_runs`]) performs a patching operation whose effect lives entirely in
//! THIS service's store — the baseline in force for the node's patch group, and the
//! compliance report the patch reads project — so a run that applied that document on a node
//! owes that node a report, and this is what materialises it. A document whose steps run in
//! the node's guest is NOT applied here: a run of one is a delivery to that guest, which is
//! what a Run Command send performs ([`crate::ops::send_command`]).
//!
//! # Why a read may make the write
//!
//! The report is owed once the run has FINISHED
//! ([`super::associations::ASSOCIATION_EXECUTION_DELAY`]) and is written as of that instant —
//! the instant the run brought its result back, whether or not anybody called. That is why a
//! REPORTING operation of the patch reads takes this before it reads the store. It is written
//! once per run and node: a report already carrying that run's finishing instant is the
//! report of that run, so a second settle leaves it alone and two callers see the same
//! compliance.

use std::time::SystemTime;

use super::patch_runs::{self, PatchBaselineRun};
use super::ComAmazonawsSsmAmazonssmState;

/// One report a finished run owes ONE node it applied the patch-baseline document on:
/// everything the write needs, read while the store's guard was held.
#[derive(Debug, Clone)]
struct OwedReport {
    /// The node the document was applied on.
    instance_id: String,
    /// The `PlatformType` that node reports, which decides the operating system it is
    /// patched against.
    platform_type: String,
    /// The patching operation the document asks for on that platform.
    run: PatchBaselineRun,
    /// When the run finished, which is the instant the report it brought back carries.
    finished_at: SystemTime,
}

/// Materialise every patch-compliance report that `region`'s association runs already owe, so
/// a read of the patch state reports what was due whether or not anybody called.
///
/// Call it with NO guard of this service's store held: the operating system each node is
/// patched against is read off the image the node was launched from, which is Amazon EC2's
/// own store ([`patch_runs::node_operating_system`]).
pub fn settle_association_patch_runs<Eng: Send + Sync + 'static>(engine: &Eng, region: &str) {
    let now = SystemTime::now();
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let owed = {
        let mut store = state.lock();
        // The application an association owes a node that has registered since its current
        // run was dispatched, which is the write that decides which nodes that run reached.
        store.settle_due_association_targets();
        owed_reports(&store, region, now)
    };
    if owed.is_empty() {
        return;
    }
    // Which operating system each node is patched against, resolved with no guard of this
    // service held, exactly as a Run Command send resolves it.
    let patched_against: Vec<(OwedReport, &'static str)> = owed
        .into_iter()
        .map(|report| {
            let operating_system = patch_runs::node_operating_system(
                engine,
                &report.instance_id,
                Some(&report.platform_type),
            );
            (report, operating_system)
        })
        .collect();
    let mut store = state.lock();
    for (report, operating_system) in patched_against {
        // Read again under this guard: another caller may have settled the same report while
        // the operating systems were being resolved, and a report is written once per run.
        if !report_is_owed(&store, region, &report.instance_id, report.finished_at) {
            continue;
        }
        patch_runs::perform(
            &mut store,
            region,
            &report.instance_id,
            operating_system,
            &report.run,
            report.finished_at,
        );
    }
}

/// Every report `region`'s association runs owe at `now`: one per node a FINISHED run of an
/// association applying the patch-baseline document has applied it on, where that node's
/// compliance is older than the instant that run finished.
///
/// A READ of the store, so the walk that writes above takes the guard again rather than
/// holding one while it reads another service's state. Bounded by what the stores hold: the
/// associations of the Region, and the nodes one run reaches
/// ([`super::associations::MAX_ASSOCIATION_RUN_NODES`]).
fn owed_reports(
    store: &ComAmazonawsSsmAmazonssmState,
    region: &str,
    now: SystemTime,
) -> Vec<OwedReport> {
    let mut owed = Vec::new();
    for ((held_region, _), record) in &store.associations {
        if held_region != region {
            continue;
        }
        // The run State Manager last dispatched, and only once it has finished: a run still
        // being worked through on its nodes has brought nothing back to report.
        let Some(run) = record.last_run().filter(|run| run.has_finished(now)) else {
            continue;
        };
        let finished_at = run.finished_at();
        // The version that run APPLIED, whose parameters are the ones it ran with; an
        // association keeps a bounded number of versions, so one a long-past run applied may
        // be gone, and what is left to read then is the version in force.
        let Some(version) = record
            .version_numbered(run.association_version)
            .or_else(|| record.latest())
        else {
            continue;
        };
        // The document the association applies, at the version in force for it: one this
        // endpoint no longer holds is one nothing applies.
        let Some(document) = store.resolve_document(region, &record.name) else {
            continue;
        };
        let selector = version
            .document_version
            .as_deref()
            .filter(|selector| !selector.is_empty())
            .unwrap_or(super::DOCUMENT_VERSION_DEFAULT);
        let Some(content) = document.select(selector) else {
            continue;
        };
        for instance_id in &run.nodes {
            let Some(node) = store.managed_node(region, instance_id) else {
                continue;
            };
            let Some(platform_type) = node.properties.platform_type.clone() else {
                continue;
            };
            // Only the document Patch Manager publishes to patch a node declares a
            // patch-baseline step; every other document — including every one a caller
            // created — reports no patch compliance at all.
            let Some(run) = patch_runs::requested(
                &content.content,
                &platform_type,
                &version.parameters,
                &content.parsed.parameters,
            ) else {
                continue;
            };
            if !report_is_owed(store, region, instance_id, finished_at) {
                continue;
            }
            owed.push(OwedReport {
                instance_id: instance_id.clone(),
                platform_type,
                run,
                finished_at,
            });
        }
    }
    owed
}

/// Whether the node `instance_id` of `region` still owes the report of a run that finished at
/// `finished_at`.
///
/// The instant is what makes the write idempotent: the report of ONE run carries that run's
/// finishing instant, so a report already carrying it — or a later one, left by a run
/// dispatched since — is the report that run owed and nothing is written again.
fn report_is_owed(
    store: &ComAmazonawsSsmAmazonssmState,
    region: &str,
    instance_id: &str,
    finished_at: SystemTime,
) -> bool {
    store
        .instance_patch_state(region, instance_id)
        .is_none_or(|held| held.operation_end_time < finished_at)
}
