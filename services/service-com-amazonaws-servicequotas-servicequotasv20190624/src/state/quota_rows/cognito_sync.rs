// SPDX-License-Identifier: CC0-1.0

//! Amazon Cognito Sync's published default-quota catalog.

use super::*;

/// Amazon Cognito Sync's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `cognito-sync` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// sentences this service publishes for its rows are not among the data available here,
/// so the rows publish none rather than wording of this catalog's own.
pub(in crate::state) fn cognito_sync_quotas() -> Vec<QuotaRow> {
    vec![
        // How long a bulk publish must wait before the next one may be started, which
        // AWS publishes as not adjustable.
        quota("Bulk publish wait time", 24.0).code("L-239B0C0A").fixed(),
        // The dataset plane of one identity.
        quota("Datasets per identity", 20.0).code("L-2625A4A0"),
        // A dataset's NAME is bounded as a size, in bytes, and AWS publishes that bound
        // as not adjustable; the dataset's own size is bounded in megabytes and is
        // adjustable.
        quota("Dataset name size", 128.0)
            .code("L-4CD747A5")
            .unit("Bytes")
            .fixed(),
        quota("Records per dataset", 1024.0).code("L-97A9518B"),
        quota("Dataset size", 1.0).code("L-C5EE478A").unit("Megabytes"),
    ]
}
