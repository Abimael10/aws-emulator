// SPDX-License-Identifier: CC0-1.0

//! AWS Amplify's published default-quota catalog.

use super::*;

/// The `Description` AWS Amplify publishes for a plane of resources an app may have.
///
/// SOURCE: the recorded default-quota listing of `amplify`, which carries this sentence
/// for the BRANCH plane of an app with that plane's own published name read as the
/// phrase it counts ("The maximum number of branches per app that you can create in this
/// account in the current Region."). The sentence is fixed text around the row's own
/// `QuotaName`, so every row of the class is described through this one function and two
/// rows of it cannot drift apart. A row whose published name does NOT read as such a
/// phrase — the hourly app-creation rate and the cache-artifact size below — publishes no
/// description rather than one worded here (see [`super::maximum_number_phrase`]).
fn amplify_app_plane_description(quota_name: &str) -> String {
    format!(
        "{} that you can create in this account in the current Region.",
        maximum_number_phrase(&quota_name.to_lowercase())
    )
}

/// One AWS Amplify PER-APP plane: the resources of one kind an app may have.
///
/// Amplify publishes a row of this class per resource an app holds, with the plane's own
/// name and limit; the class's published members — the countable `None` unit and the
/// sentence above — are the same across it. `Adjustable` is NOT a fact of the class: the
/// evidence behind this catalog publishes the branch plane as fixed and the webhook
/// plane as adjustable, so each row states its own.
fn amplify_app_plane(code: &str, quota_name: &str, value: f64) -> QuotaRow {
    quota(quota_name, value)
        .code(code)
        .describe(&amplify_app_plane_description(quota_name))
}

/// AWS Amplify's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `amplify` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code amplify` reports, which
/// is the "Quotas" table of the AWS Amplify Hosting User Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which plane, its
/// published name, unit, default, adjustability and — for the branch plane — its own
/// published sentence.
///
/// Every row is published under the `L-` code AWS mints for it, in code order. The
/// descriptive stand-ins this catalog once published two of these planes under
/// (`AppsPerRegion`, `BranchesPerApp`) are reported by that listing as surplus entries:
/// the branch plane is published under AWS's own code below, and the app count stays
/// unpublished because that listing states no AWS code for it. The further codes that
/// listing carries name no plane this catalog can cite, or state no default for the plane
/// they publish, so they are left to the default-quota-set gap rather than bound to a
/// plane guessed at here.
pub(in crate::state) fn amplify_quotas() -> Vec<QuotaRow> {
    vec![
        // How fast apps may be created, which AWS publishes as not adjustable. Its name
        // states the hour it is counted over rather than a plane the class sentence
        // reads off its name, and the evidence behind this catalog shows that sentence
        // worded around the apps an hour allows is not what this row carries, so it
        // publishes no description of this catalog's own.
        quota("Maximum app creations per hour", 25.0)
            .code("L-2FC3A2FA")
            .fixed(),
        amplify_app_plane("L-4113FC04", "Webhooks per app", 50.0),
        // The build cache one environment may hold, published in GIGABYTES and as not
        // adjustable: a size rather than a count, so the per-app class sentence does not
        // read for it.
        quota("Environment cache artifact size", 5.0)
            .code("L-A0FC4951")
            .unit("Gigabytes")
            .fixed(),
        // The branch plane of an app, RE-KEYED onto AWS's own code: the recorded default
        // listing of this service reports the descriptive stand-in this catalog used
        // (`BranchesPerApp`) as a surplus entry and states the code AWS publishes the
        // quota under, that AWS publishes it as NOT adjustable, and its own sentence.
        amplify_app_plane("L-A6716586", "Branches per app", 50.0).fixed(),
        amplify_app_plane("L-AD277529", "Domains per app", 5.0),
    ]
}
