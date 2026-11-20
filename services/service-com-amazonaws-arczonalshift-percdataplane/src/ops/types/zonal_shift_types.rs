// SPDX-License-Identifier: CC0-1.0

//! The ZONAL SHIFT objects this service reports, as AWS carries them.
//!
//! These are every type a response of this crate reports a zonal shift through: the
//! `ZonalShift` body of `StartZonalShift`, `CancelZonalShift` and `UpdateZonalShift`,
//! the `ZonalShiftSummary` of `ListZonalShifts`, the `ZonalShiftInResource` of
//! `GetManagedResource` and `ListManagedResources`, and the `StartPracticeRunResponse`
//! of `StartPracticeRun` — so what they declare is what every read of a shift reports.
//!
//! They are declared here rather than beside the other shapes because AWS answers with
//! one member the published model snapshot does not declare, and that snapshot is kept
//! as shipped (see `smithy-model/LOCAL-ADDITIONS.md`):
//!
//! * `azGroupId` — the Availability Zone group of the Region the shifted resource lives
//!   in, which the recorded create, reads and cancel of one zonal shift all carry. The
//!   snapshot declares neither the member nor a shape of that name.
//!
//! So the types are generated, like every other modelled shape, from a model file — the
//! small overlay `smithy-model/local-additions.json`, which restates the members the
//! snapshot declares, with their own targets so every length, pattern and enum
//! constraint is captured exactly as the snapshot spells it, and adds the one AWS
//! carries. Nothing about the wire is hand-written: the member names, the shape ids and
//! the constraints all come from that file.
//!
//! The VALUE of the member is not declared here: it is derived from the shifted
//! resource's Region ([`crate::state::az_group_id`]), so every site above reports the
//! same group for the same shift.
//!
//! `tests/all/zonal_shift_az_group.rs` pins the member on the wire and the overlay
//! against the snapshot it restates.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/local-additions.json");

/// The `ZonalShift` body `StartZonalShift` answers 201 with.
#[smithy_macros::smithy(shape = "com.amazonaws.arczonalshift#ZonalShift")]
#[derive(Debug, Clone)]
pub struct StartZonalShiftOutput {
    pub zonal_shift_id: String,
    pub resource_identifier: String,
    pub away_from: String,
    pub expiry_time: ::aws_smithy_types::DateTime,
    pub start_time: ::aws_smithy_types::DateTime,
    pub status: String,
    pub comment: String,
    /// The member the snapshot does not declare: the Availability Zone group of the
    /// Region the shifted resource lives in. `None` only for a resource whose ARN names
    /// no Region at all, which names no group either.
    pub az_group_id: Option<String>,
}

/// The `ZonalShift` body `CancelZonalShift` answers 200 with — the same modelled shape
/// as the create, so it reports the same members for the shift it canceled.
#[smithy_macros::smithy(shape = "com.amazonaws.arczonalshift#ZonalShift")]
#[derive(Debug, Clone)]
pub struct CancelZonalShiftOutput {
    pub zonal_shift_id: String,
    pub resource_identifier: String,
    pub away_from: String,
    pub expiry_time: ::aws_smithy_types::DateTime,
    pub start_time: ::aws_smithy_types::DateTime,
    pub status: String,
    pub comment: String,
    /// See [`StartZonalShiftOutput::az_group_id`].
    pub az_group_id: Option<String>,
}

/// The `ZonalShift` body `UpdateZonalShift` answers 200 with.
#[smithy_macros::smithy(shape = "com.amazonaws.arczonalshift#ZonalShift")]
#[derive(Debug, Clone)]
pub struct UpdateZonalShiftOutput {
    pub zonal_shift_id: String,
    pub resource_identifier: String,
    pub away_from: String,
    pub expiry_time: ::aws_smithy_types::DateTime,
    pub start_time: ::aws_smithy_types::DateTime,
    pub status: String,
    pub comment: String,
    /// See [`StartZonalShiftOutput::az_group_id`].
    pub az_group_id: Option<String>,
}

/// One entry of the `items` page `ListZonalShifts` answers with.
#[smithy_macros::smithy(shape = "com.amazonaws.arczonalshift#ZonalShiftSummary")]
#[derive(Debug, Clone)]
pub struct ZonalShiftSummary {
    pub zonal_shift_id: String,
    pub resource_identifier: String,
    pub away_from: String,
    pub expiry_time: ::aws_smithy_types::DateTime,
    pub start_time: ::aws_smithy_types::DateTime,
    pub status: String,
    pub comment: String,
    pub shift_type: Option<String>,
    pub practice_run_outcome: Option<String>,
    /// See [`StartZonalShiftOutput::az_group_id`].
    pub az_group_id: Option<String>,
}

/// One entry of the `zonalShifts` a managed resource reports.
#[smithy_macros::smithy(shape = "com.amazonaws.arczonalshift#ZonalShiftInResource")]
#[derive(Debug, Clone)]
pub struct ZonalShiftInResource {
    pub applied_status: String,
    pub zonal_shift_id: String,
    pub resource_identifier: String,
    pub away_from: String,
    pub expiry_time: ::aws_smithy_types::DateTime,
    pub start_time: ::aws_smithy_types::DateTime,
    pub comment: String,
    pub shift_type: Option<String>,
    pub practice_run_outcome: Option<String>,
    /// See [`StartZonalShiftOutput::az_group_id`].
    pub az_group_id: Option<String>,
}

/// The practice run zonal shift `StartPracticeRun` answers 201 with. A practice run IS a
/// zonal shift — `ListZonalShifts` and `GetManagedResource` report it through the shapes
/// above — so it reports the same group for the same shift.
#[smithy_macros::smithy(shape = "com.amazonaws.arczonalshift#StartPracticeRunResponse")]
#[derive(Debug, Clone)]
pub struct StartPracticeRunOutput {
    pub zonal_shift_id: String,
    pub resource_identifier: String,
    pub away_from: String,
    pub expiry_time: ::aws_smithy_types::DateTime,
    pub start_time: ::aws_smithy_types::DateTime,
    pub status: String,
    pub comment: String,
    /// See [`StartZonalShiftOutput::az_group_id`].
    pub az_group_id: Option<String>,
}
