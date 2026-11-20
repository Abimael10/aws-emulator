// SPDX-License-Identifier: CC0-1.0

//! The ONE read projection of a stored backup job.
//!
//! # The rule this module implements
//!
//! A backup job is a STORED record, and both operations that report one —
//! `DescribeBackupJob` and `ListBackupJobs` — report exactly what that record carries, so a
//! job read on its own and the same job in a page can never disagree about it.
//!
//! What the record does NOT carry is left unset rather than defaulted: the backup options a
//! job was started with none of, the vault-type classification of a job whose recovery point
//! is in an ordinary backup vault, and the completion a job that has already completed is no
//! longer expected to reach are all absent from the body, because a client that branches on
//! a present member would otherwise branch on nothing the service was told.
//!
//! What it DOES carry is reported in full, derived from the record once here rather than in
//! each reader: the instant the job was initiated, the deadline its start window set, the
//! name of the protected resource its `ResourceArn` names, the lifecycle its recovery point
//! is kept under and the message category its terminal state puts it in.

use std::time::SystemTime;

use super::{
    resource_name_of, BackupJob, ComAmazonawsBackupCryocontrollerusermanagerState, Lifecycle,
    VAULT_TYPE_BACKUP_VAULT,
};

/// The members a backup-job read projects, computed ONCE from the stored job and the vault
/// holding its recovery point (the module docs). A member the stored job does not carry
/// stays `None` here, and so is absent from both responses.
#[derive(Debug, Clone)]
pub struct BackupJobView {
    pub job_id: String,
    pub backup_vault_name: String,
    pub backup_vault_arn: String,
    /// The type of the vault holding the job's recovery point, reported only for a vault
    /// that is not an ordinary backup vault: the member exists to tell the other vault
    /// types apart ("valid values are `BACKUP_VAULT` for standard backup vaults and
    /// `LOGICALLY_AIR_GAPPED_BACKUP_VAULT` for logically air-gapped vaults", its model
    /// doc), and AWS reports none for a job in a standard one.
    pub vault_type: Option<String>,
    pub recovery_point_arn: String,
    /// The lifecycle the job's recovery point is kept under, always reported: the job has
    /// one whether or not its start named day counts, so the structure is present and its
    /// members state what was asked for — nothing, when nothing was.
    pub recovery_point_lifecycle: Lifecycle,
    pub encryption_key_arn: Option<String>,
    pub is_encrypted: bool,
    pub resource_arn: String,
    pub resource_type: Option<String>,
    /// The protected resource's non-unique name, read off its ARN
    /// ([`resource_name_of`]).
    pub resource_name: Option<String>,
    pub creation_date: SystemTime,
    /// When the job was initiated. An on-demand job is initiated by the call that creates
    /// it, so this is the instant it was created; a job something else schedules would be
    /// initiated at an instant of its own.
    pub initiation_date: SystemTime,
    pub completion_date: Option<SystemTime>,
    /// When the job is expected to complete — reported only while it still has a
    /// completion ahead of it. A job that has reached a terminal state reports the
    /// completion that HAPPENED instead.
    pub expected_completion_date: Option<SystemTime>,
    /// The deadline the job's start window set ([`BackupJob::start_by`]).
    pub start_by: Option<SystemTime>,
    pub state: String,
    pub status_message: Option<String>,
    /// The message category the job's terminal state puts it in, as the settle that
    /// decided that state recorded it: a job still on its way to one is in no category
    /// yet.
    pub message_category: Option<String>,
    pub percent_done: String,
    pub backup_size_in_bytes: i64,
    pub bytes_transferred: i64,
    pub iam_role_arn: String,
    /// The backup options the job was started with, reported only when it carries any: an
    /// option is "only available for Windows Volume Shadow Copy Service (VSS) backup jobs"
    /// (its model doc), and a job started with none is reported with none rather than with
    /// an empty map.
    pub backup_options: Option<std::collections::HashMap<String, String>>,
    pub is_parent: bool,
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// Project one stored backup job for a read. The ONE place a backup job's wire view is
    /// computed; both backup-job read operations go through it.
    pub fn backup_job_view(&self, job: &BackupJob) -> BackupJobView {
        BackupJobView {
            job_id: job.job_id.clone(),
            backup_vault_name: job.backup_vault_name.clone(),
            backup_vault_arn: job.backup_vault_arn.clone(),
            vault_type: self
                .vault_type_of(&job.backup_vault_arn)
                .filter(|vault_type| vault_type != VAULT_TYPE_BACKUP_VAULT),
            recovery_point_arn: job.recovery_point_arn.clone(),
            recovery_point_lifecycle: job.lifecycle.clone().unwrap_or_default(),
            encryption_key_arn: job.encryption_key_arn.clone(),
            is_encrypted: job.is_encrypted,
            resource_arn: job.resource_arn.clone(),
            resource_type: job.resource_type.clone(),
            resource_name: resource_name_of(&job.resource_arn),
            creation_date: job.creation_date,
            initiation_date: job.creation_date,
            completion_date: job.completion_date,
            // The due instant is what the job is expected to complete at, and it stops
            // being an expectation the moment the job has completed.
            expected_completion_date: job
                .completion_date
                .is_none()
                .then_some(job.settles_at),
            start_by: job.start_by,
            state: job.state.clone(),
            status_message: job.status_message.clone(),
            message_category: job.message_category.clone(),
            percent_done: job.percent_done.clone(),
            backup_size_in_bytes: job.backup_size_in_bytes,
            bytes_transferred: job.bytes_transferred,
            iam_role_arn: job.iam_role_arn.clone(),
            backup_options: (!job.backup_options.is_empty())
                .then(|| job.backup_options.clone().into_iter().collect()),
            is_parent: job.is_parent,
        }
    }
}
