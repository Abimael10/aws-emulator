// SPDX-License-Identifier: CC0-1.0

//! `Options`: how a transfer task — and each of its executions — is configured.
//!
//! This is the ONE `Options` type of this crate, the shape `CreateTask`,
//! `UpdateTask`, `StartTaskExecution`, `UpdateTaskExecution`, `DescribeTask` and
//! `DescribeTaskExecution` all reach, so what it declares is what every operation of
//! this service parses, persists and reports.
//!
//! It is declared here rather than beside the other shared shapes because AWS answers
//! with one member the published model snapshot does not declare, and that snapshot is
//! kept as shipped (see `smithy-model/LOCAL-ADDITIONS.md`):
//!
//! * `PreserveNfsAcls` — reported alongside the other metadata options on the tasks and
//!   task executions AWS describes, recorded in the `simparity-wire-aws-v1` wire
//!   fixtures. The snapshot declares neither the member nor a shape of that name.
//!
//! So the type is generated, like every other modelled shape, from a model file — the
//! small overlay `smithy-model/local-additions.json`, which restates the sixteen members
//! the snapshot declares, with their own targets so every enum value set and range is
//! captured exactly as the snapshot spells it, and adds the seventeenth. Nothing about
//! the wire is hand-written: the member names, the shape id and the constraints all come
//! from that file.
//!
//! `tests/all/transfer_task_lifecycle.rs` pins both halves: the member on the wire, and
//! the overlay against the snapshot it restates.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/local-additions.json");

#[smithy_macros::smithy(shape = "com.amazonaws.datasync#Options")]
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub verify_mode: Option<String>,
    pub overwrite_mode: Option<String>,
    pub atime: Option<String>,
    pub mtime: Option<String>,
    pub uid: Option<String>,
    pub gid: Option<String>,
    pub preserve_deleted_files: Option<String>,
    pub preserve_devices: Option<String>,
    pub posix_permissions: Option<String>,
    pub bytes_per_second: Option<i64>,
    pub task_queueing: Option<String>,
    pub log_level: Option<String>,
    pub transfer_mode: Option<String>,
    pub security_descriptor_copy_flags: Option<String>,
    pub object_tags: Option<String>,
    pub object_metadata: Option<String>,
    /// The member the snapshot does not declare: whether the transfer copies the NFS
    /// access control lists of what it moves. An open string, because nothing publishes
    /// the value set — whatever a request carries is what this service stores and
    /// reports back, so no value set is invented for it. Its value is resolved like
    /// every other option, by [`crate::state::default_options`] and
    /// [`crate::state::overlay_options`].
    pub preserve_nfs_acls: Option<String>,
}
