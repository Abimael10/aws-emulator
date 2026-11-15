// SPDX-License-Identifier: CC0-1.0

//! The two spellings of the Availability Zones an Auto Scaling group places instances in.
//!
//! A group states where it launches as `AvailabilityZones` ("one or more Availability
//! Zones for the group") or as `AvailabilityZoneIds` ("a list of Availability Zone IDs for
//! the Auto Scaling group"), and "you cannot specify both … in the same request"
//! (`CreateAutoScalingGroup$AvailabilityZoneIds`). The two members are two spellings of
//! ONE placement, so a request that states either one states both: a zone has a name AND
//! an id, and the group reports each of them whichever the caller wrote.
//!
//! The mapping between the two is Amazon EC2's own catalog for the Region
//! ([`ec2::zone_id_for_zone_name`], [`ec2::availability_zones_in_region`]) — the same one
//! a subnet is placed from and the same one [`super::group_configuration`] resolves a
//! requested zone against — so one zone is never known to two services under two
//! different ids. A value the catalog does not hold maps to nothing rather than to an id
//! this module invents; the handlers refuse such a request before anything is stored.

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

/// The id of the Availability Zone named `zone_name`, or `None` when no Region's catalog
/// holds a zone under that name.
#[must_use]
pub fn zone_id_of(zone_name: &str) -> Option<String> {
    ec2::zone_id_for_zone_name(zone_name)
}

/// The ids of the zones `zone_names` names, in the order they were named, each one ONCE.
///
/// A name the catalog does not hold contributes no id: the list reports the zones that
/// resolve rather than a placeholder for one that does not.
#[must_use]
pub fn zone_ids_of(zone_names: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for zone_name in zone_names {
        if let Some(zone_id) = zone_id_of(zone_name) {
            if !ids.contains(&zone_id) {
                ids.push(zone_id);
            }
        }
    }
    ids
}

/// The names of the zones `zone_ids` identifies in `region`, in the order they were
/// named, each one ONCE.
///
/// The inverse of [`zone_ids_of`], for a request that states the placement by id: the
/// group's `AvailabilityZones` is a required member of every read of it, so the names
/// are resolved when the group is written rather than left empty.
#[must_use]
pub fn zone_names_of(region: &str, zone_ids: &[String]) -> Vec<String> {
    let catalog = ec2::availability_zones_in_region(region);
    let mut names: Vec<String> = Vec::new();
    for zone_id in zone_ids {
        let Some(zone) = catalog.iter().find(|zone| zone.zone_id == *zone_id) else {
            continue;
        };
        if !names.contains(&zone.zone_name) {
            names.push(zone.zone_name.clone());
        }
    }
    names
}
