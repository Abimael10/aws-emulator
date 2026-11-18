// SPDX-License-Identifier: CC0-1.0

//! The Spot pools a Spot Fleet request's LAUNCH TEMPLATE configurations describe: the
//! resolution of one `LaunchTemplateConfig` list into the pools a fleet places its Spot
//! capacity in, and what one instance of a pool provides.
//!
//! "The launch template and overrides" (`SpotFleetRequestConfigData$LaunchTemplateConfigs`
//! and `ModifySpotFleetRequestRequest$LaunchTemplateConfigs`): the same list is read when a
//! fleet is CREATED and when its launch template configurations are MODIFIED, so the
//! resolution lives here once — a template the account does not hold, a version it does not
//! have, an AMI it cannot see, a subnet or an Availability Zone the Region does not have are
//! refused the one way, and the launch a pool places is built the one way.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

use crate::ops::types::{
    LaunchTemplateConfig, LaunchTemplateOverrides, RunInstancesMonitoringEnabled, SpotPlacement,
};

/// "The number of units provided by the specified instance type"
/// (`SpotFleetLaunchSpecification$WeightedCapacity`), defaulted to
/// [`DEFAULT_FLEET_INSTANCE_WEIGHT`] — one unit per instance — when a pool names none.
///
/// A weight of zero or less would make a pool provide nothing however many instances it
/// launched, so it is refused rather than letting a fleet ask for an unbounded number of
/// instances. One definition for a fleet's own launch specifications and for a launch
/// template's overrides, so the two cannot read the member differently.
pub fn spot_fleet_pool_weighted_capacity<E>(requested: Option<f64>) -> Result<f64, Scaffolded<E>> {
    let weight = requested.unwrap_or(DEFAULT_FLEET_INSTANCE_WEIGHT);
    if weight.is_finite() && weight > 0.0 {
        Ok(weight)
    } else {
        Err(Scaffolded::invalid_parameter_value(format!(
            "Invalid value '{weight}' for weightedCapacity. \
             It must be the number of units one instance provides"
        )))
    }
}

/// How many Spot pools a `LaunchTemplateConfig` list DESCRIBES: one per override, and one for
/// a configuration that names no override at all — the template as it stands.
///
/// Counted before anything is resolved, because that count is the per-pool work one call asks
/// for (an AMI, a subnet, a security-group and a launch-template lookup each), which
/// [`MAX_SPOT_FLEET_POOLS`] bounds.
pub fn spot_fleet_template_pool_count(configs: &[LaunchTemplateConfig]) -> usize {
    configs
        .iter()
        .map(|entry| entry.overrides.as_ref().map_or(1, Vec::len).max(1))
        .fold(0usize, usize::saturating_add)
}

/// The Spot pools the launch template `configs` describe, resolved against the account's own
/// launch templates and the `region`'s Availability Zones.
///
/// "The launch template and overrides" (`SpotFleetRequestConfigData$LaunchTemplateConfigs`):
/// the template holds the launch parameters, and each override is one Spot pool of the fleet —
/// so a template the account does not hold, or a version it does not have, is refused with the
/// launch template API's own error code. `fleet_spot_price` is the fleet's own maximum price,
/// which a pool that names none falls back to, and `operation` is the operation the refusal of
/// a member this service resolves no pool from names.
pub fn spot_fleet_template_pools<E>(
    store: &ComAmazonawsEc2Amazonec2State,
    zones: &[AvailabilityZoneRecord],
    region: &str,
    configs: &[LaunchTemplateConfig],
    fleet_spot_price: Option<&str>,
    operation: &str,
) -> Result<Vec<SpotFleetPoolPlacement>, Scaffolded<E>> {
    let mut pools: Vec<SpotFleetPoolPlacement> = Vec::new();
    for entry in configs {
        let Some(specification) = entry.launch_template_specification.as_ref() else {
            return Err(Scaffolded::missing_parameter(
                "Each launchTemplateConfig must contain the parameter \
                 launchTemplateSpecification",
            ));
        };
        let template_id = resolve_launch_template_id(
            store,
            specification.launch_template_id.as_deref(),
            specification.launch_template_name.as_deref(),
        )?;
        let Some(record) = store.launch_templates.get(&template_id) else {
            return Err(invalid_launch_template_id_not_found());
        };
        // "The launch template version number, `$Latest`, or `$Default`"
        // (`FleetLaunchTemplateSpecification$Version`): a configuration that names none
        // launches the template's default version, which is the version a launch with no
        // version named gets everywhere in this API.
        let version = specification
            .version
            .as_deref()
            .unwrap_or(LAUNCH_TEMPLATE_VERSION_DEFAULT);
        let number = resolve_launch_template_version(record, version)?;
        let Some(launch_data) = record.versions.get(&number).map(|held| held.data.clone()) else {
            return Err(Scaffolded::internal_error(format!(
                "launch template '{template_id}' version '{version}' could not be read back"
            )));
        };
        let Some(image_id) = launch_data.image_id.clone() else {
            return Err(Scaffolded::missing_parameter(format!(
                "Launch template '{template_id}' names no imageId, so the Spot Fleet \
                 request has no AMI to launch from"
            )));
        };
        require_visible_image(store, &image_id)?;
        // A configuration with no overrides is ONE pool: the template as it stands.
        let overrides: Vec<Option<&LaunchTemplateOverrides>> = match entry.overrides.as_deref() {
            None | Some([]) => vec![None],
            Some(entries) => entries.iter().map(Some).collect(),
        };
        for over in overrides {
            // "If you specify `InstanceRequirements`, you can't specify `InstanceType`"
            // (`LaunchTemplateOverrides$InstanceRequirements`): the two members are two
            // ways of saying which instance type a pool launches, so an override that
            // gives both says it twice — refused through the one place that rule lives,
            // so every operation that reads the list answers it alike.
            if over.is_some_and(|over| {
                over.instance_type.is_some() && over.instance_requirements.is_some()
            }) {
                return Err(instance_requirements_with_instance_type());
            }
            // An override that names its pool by instance REQUIREMENTS asks EC2 to
            // "identify instance types with the provided requirements" and launch from
            // those (`$InstanceRequirements`), which this slice does not do: nothing here
            // resolves a requirement set to the instance types that satisfy it. Refused
            // rather than accepted and dropped, because a pool that launched the
            // template's own instance type instead would not be the pool the request
            // described.
            if over.is_some_and(|over| over.instance_requirements.is_some()) {
                return Err(unsupported_operation(format!(
                    "{operation} places the Spot pools the instance type of a launch \
                     template or of an override names; the override member \
                     'instanceRequirements' is not one this service resolves instance \
                     types from"
                )));
            }
            if let Some(subnet_id) = over.and_then(|over| over.subnet_id.as_deref()) {
                if !store.subnets.contains_key(subnet_id) {
                    return Err(invalid_subnet_id_not_found(subnet_id));
                }
            }
            if let Some(zone_name) = over.and_then(|over| over.availability_zone.as_deref()) {
                availability_zone_of_region(zones, region, "availabilityZone", zone_name, |zone| {
                    zone.zone_name.as_str()
                })?;
            }
            let pool_price = over.and_then(|over| over.spot_price.clone());
            validate_hourly_price("spotPrice", pool_price.as_deref())?;
            pools.push(SpotFleetPoolPlacement {
                launch: spot_fleet_launch_from_template(&launch_data, over),
                weighted_capacity: spot_fleet_pool_weighted_capacity(
                    over.and_then(|over| over.weighted_capacity),
                )?,
                spot_price: pool_price.or_else(|| fleet_spot_price.map(str::to_string)),
                count: 0,
            });
        }
    }
    Ok(pools)
}

/// One Spot pool's launch from the launch template version `data` and the `over` that
/// narrows it, if any.
///
/// "The launch template and overrides" (`SpotFleetRequestConfigData$LaunchTemplateConfigs`):
/// an override names the instance type, the subnet, the Availability Zone and the weighted
/// capacity of ONE pool, and everything else about the launch comes from the template — so
/// the pools of one template differ exactly where the overrides say they do.
pub fn spot_fleet_launch_from_template(
    data: &RequestLaunchTemplateData,
    over: Option<&LaunchTemplateOverrides>,
) -> LaunchSpecification {
    let zone = over
        .and_then(|over| over.availability_zone.clone())
        .or_else(|| {
            data.placement
                .as_ref()
                .and_then(|placement| placement.availability_zone.clone())
        });
    let groups: Vec<GroupIdentifier> = data
        .security_group_ids
        .iter()
        .flatten()
        .map(|group_id| GroupIdentifier {
            group_id: Some(group_id.clone()),
            group_name: None,
        })
        .chain(
            data.security_groups
                .iter()
                .flatten()
                .map(|group_name| GroupIdentifier {
                    group_id: None,
                    group_name: Some(group_name.clone()),
                }),
        )
        .collect();
    LaunchSpecification {
        image_id: data.image_id.clone(),
        instance_type: over
            .and_then(|over| over.instance_type.clone())
            .or_else(|| data.instance_type.clone()),
        key_name: data.key_name.clone(),
        subnet_id: over.and_then(|over| over.subnet_id.clone()),
        user_data: data.user_data.clone(),
        addressing_type: None,
        kernel_id: data.kernel_id.clone(),
        ramdisk_id: data.ram_disk_id.clone(),
        ebs_optimized: Some(data.ebs_optimized.unwrap_or(false)),
        monitoring: data.monitoring.as_ref().map(|monitoring| {
            RunInstancesMonitoringEnabled {
                enabled: monitoring.enabled.unwrap_or(false),
            }
        }),
        placement: zone.map(|zone| SpotPlacement {
            availability_zone: Some(zone),
            group_name: None,
            tenancy: None,
            availability_zone_id: None,
        }),
        security_groups: Some(groups),
        // "The block device mapping" (`RequestLaunchTemplateData$BlockDeviceMappings`) a
        // version carries applies to every launch made from it, so a pool of this template
        // boots the volume that version describes — read through the one conversion of a
        // version's entries ([`template_block_device_mapping`]).
        block_device_mappings: data.block_device_mappings.as_ref().map(|mappings| {
            mappings
                .iter()
                .map(template_block_device_mapping)
                .collect()
        }),
        iam_instance_profile: None,
        network_interfaces: None,
    }
}
