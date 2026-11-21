// SPDX-License-Identifier: CC0-1.0

//! AWS Auto Scaling Plans' published default-quota catalog.

use super::*;

/// AWS Auto Scaling Plans' published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `autoscaling-plans` service code,
/// which states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// descriptive stand-in this catalog used to publish the scaling-plan count under
/// (`ScalingPlansPerRegion`) is reported by that listing as a surplus entry, and the
/// quota it names is published here under `L-BD401546` instead — the rule
/// [`crate::state::descriptive_quota_code`] states: a row is published under the code
/// AWS mints for it wherever this catalog can cite that code.
pub(in crate::state) fn autoscaling_plans_quotas() -> Vec<QuotaRow> {
    vec![
        // The nested planes of a scaling plan: the configurations one scaling
        // instruction may track, and the instructions one plan may carry.
        quota("Target tracking configurations per scaling instruction", 10.0)
            .code("L-6538FA5E"),
        quota("Scaling instructions per scaling plan", 500.0).code("L-7FAA513E"),
        // The account's scaling plans themselves.
        quota("Scaling plans", 100.0).code("L-BD401546"),
    ]
}
