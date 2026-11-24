// SPDX-License-Identifier: CC0-1.0

//! How a registered job-definition revision is PROJECTED on a read, and the platform
//! default the register applies to it before it is stored.
//!
//! AWS Batch does not describe a job definition as the sparse document the register was
//! sent: it materialises the record's collections, so a member nothing was supplied for
//! comes back as an EMPTY map or list rather than as a missing member — the same way the
//! tag plane materialises `tags` ([`super::materialized_tags`]). A client that reads a
//! revision back to diff it against desired state therefore never sees a collection move
//! between `[]` and absent, and one that indexes a collection needs no null guard.
//!
//! The projection lives here, once, because every reader of a stored revision owes the
//! same document: the maps the definition itself carries (`parameters`, `tags`) and every
//! list member of each `ContainerProperties` object it carries — its own lists and those of
//! the structures it nests — wherever it carries it. The rule it applies is the one the
//! read projection beside it states for every document this service describes, and it
//! materialises each list through that module's helper.
//! Nothing is invented by it — an empty collection says exactly what the sparse member
//! said — and nothing is written: it projects a CLONE of the stored record.
//!
//! The members a `ContainerProperties` object is reported with WHATEVER the record holds —
//! its interface-configuration list and the two deprecated resource reservations — need no
//! projection step at all: they are written by that structure's own serializer
//! (`crate::ops::types::container_properties_wire`), on every read of every definition.
//!
//! Beside it is the one default the register applies, which is a WRITE: the Fargate
//! platform version the definition's containers will actually run on, stored at
//! registration so every later read reports it.

use super::*;

use crate::ops::types::FargatePlatformConfiguration;

/// The Fargate platform version a container that names none runs on.
///
/// Both members that carry one say the same thing — "If one isn't specified, the `LATEST`
/// platform version is used by default" (`FargatePlatformConfiguration$platformVersion`
/// and `EcsTaskProperties$platformVersion`) — so the value is the model's, not a choice
/// made here.
pub const DEFAULT_FARGATE_PLATFORM_VERSION: &str = "LATEST";

/// Apply the documented Fargate platform-version default to every container-properties
/// object a job definition that runs on Fargate carries.
///
/// Called where the revision is WRITTEN, because the default is applied at registration:
/// the version decides how the job will really run, so the stored record carries it and
/// every read of the revision — and every job submitted against it — reports the same
/// one. A request that states a version keeps it.
///
/// `nodeProperties` is not walked: a multi-node definition cannot run on Fargate
/// ("If the job is run on Fargate resources, then `multinode` isn't supported"), so a
/// definition reaching here with the Fargate capability carries none.
pub fn apply_fargate_platform_defaults(
    container_properties: Option<&mut ContainerProperties>,
    ecs_properties: Option<&mut EcsProperties>,
) {
    if let Some(container) = container_properties {
        let stated = container
            .fargate_platform_configuration
            .take()
            .unwrap_or_default();
        container.fargate_platform_configuration = Some(FargatePlatformConfiguration {
            platform_version: Some(
                stated
                    .platform_version
                    .unwrap_or_else(|| DEFAULT_FARGATE_PLATFORM_VERSION.to_string()),
            ),
        });
    }
    if let Some(ecs) = ecs_properties {
        for task in &mut ecs.task_properties {
            if task.platform_version.is_none() {
                task.platform_version = Some(DEFAULT_FARGATE_PLATFORM_VERSION.to_string());
            }
        }
    }
}

/// The `JobDefinition` a reader reports for the stored revision `definition`: the record
/// itself, with its collections materialised.
///
/// The definition's own maps (`parameters`, `tags`) come back present and empty where the
/// register supplied neither, and so does every collection of each `ContainerProperties`
/// object the revision carries — the top-level `containerProperties` and the `container`
/// of each `nodeProperties.nodeRangeProperties` entry, which is the same shape and so owes
/// the same document.
///
/// Members that are not collections are untouched: a scalar nothing was supplied for stays
/// absent, because an absent scalar is not an empty one.
pub fn described_job_definition(definition: &JobDefinition) -> JobDefinition {
    let mut detail = definition.clone();
    detail.parameters = Some(detail.parameters.take().unwrap_or_default());
    detail.tags = materialized_tags(&detail.tags);
    if let Some(container) = detail.container_properties.as_mut() {
        materialize_container_collections(container);
    }
    if let Some(node) = detail.node_properties.as_mut() {
        for range in &mut node.node_range_properties {
            if let Some(container) = range.container.as_mut() {
                materialize_container_collections(container);
            }
        }
    }
    detail
}

/// Materialise every collection ONE container-properties object carries.
///
/// Its own members are all seven lists `ContainerProperties` declares
/// (`smithy_query(query = "shape", shape = "ContainerProperties")`): `command`, `volumes`,
/// `environment`, `mountPoints`, `ulimits`, `resourceRequirements` and `secrets`.
///
/// The collections of the structures it NESTS are materialised the same way, because they
/// are part of the same projected container: a log configuration reports its `options` map
/// and its `secretOptions` list, and Linux parameters report their `devices` and `tmpfs`
/// lists (each device its `permissions`, each tmpfs its `mountOptions`). A nested structure
/// nothing was supplied for stays ABSENT — an absent structure is not an empty one, and
/// only the collections of a structure that is there are materialised.
fn materialize_container_collections(container: &mut ContainerProperties) {
    materialize_list(&mut container.command);
    materialize_list(&mut container.volumes);
    materialize_list(&mut container.environment);
    materialize_list(&mut container.mount_points);
    materialize_list(&mut container.ulimits);
    materialize_list(&mut container.resource_requirements);
    materialize_list(&mut container.secrets);
    if let Some(logging) = container.log_configuration.as_mut() {
        if logging.options.is_none() {
            logging.options = Some(HashMap::new());
        }
        materialize_list(&mut logging.secret_options);
    }
    if let Some(parameters) = container.linux_parameters.as_mut() {
        materialize_list(&mut parameters.devices);
        materialize_list(&mut parameters.tmpfs);
        for device in parameters.devices.iter_mut().flatten() {
            materialize_list(&mut device.permissions);
        }
        for mount in parameters.tmpfs.iter_mut().flatten() {
            materialize_list(&mut mount.mount_options);
        }
    }
}
