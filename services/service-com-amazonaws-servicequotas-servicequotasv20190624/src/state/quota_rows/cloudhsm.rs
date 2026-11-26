// SPDX-License-Identifier: CC0-1.0

//! AWS CloudHSM's published default-quota catalog.

use super::*;

/// The `AWS/Usage` display name CloudHSM publishes its resource counts under.
const CLOUDHSM_USAGE_SERVICE: &str = "CloudHSM";

/// The `AWS/Usage` `Resource` series CloudHSM publishes its cluster count under.
///
/// `AWS/Usage` names the series after the resource TYPE counted, in the singular — it is
/// the dimension VALUE a client puts in a CloudWatch query, so a plural would query a
/// series that does not exist.
const CLUSTER_RESOURCE: &str = "Cluster";

/// The `AWS/Usage` `Resource` series CloudHSM publishes its HSM count under.
const HSM_RESOURCE: &str = "HSM";

/// One plane CloudHSM counts a provisioned resource on: the plane as its `QuotaName` names
/// it (`<Resources> per <plane>`), and the plane as its `Description` words it.
///
/// CloudHSM counts a plane either per Region and account or per CLUSTER — a cluster is the
/// unit its HSMs, keys and users are counted in.
type Plane = (&'static str, &'static str);

/// The CLUSTER plane: the resources one cluster may hold.
const CLUSTER_PLANE: Plane = ("CloudHSM cluster", "in a CloudHSM cluster");

/// The account's own plane, which CloudHSM names by the Region and the account together.
const REGION_PLANE: Plane = (
    "AWS Region and AWS account",
    "in this AWS Region and AWS account",
);

/// One AWS CloudHSM quota of a plane a caller PROVISIONS: the clusters, HSMs, keys or users
/// that may be created on that plane.
///
/// CloudHSM publishes a row per resource per plane, and every member but the resource, the
/// plane and the value is the same across the class: the `QuotaName` is `<Resources> per
/// <plane>` and the `Description` states the number of them "that you can create" on that
/// plane. One constructor fills them, so two rows of the class cannot drift apart and no
/// row loses the sentence out of its text.
///
/// `created` is the resource as the name spells it and `described` as the sentence words it
/// — an acronym keeps its capitals in both.
fn created_plane(
    code: &str,
    created: &str,
    described: &str,
    plane: Plane,
    value: f64,
) -> QuotaRow {
    quota(format!("{created} per {}", plane.0), value)
        .code(code)
        .describe(&format!(
            "The maximum number of {described} that you can create {}.",
            plane.1
        ))
}

/// AWS CloudHSM's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `cloudhsm` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code cloudhsm` reports, which
/// is the "AWS CloudHSM quotas" page of the AWS CloudHSM User Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which quota, for the
/// wording its provisioned planes carry ([`created_plane`]), for each row's adjustability
/// and for the usage metrics its cluster and HSM planes are metered by.
///
/// Every `QuotaName` starts with a CAPITAL, the way AWS publishes it: a name is the text a
/// console or a client matches a quota by, so a row whose name was lower-cased would not be
/// found by it.
///
/// Beside the planes, CloudHSM publishes BOUNDS — the length a password may have and the
/// clients one HSM serves at once — which state the bound itself, since nothing is created
/// by reaching them.
pub(in crate::state) fn cloudhsm_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Number of concurrent clients", 900.0)
            .code("L-0B35A0E6")
            .fixed()
            .describe("The maximum number of concurrent clients per HSM."),
        created_plane("L-0D8B0541", "HSMs", "HSMs", CLUSTER_PLANE, 28.0).fixed(),
        quota("Minimum length of a password", 7.0)
            .code("L-194EB2B8")
            .fixed()
            .describe("The minimum number of characters for a password."),
        created_plane("L-36814031", "Keys", "keys", CLUSTER_PLANE, 3300.0).fixed(),
        created_plane("L-4B16B391", "Clusters", "clusters", REGION_PLANE, 4.0).metric(
            usage_metric(
                CLOUDHSM_USAGE_SERVICE,
                &[
                    ("Class", "None"),
                    ("Resource", CLUSTER_RESOURCE),
                    ("Type", "Resource"),
                ],
            ),
        ),
        // UNSOURCED BINDING: no source names the quota CloudHSM publishes under
        // `L-772318AE`, so the code goes on answering the row it has answered rather than
        // resolving to nothing.
        quota("Backups per CloudHSM cluster", 20.0)
            .code("L-772318AE")
            .fixed()
            .describe("The maximum number of backups retained for a CloudHSM cluster."),
        // The HSM plane is metered the way the cluster plane is, but the recorded listing
        // publishes it with NO window of its own, so the row carries the metric alone.
        created_plane("L-95BA35D1", "HSMs", "HSMs", REGION_PLANE, 6.0).metric_without_period(
            usage_metric(
                CLOUDHSM_USAGE_SERVICE,
                &[
                    ("Class", "None"),
                    ("Resource", HSM_RESOURCE),
                    ("Type", "Resource"),
                ],
            ),
        ),
        created_plane("L-F847CBBF", "Users", "users", CLUSTER_PLANE, 1024.0).fixed(),
        quota("Length of a password", 32.0)
            .code("L-F9BBB650")
            .fixed()
            .describe("The maximum number of characters for a password."),
    ]
}
