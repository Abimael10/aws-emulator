// SPDX-License-Identifier: CC0-1.0

//! How a batch build FANS OUT: which builds it runs, when each of them runs, and the
//! build record one declared group is run as.
//!
//! Two operations start the builds of a batch — `StartBuildBatch`, which fans a batch out
//! for the first time, and `RetryBuildBatch`, which runs the groups of a failed batch
//! again — and a build of a batch is the same build whichever of them started it: a build
//! of the batch's project, carrying the configuration the BATCH resolved, paced by the
//! level of the dependency graph its group sits at. So the plan and that build record are
//! written once, here, rather than once per operation.

use super::{
    build_arn, build_logs_location, datetime, declaration_levels, in_progress_phases,
    source_download_declaration, BatchBuildDeclaration, GroupArtifacts, BUILD_SETTLE_MILLIS,
};
use crate::ops::types::{Build, BuildBatch};

/// The builds one batch build is made of, as the plan its pacing is read from.
#[derive(Debug, Clone, Default)]
pub struct BatchFanOut {
    /// One entry per build the batch runs — the batch's own source download first, then
    /// each build the batchspec declared — with the LEVEL of the dependency graph that
    /// build waits behind. Empty for a batch whose specification declared nothing: there
    /// is then no build to download a source for either.
    pub planned: Vec<(BatchBuildDeclaration, usize)>,
    /// How many spans the batch itself takes: one per level of its dependency graph, plus
    /// the one in which it combines what its deepest level produced. A batch that declared
    /// nothing fans out into nothing and takes the single span a build takes.
    pub spans: usize,
}

/// The fan-out `declarations` describe: the groups the batch runs, in the order it reports
/// them, each with the level it waits behind ([`declaration_levels`]).
///
/// Independent groups start together and a group waits one span per level of the graph it
/// sits under — every declared group behind the source download
/// ([`source_download_declaration`]), which is the level they all build from — so the
/// batch itself finishes a span after its deepest level does.
pub fn batch_fan_out(declarations: &[BatchBuildDeclaration]) -> BatchFanOut {
    if declarations.is_empty() {
        return BatchFanOut {
            planned: Vec::new(),
            spans: 1,
        };
    }
    let levels = declaration_levels(declarations);
    let planned: Vec<(BatchBuildDeclaration, usize)> =
        ::std::iter::once((source_download_declaration(), 0))
            .chain(
                declarations
                    .iter()
                    .cloned()
                    .zip(levels.iter().map(|level| level.saturating_add(1))),
            )
            .collect();
    let deepest = planned.iter().map(|(_, level)| *level).max().unwrap_or(0);
    BatchFanOut {
        planned,
        spans: deepest.saturating_add(2),
    }
}

/// The instant at which the transition the clock owes one group's build falls due: a group
/// at level `level` waits that many spans behind the level it builds from, so its own span
/// is the next one.
pub fn batch_group_settle_due_millis(now_millis: i64, level: usize) -> i64 {
    now_millis + BUILD_SETTLE_MILLIS * i64::try_from(level.saturating_add(1)).unwrap_or(1)
}

/// The build a batch runs for one declared build group.
///
/// A build of a batch runs with the configuration the BATCH resolved — the source,
/// environment, artifacts, role, timeouts and key the batch itself reports — because those
/// are the settings the project and the request that started the batch settled for the
/// whole batch; the group adds only what the batchspec declares for it, which is its own
/// buildspec when it names one (`buildspec` in the batchspec, the per-build equivalent of
/// `StartBuild`'s `buildspecOverride`). It is a build of the same project, with its own
/// build number from that project's sequence, and it names the batch it belongs to through
/// `buildBatchArn`.
///
/// `artifacts` is the batch's destination as a run reports one ([`super::build_artifacts_of`]),
/// so a build of the batch answers exactly what a build of the same project answers.
pub fn batch_group_build(
    build_batch: &BuildBatch,
    artifacts: &GroupArtifacts,
    declaration: &BatchBuildDeclaration,
    build_id: &str,
    region: &str,
    build_number: i64,
    now: i64,
) -> Build {
    let mut source = build_batch.source.clone().unwrap_or_default();
    if let Some(buildspec) = declaration.buildspec.clone() {
        source.buildspec = Some(buildspec);
    }
    Build {
        id: Some(build_id.to_string()),
        arn: Some(build_arn(region, build_id)),
        build_number: Some(build_number),
        start_time: Some(datetime(now)),
        end_time: None,
        current_phase: Some("QUEUED".to_string()),
        build_status: Some("IN_PROGRESS".to_string()),
        source_version: build_batch.source_version.clone(),
        // The commit the source version resolved to. Nothing clones a repository here,
        // so there is no resolved revision to report.
        resolved_source_version: None,
        project_name: build_batch.project_name.clone(),
        phases: Some(in_progress_phases(now)),
        source: Some(source),
        secondary_sources: build_batch.secondary_sources.clone(),
        secondary_source_versions: build_batch.secondary_source_versions.clone(),
        artifacts: artifacts.primary.clone(),
        secondary_artifacts: artifacts.secondary.clone(),
        cache: build_batch.cache.clone(),
        environment: build_batch.environment.clone(),
        service_role: build_batch.service_role.clone(),
        // Where this build's output goes, under the log configuration the BATCH runs
        // with — every build of the batch delivers under that one configuration, each to
        // the stream its own id names ([`build_logs_location`]).
        logs: Some(build_logs_location(
            region,
            build_batch.project_name.as_deref().unwrap_or_default(),
            build_id,
            build_batch.log_config.as_ref(),
        )),
        timeout_in_minutes: build_batch.build_timeout_in_minutes,
        queued_timeout_in_minutes: build_batch.queued_timeout_in_minutes,
        build_complete: false,
        initiator: build_batch.initiator.clone(),
        vpc_config: build_batch.vpc_config.clone(),
        // Allocated when a build container joins the project's VPC; no container is
        // provisioned here.
        network_interface: None,
        encryption_key: build_batch.encryption_key.clone(),
        // Exported by a buildspec's `env/exported-variables` while it runs, and reports
        // and artifacts are produced by a running build.
        exported_environment_variables: None,
        report_arns: None,
        file_system_locations: build_batch.file_system_locations.clone(),
        debug_session: None,
        // What makes this a build OF the batch: the batch it belongs to, by ARN.
        build_batch_arn: build_batch.arn.clone(),
        // Retries are per build and are asked for by `RetryBuild`; a batch's own retries
        // are `RetryBuildBatch`, which runs the batch's groups again rather than
        // auto-retrying one build.
        auto_retry_config: None,
    }
}
