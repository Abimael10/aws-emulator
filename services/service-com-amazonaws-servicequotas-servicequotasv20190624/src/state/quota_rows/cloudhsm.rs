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

/// AWS CloudHSM's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `cloudhsm` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code cloudhsm` reports, which
/// is the "AWS CloudHSM quotas" page of the AWS CloudHSM User Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which quota, for the
/// account-and-Region wording its counted planes carry, for each row's adjustability and
/// for the usage metrics its cluster and HSM planes are metered by. CloudHSM counts a plane
/// either per Region and account or per CLUSTER — a cluster is the unit its HSMs, keys and
/// users are counted in — so a row names the plane it is counted on.
///
/// Every `QuotaName` starts with a CAPITAL, the way AWS publishes it: a name is the text a
/// console or a client matches a quota by, so a row whose name was lower-cased would not be
/// found by it.
pub(in crate::state) fn cloudhsm_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Users per CloudHSM cluster", 1024.0)
            .code("L-0B35A0E6")
            .fixed()
            .describe("The maximum number of users in a CloudHSM cluster."),
        quota("HSMs per CloudHSM cluster", 28.0)
            .code("L-0D8B0541")
            .fixed()
            .describe("The maximum number of HSMs in a CloudHSM cluster."),
        quota("Keys per hsm2m.medium CloudHSM cluster", 16_666.0)
            .code("L-194EB2B8")
            .fixed()
            .describe("The maximum number of keys in a cluster of hsm2m.medium HSMs."),
        quota("Keys per CloudHSM cluster", 3300.0)
            .code("L-36814031")
            .fixed()
            .describe("The maximum number of keys in a cluster of hsm1.medium HSMs."),
        resource_count_plane(
            "L-4B16B391",
            "Clusters per AWS Region and AWS account",
            4.0,
            CLOUDHSM_USAGE_SERVICE,
            CLUSTER_RESOURCE,
            "CloudHSM clusters",
        ),
        quota("Backups per CloudHSM cluster", 20.0)
            .code("L-772318AE")
            .fixed()
            .describe("The maximum number of backups retained for a CloudHSM cluster."),
        // The HSM plane is metered the way the cluster plane is, but the recorded listing
        // publishes it with NO window of its own, so the row carries the metric alone.
        quota("HSMs per AWS Region and AWS account", 6.0)
            .code("L-95BA35D1")
            .metric_without_period(usage_metric(
                CLOUDHSM_USAGE_SERVICE,
                &[
                    ("Class", "None"),
                    ("Resource", HSM_RESOURCE),
                    ("Type", "Resource"),
                ],
            ))
            .describe("The maximum number of HSMs in this account in the current Region."),
        quota("Client connections per HSM", 900.0)
            .code("L-F847CBBF")
            .fixed()
            .describe("The maximum number of client connections to one HSM."),
        quota("Length of a password", 32.0)
            .code("L-F9BBB650")
            .fixed()
            .describe("The maximum number of characters for a password."),
    ]
}
