// SPDX-License-Identifier: CC0-1.0

//! Amazon Elastic Kubernetes Service's published default-quota catalog.

use super::*;

/// The sentence Amazon EKS publishes for a plane counted inside ONE cluster or node
/// group: the plane's own published name, in the service's own voice, with no account or
/// Region in it because the plane is not counted on either.
///
/// SOURCE: the recorded default-quota listing of `eks`, whose per-cluster rows all read
/// `The maximum number of <the quota's own name, uncapitalised>.` — the shared
/// [`maximum_number_phrase`] form with this service's closing stop on it.
fn per_cluster_description(counted: &str) -> String {
    format!("{}.", maximum_number_phrase(counted))
}

/// The sentence Amazon EKS publishes for a plane counted across the ACCOUNT in one
/// Region.
///
/// SOURCE: the recorded default-quota listing of `eks`, whose account planes (its
/// registered-cluster and Anywhere-subscription rows) read
/// `The maximum number of <counted> in this account in the current Region.`
fn per_account_description(counted: &str) -> String {
    per_cluster_description(&format!("{counted} in this account in the current Region"))
}

/// Amazon Elastic Kubernetes Service's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `eks` service code (the "Amazon
/// EKS service quotas" page of the Amazon EKS User Guide), and the recorded
/// default-quota listing of that code for which `QuotaCode` names which quota, for each
/// code's value, adjustability and published sentence. EKS publishes its planes per
/// CLUSTER as well as per Region — a cluster is the unit its node groups, Fargate
/// profiles, remote networks and access configuration are counted in — so a row names the
/// plane it is counted on and carries the sentence form of that plane
/// ([`per_cluster_description`] / [`per_account_description`]).
///
/// The hybrid-cluster planes are TWO quotas, not one: the NODE networks a cluster may
/// reach (`L-5C12D558`) and the POD networks it may reach (`L-6AFFD1D8`), each published
/// under its own code. The security-group plane of the control plane and the clusters of
/// the account are likewise distinct quotas under distinct codes, and this catalog
/// publishes each row under the code the recorded listing states for it rather than under
/// a neighbour's.
///
/// A quota this catalog can cite no code for is left to the default-quota-set gap rather
/// than published under a neighbouring code (see [`super::descriptive_quota_code`]), which
/// is why the add-on plane and the pod-identity-association plane of a cluster are absent.
pub(in crate::state) fn eks_quotas() -> Vec<QuotaRow> {
    vec![
        // The security-group plane of a cluster's control plane, which AWS publishes as
        // fixed.
        quota("Control plane security groups per cluster", 4.0)
            .code("L-11427A54")
            .fixed()
            .describe(&per_cluster_description(
                "control plane security groups per cluster",
            )),
        quota("Clusters", 100.0)
            .code("L-1194D53C")
            .describe(&per_account_description("EKS clusters")),
        quota("Label pairs per Fargate profile selector", 5.0)
            .code("L-23414FF3")
            .describe(&per_cluster_description(
                "label pairs per Fargate profile selector",
            )),
        quota("Fargate profiles per cluster", 10.0)
            .code("L-33415657")
            .describe(&per_cluster_description("Fargate profiles per cluster")),
        // The remote-network planes of a hybrid cluster: the NODE networks a cluster may
        // reach outside the VPC, published under its own code beside the pod networks
        // below.
        quota("Remote node networks per cluster", 15.0)
            .code("L-5C12D558")
            .describe(&per_cluster_description("remote node networks per cluster")),
        quota("Remote pod networks per cluster", 15.0)
            .code("L-6AFFD1D8")
            .describe(&per_cluster_description("remote pod networks per cluster")),
        quota("Managed node groups per cluster", 30.0)
            .code("L-6D54EA21")
            .describe(&per_cluster_description("managed node groups per cluster")),
        quota("Public endpoint access CIDR ranges per cluster", 40.0)
            .code("L-93A74D60")
            .fixed()
            .describe(&per_cluster_description(
                "public endpoint access CIDR ranges per cluster",
            )),
        quota("Nodes per managed node group", 450.0)
            .code("L-BD136A63")
            .describe(&per_cluster_description("nodes per managed node group")),
        // The access-entry plane of a cluster, which AWS publishes as fixed. The row is the
        // one this catalog already published for that quota — its name, value and sentence
        // are unchanged — moved whole onto the code the recorded listing states for it,
        // which is what the binding rule asks: nothing about the quota itself is restated
        // here.
        quota("Access entries per cluster", 100.0)
            .code("L-C56B9FC3")
            .fixed()
            .describe(&per_cluster_description("access entries per cluster")),
        quota("Selectors per Fargate profile", 5.0)
            .code("L-D78D8AF8")
            .describe(&per_cluster_description("selectors per Fargate profile")),
        quota("EKS Anywhere Enterprise Subscriptions", 10.0)
            .code("L-EA277FDC")
            .describe(&per_account_description(
                "EKS Anywhere Enterprise Subscriptions",
            )),
        // The plane of the clusters a client REGISTERS with EKS rather than has EKS run,
        // which the service words as a count of registered clusters rather than of what
        // they are connected from.
        quota("Registered clusters", 10.0)
            .code("L-FDFA5F81")
            .describe(&per_account_description("registered clusters")),
    ]
}
