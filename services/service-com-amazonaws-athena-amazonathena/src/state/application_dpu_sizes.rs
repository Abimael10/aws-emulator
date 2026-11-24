// SPDX-License-Identifier: CC0-1.0

//! The application runtimes this service publishes, and the data processing unit sizes
//! each one supports.
//!
//! `ListApplicationDPUSizes` is documented "Returns the supported DPU sizes for the
//! supported application runtimes (for example, Athena notebook version 1)", so the set
//! this module holds IS the catalog that listing reports: it is the service's own rather
//! than anything a request wrote — no operation adds to it or takes from it, which is why
//! two listings of it report the same entries.
//!
//! Both sides of an entry come from the model rather than from a reading request:
//!
//! * the runtime id is the notebook version a session may name —
//!   `StartSessionRequest$NotebookVersion` states "The only valid notebook version is
//!   `Athena notebook version 1`", so that is the one runtime Athena runs applications on;
//! * its supported sizes are the DPU counts a unit of that runtime's compute may be asked
//!   for, which `EngineConfiguration$CoordinatorDpuSize` and
//!   `EngineConfiguration$DefaultExecutorDpuSize` bound with a `@range` of 1 to 1 — "An
//!   executor is the smallest unit of compute that a notebook session can request from
//!   Athena. The default is 1."
//!
//! So the sizes this catalog publishes are exactly the sizes a `StartSession` naming them
//! is accepted with, and the ones it refuses are the ones outside that modelled range.

use super::*;

use crate::ops::types::ApplicationDPUSizes;

/// The one application runtime Athena runs notebook sessions on: the notebook version
/// `StartSessionRequest$NotebookVersion` calls "the only valid notebook version", which is
/// also the example `ListApplicationDPUSizes` names for an application runtime. Within the
/// 1 to 128 characters `ApplicationDPUSizes$ApplicationRuntimeId` admits.
pub const ATHENA_NOTEBOOK_RUNTIME_ID: &str = "Athena notebook version 1";

/// The smallest number of data processing units a unit of a notebook runtime's compute can
/// be asked for: the `@range` minimum of `CoordinatorDpuSize` and `DefaultExecutorDpuSize`,
/// which is also the default those members document.
pub const MIN_RUNTIME_DPU_SIZE: i32 = 1;

/// The largest such number: the `@range` maximum of those same two members. The range is a
/// single point today, so the catalog below publishes one size for the notebook runtime.
pub const MAX_RUNTIME_DPU_SIZE: i32 = 1;

/// Every application runtime this service publishes, in the order a listing reports them,
/// each paired with the DPU sizes it supports.
///
/// Derived from the modelled bounds above rather than written out, so the sizes a listing
/// publishes cannot drift from the sizes a session start is accepted with.
pub fn supported_application_dpu_sizes() -> Vec<ApplicationDPUSizes> {
    vec![ApplicationDPUSizes {
        application_runtime_id: Some(ATHENA_NOTEBOOK_RUNTIME_ID.to_string()),
        supported_dpu_sizes: Some((MIN_RUNTIME_DPU_SIZE..=MAX_RUNTIME_DPU_SIZE).collect()),
    }]
}

/// The cursor a `ListApplicationDPUSizes` page hands back: the catalog it is paging — one
/// the whole account shares, since the operation models no member that narrows it — and the
/// position to resume at.
fn application_dpu_sizes_cursor(position: usize) -> String {
    format!("applicationdpusizes{CURSOR_SEPARATOR}{ACCOUNT_SCOPE}{CURSOR_SEPARATOR}{position}")
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// The `NextToken` a `ListApplicationDPUSizes` page resumed at `position` hands back.
    pub fn application_dpu_sizes_page_token(&self, position: usize) -> String {
        ::hex::encode(
            self.page_token_key
                .mint(&application_dpu_sizes_cursor(position)),
        )
    }

    /// The position `token` resumes an application-runtime listing at, or `None` for a token
    /// this store did not mint for THAT listing.
    pub fn application_dpu_sizes_page_position(&self, token: &str) -> Option<usize> {
        let cursor = self.page_cursor(token)?;
        cursor_position(&cursor, "applicationdpusizes", ACCOUNT_SCOPE)
    }
}
