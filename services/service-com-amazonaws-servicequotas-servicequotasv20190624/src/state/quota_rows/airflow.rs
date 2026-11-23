// SPDX-License-Identifier: CC0-1.0

//! Amazon Managed Workflows for Apache Airflow's published default-quota catalog.

use super::*;

/// Amazon Managed Workflows for Apache Airflow's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `airflow` service code (what
/// `aws service-quotas list-aws-default-service-quotas --service-code airflow` reports,
/// which is the "Amazon MWAA quotas" table of the Amazon MWAA User Guide), and the
/// recorded default-quota listing of that code for which `QuotaCode` names which plane,
/// its published name and its default.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. MWAA
/// publishes its environment count as TWO planes — one for the small, medium and large
/// environment classes and one for the larger ones — so each carries its own code, name
/// and default rather than one row standing in for both. The larger-environment plane's
/// own published sentence is not among the data available here — the evidence behind this
/// catalog shows that neither the sentence of the plane beside it nor that sentence's
/// form around this plane's own name is what this row carries, and withholds the text —
/// so it publishes none rather than wording of this catalog's own (see
/// [`super::maximum_number_phrase`]).
pub(in crate::state) fn airflow_quotas() -> Vec<QuotaRow> {
    vec![
        quota(
            "Larger environments per account per Region (xlarge, 2xlarge)",
            5.0,
        )
        .code("L-4BA0CB82"),
        quota("Web servers per environment", 5.0)
            .code("L-52DCDA32")
            .describe("The maximum number of web servers per environment."),
        quota(
            "Environments per account per Region (small, medium, large)",
            10.0,
        )
        .code("L-67D41C6B")
        .describe(&format!(
            "{}.",
            maximum_number_phrase("environments per account per Region")
        )),
        // The worker plane of an environment, which AWS publishes as adjustable the way
        // the environment counts are.
        quota("Workers per environment", 25.0)
            .code("L-78AC1883")
            .describe("The maximum number of workers per environment."),
    ]
}
