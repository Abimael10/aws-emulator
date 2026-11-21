// SPDX-License-Identifier: CC0-1.0

//! Amazon Chime's published default-quota catalog.

use super::*;

/// One BURST allowance of this service: how many calls of one API may arrive at once,
/// rather than the steady rate they may keep up.
///
/// Every member of such a row but the operation's own name and the value is the same
/// across the class: AWS publishes the allowance as `<Api> API burst rate`. One
/// constructor fills that so two rows of the class cannot drift apart. The window, the
/// usage metric and the sentence this service words a burst allowance with are not among
/// the data available here, so these rows publish none of them rather than members of
/// this catalog's own (the sentence [`super::api_burst_rate_description`] builds is
/// another service's wording).
fn chime_api_burst_rate(code: &str, api_name: &str, requests: f64) -> QuotaRow {
    quota(format!("{api_name} API burst rate"), requests).code(code)
}

/// Amazon Chime's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `chime` service code, which states
/// which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. This
/// service publishes a long catalog — a row per API rate, per burst allowance and per
/// media plane — and the quotas that listing carries under a code whose published NAME
/// or default VALUE no source available here states are left to the default-quota-set gap
/// rather than published under a guessed fact, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
pub(in crate::state) fn chime_quotas() -> Vec<QuotaRow> {
    vec![
        chime_api_burst_rate("L-398B2BD6", "DeleteAttendee", 20.0),
        // The VOICE plane, published as calls per second rather than as a count.
        quota("SIP trunking and voice - calls per second (CPS)", 1.0).code("L-B6F2C623"),
        // The meetings plane's management rate, published across every meeting
        // management API rather than per operation.
        quota(
            "Amazon Chime SDK meetings - All meeting management API rate in requests \
             per second",
            10.0,
        )
        .code("L-F147F728"),
        chime_api_burst_rate("L-FE9F896D", "CreateMeeting", 20.0),
    ]
}
