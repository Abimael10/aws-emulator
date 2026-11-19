// SPDX-License-Identifier: CC0-1.0

//! The collections a READ materialises, and the one place that decides them.
//!
//! ONE rule, the same one the job-definition projection and the tag plane already apply
//! ([`super::described_job_definition`], [`super::materialized_tags`]): AWS Batch does not
//! answer a read with the sparse document its create was sent — it materialises the
//! record's collections, so a modelled list or map the resource holds nothing in comes back
//! PRESENT and EMPTY instead of missing. "Absent" and "empty" are different answers:
//! idiomatic client code indexes a modelled collection directly, so an omitted empty list
//! raises where `[]` reads as "nothing configured".
//!
//! What lives here is the helper every projection in this service materialises a list
//! through, and the projection of the one document whose collections the readers used to
//! pass straight out of the store: the `computeResources` of a compute environment
//! (`ComputeEnvironmentDetail$computeResources`, whose EC2-only lists a Fargate environment
//! holds nothing in).
//!
//! Nothing is invented by it — an empty collection says exactly what the sparse member said
//! — and nothing is written: the projection is of a CLONE of the stored record. A member
//! that is not a collection is untouched, because an absent scalar is not an empty one, and
//! a nested structure nothing was supplied for stays ABSENT: only the collections of a
//! structure that is there are materialised.

use super::*;

/// One list member, reported as the empty list when nothing was supplied for it.
pub fn materialize_list<T>(member: &mut Option<Vec<T>>) {
    if member.is_none() {
        *member = Some(Vec::new());
    }
}

/// The `computeResources` a reader reports for the stored `resource`: the record itself,
/// with its collections materialised.
///
/// Every list member `ComputeResource` declares — `instanceTypes`, `subnets`,
/// `securityGroupIds` and `ec2Configuration`
/// (`smithy_query(query = "shape", shape = "ComputeResource")`) — comes back present and
/// empty where the environment holds nothing in it, and so does its `tags` map, the way the
/// tag plane materialises a resource's own tags. That is what lets a client read the
/// `instanceTypes` of an environment whose compute type is a Fargate one and conclude that
/// no instance types, and so no vCPU floors, are configured, rather than failing on a member
/// that is not there.
///
/// `capacityTags` is left exactly as stored: its own length constraint publishes a minimum
/// of one entry, so the empty map is not a value that member can take.
pub fn described_compute_resource(resource: &ComputeResource) -> ComputeResource {
    let mut described = resource.clone();
    materialize_list(&mut described.instance_types);
    materialize_list(&mut described.subnets);
    materialize_list(&mut described.security_group_ids);
    materialize_list(&mut described.ec2_configuration);
    described.tags = materialized_tags(&described.tags);
    // A launch template is one of those nested structures: present only where the create
    // named one, and then reporting its own list.
    if let Some(template) = described.launch_template.as_mut() {
        materialize_list(&mut template.overrides);
    }
    described
}

/// The `container` a reader reports for the stored `container`: the record itself, with its
/// collections materialised.
///
/// Every list member `ContainerDetail` declares that a container job holds nothing in —
/// `volumes`, `environment`, `mountPoints`, `ulimits`, `secrets` and the
/// `networkInterfaces` of the task it runs as
/// (`smithy_query(query = "shape", shape = "ContainerDetail")`) — comes back present and
/// EMPTY, which is what AWS answers a described job with: a client that iterates the
/// `environment` of a container whose definition set none reads "nothing configured" rather
/// than failing on a member that is not there.
///
/// `resourceRequirements` is left exactly as stored, because a container job of this service
/// always states its reservation and an empty list is not what a container with none reads
/// as; so are the scalars and the nested structures, for the reason this module's header
/// gives — an absent scalar is not an empty one.
pub fn described_container(container: &ContainerDetail) -> ContainerDetail {
    let mut described = container.clone();
    materialize_list(&mut described.volumes);
    materialize_list(&mut described.environment);
    materialize_list(&mut described.mount_points);
    materialize_list(&mut described.ulimits);
    materialize_list(&mut described.secrets);
    materialize_list(&mut described.network_interfaces);
    described
}
