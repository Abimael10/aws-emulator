// SPDX-License-Identifier: CC0-1.0

//! Amazon OpenSearch Service's published default-quota catalog, under the `ServiceCode`
//! its API endpoint uses.

use super::*;

/// The phrase Amazon OpenSearch Service publishes every one of its cluster planes around:
/// the plane is counted in a SINGLE named instance of the service, and the service names
/// itself in full in the sentence rather than leaving the reader to infer it.
///
/// SOURCE: the recorded default-quota listing of `es`, whose warm-node, instance,
/// dedicated-master and burstable-instance rows all read
/// `The maximum number of <counted> in a single Amazon OpenSearch Service <plane>.` —
/// one sentence form for the whole class, so it is built once here.
fn in_a_single(counted: &str, plane: &str) -> String {
    format!("The maximum number of {counted} in a single Amazon OpenSearch Service {plane}.")
}

/// Amazon OpenSearch Service's published quota catalog, under the `ServiceCode` its
/// API endpoint uses.
///
/// SOURCE: AWS's published Service Quotas data for the `es` service code (the "Amazon
/// OpenSearch Service quotas" page of its Developer Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which quota and for
/// the published wording of each row. The service publishes the domain plane of the
/// Region beside the planes counted inside one domain, and it words those planes as
/// planes of the DOMAIN — the resource a client creates — except the warm tier, which it
/// words as a plane of the cluster the domain runs. The cross-cluster-search and package
/// planes of a domain are left to the default-quota-set gap: this catalog can cite no code
/// for either, and a code identifies ONE quota (see [`super::descriptive_quota_code`]).
pub(in crate::state) fn es_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Domains per Region", 500.0)
            .code("L-076D529E")
            .describe(
                "The maximum number of Amazon OpenSearch Service domains you can create in \
                 each AWS Region.",
            ),
        // The warm-tier instance plane, the one plane this service publishes as a plane of
        // the CLUSTER a domain runs rather than of the domain itself. AWS publishes it as
        // fixed.
        quota("Warm instances per domain", 150.0)
            .code("L-1F053E6F")
            .fixed()
            .describe(&in_a_single("warm nodes", "cluster")),
        // The quota `QuotaContextInfo`'s own documentation uses as its worked
        // example of a RESOURCE-level quota: the instance count is applied per
        // OpenSearch Service domain, and can also be set account-wide, so AWS
        // publishes it as adjustable at either level. The scope type names the
        // resource type the per-resource plane applies to, which for this service is
        // its OpenSearch Service domain resource, and the quota is published as a
        // quota OF THAT RESOURCE — the recorded default-quota listing of this code
        // reports the `RESOURCE` scope on the record that names no resource — so every
        // record of it, the account-wide one included, reports that scope. Its published
        // sentence names the ceiling an increase may be asked up to beside the default.
        quota("Instances per domain", 200.0)
            .code("L-6408ABDE")
            .adjustable_at(LEVEL_ALL, "AWS::OpenSearchService::Domain")
            .counted_per_resource()
            .describe(&format!(
                "{} You can request an increase up to 1002 instances per domain.",
                in_a_single("instances", "domain")
            )),
        // The dedicated-master plane of a domain, which AWS publishes as fixed.
        quota("Dedicated master instances per domain", 5.0)
            .code("L-AE676A72")
            .fixed()
            .describe(&in_a_single("dedicated master instances", "domain")),
        // The burstable-instance plane of a domain, which AWS publishes as fixed: the
        // value is the number of T2 data nodes a domain may run, as the service's own
        // published cluster limits state it. AWS publishes the plane under the instance
        // TYPE it restricts rather than under the type's bare name.
        quota("Instances per domain (T2 instance type)", 10.0)
            .code("L-E9BC8C95")
            .fixed()
            .describe(&in_a_single("T2 instances", "domain")),
    ]
}
