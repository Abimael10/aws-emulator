// SPDX-License-Identifier: CC0-1.0

//! Amazon CodeCatalyst's published default-quota catalog.

use super::*;

/// Amazon CodeCatalyst's published default-quota catalog.
///
/// SOURCE: the recorded default-quota listing of the `codecatalyst` service code, which
/// states which quota each `QuotaCode` names and the facts published under it.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The rest
/// of that listing's codes — the SSO-group plane of a team (`L-1EE0DDBE`, whose quota name
/// it states while its value stays withheld) among them — are left to the
/// default-quota-set gap rather than published under a guessed value, the way
/// [`super::access_analyzer_quotas`] leaves the rest of that service's catalog.
///
/// The sentences this service publishes for its rows are not among the data available
/// here, so the rows publish none rather than wording of this catalog's own.
///
/// Every row this service publishes is published as NOT adjustable: the quotas are fixed
/// by the product's own structure, so each row states that rather than taking the
/// adjustable shape most published quotas have.
pub(in crate::state) fn codecatalyst_quotas() -> Vec<QuotaRow> {
    vec![
        // The asset plane of ONE package version.
        quota("Number of assets in a version", 150.0)
            .code("L-861435F6")
            .fixed(),
        // How many package repositories one search of this service's package plane
        // reaches.
        quota("Upstream package repositories searched", 25.0)
            .code("L-AF962739")
            .fixed(),
        // The membership plane of a team.
        quota("Number of users in a team", 1000.0)
            .code("L-B22DB613")
            .fixed(),
        // How many repositories one package repository may draw directly from.
        quota("Number of direct upstream repositories", 10.0)
            .code("L-C5F8A35A")
            .fixed(),
    ]
}
