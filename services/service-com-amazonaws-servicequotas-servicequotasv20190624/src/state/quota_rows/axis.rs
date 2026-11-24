// SPDX-License-Identifier: CC0-1.0

//! The `axis` service code's published default-quota catalog: the geospatial job planes
//! of Amazon SageMaker geospatial capabilities.

use super::*;

/// One concurrent-JOB quota of this service: how many jobs of one kind it may have
/// running at once.
///
/// Every member of such a row but the job type's own name and the value is the same
/// across the class — a plain count, published as NOT adjustable — so one constructor
/// fills it and two rows of the class cannot drift apart. The sentences this service
/// publishes for the class are not among the data available here, so the rows publish
/// none rather than wording of this catalog's own.
fn concurrent_jobs(code: &str, job_type: &str, jobs: f64) -> QuotaRow {
    quota(format!("Maximum concurrent {job_type}"), jobs)
        .code(code)
        .fixed()
}

/// The `axis` service code's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `axis` service code, which states
/// which quota each `QuotaCode` names and the facts published under it.
///
/// Both rows are published under the `L-` code AWS mints for them, in code order.
pub(in crate::state) fn axis_quotas() -> Vec<QuotaRow> {
    vec![
        concurrent_jobs("L-962C3D55", "VectorEnrichmentJobs", 3.0),
        concurrent_jobs("L-DC290044", "EarthObservationJobs", 3.0),
    ]
}
