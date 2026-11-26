// SPDX-License-Identifier: CC0-1.0

//! The AWS Local Zones a Region publishes, and the zone groups this account has opted in to.
//!
//! A Local Zone is "an extension of an Amazon Web Services Region in geographic proximity to
//! your users" that a caller reaches only after opting its GROUP in
//! (`ModifyAvailabilityZoneGroup`), which is why such a zone reports an `OptInStatus` at all
//! while a standard Availability Zone "always has the value of `opt-in-not-required`"
//! (`AvailabilityZone$OptInStatus`). The zones themselves are AWS's published list (AWS
//! "Local Zones locations"), one entry per published group; which groups this account has
//! opted in to is the only part of it a caller can move, so that is the only part held in the
//! store ([`ComAmazonawsEc2Amazonec2State::opted_in_zone_groups`]).
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// `AvailabilityZone$ZoneType` for a Local Zone, as opposed to a standard Availability Zone
/// ([`ZONE_TYPE_AVAILABILITY_ZONE`]) or a Wavelength Zone. From the member's own doc ("Valid
/// values: `availability-zone` | `local-zone` | `wavelength-zone`").
pub const ZONE_TYPE_LOCAL_ZONE: &str = "local-zone";

/// One published Local Zone group of one Region: the group's own suffix, the component its
/// zone ids carry, and how many zones the group publishes.
///
/// The group's name is `<region>-<suffix>` and its zones are `<region>-<suffix><letter>`,
/// which is the form AWS publishes them in (`us-west-2-lax-1a` in the `us-west-2-lax-1`
/// group), with the zone ids `<zone-id prefix of the Region>-<id component>-az<n>`
/// (`usw2-lax1-az1`) the same list gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalZoneGroupSpec {
    /// The Region the group extends.
    pub region: &'static str,
    /// The group-name suffix, as AWS publishes it: `lax-1` for the Los Angeles group.
    pub suffix: &'static str,
    /// The component that group's zone ids carry: `lax1` for the same group.
    pub id_component: &'static str,
    /// The zone letters the group publishes, in order.
    pub letters: &'static [&'static str],
}

/// The Local Zone groups this emulator's Regions publish, from AWS's published list of Local
/// Zones and their zone names and ids (AWS "Local Zones locations": Los Angeles and Phoenix
/// extend `us-west-2`, Boston and Atlanta extend `us-east-1`, Hamburg extends
/// `eu-central-1`). Los Angeles is the one group AWS publishes TWO zones in, which is why its
/// row carries two letters: a group is opted in whole, so a group with more than one zone is
/// part of what that means.
///
/// A Region with no row publishes no Local Zone, which is most of them.
pub const PUBLISHED_LOCAL_ZONE_GROUPS: &[LocalZoneGroupSpec] = &[
    LocalZoneGroupSpec {
        region: "us-west-2",
        suffix: "lax-1",
        id_component: "lax1",
        letters: &["a", "b"],
    },
    LocalZoneGroupSpec {
        region: "us-west-2",
        suffix: "phx-2",
        id_component: "phx2",
        letters: &["a"],
    },
    LocalZoneGroupSpec {
        region: "us-east-1",
        suffix: "bos-1",
        id_component: "bos1",
        letters: &["a"],
    },
    LocalZoneGroupSpec {
        region: "us-east-1",
        suffix: "atl-1",
        id_component: "atl1",
        letters: &["a"],
    },
    LocalZoneGroupSpec {
        region: "eu-central-1",
        suffix: "ham-1",
        id_component: "ham1",
        letters: &["a"],
    },
];

impl LocalZoneGroupSpec {
    /// The group's name, which `ModifyAvailabilityZoneGroup` names it by and every zone of it
    /// reports as its `GroupName`: `<region>-<suffix>`.
    pub fn group_name(&self) -> String {
        format!("{region}-{suffix}", region = self.region, suffix = self.suffix)
    }
}

/// The name of every Local Zone group `region` publishes, in the order
/// [`PUBLISHED_LOCAL_ZONE_GROUPS`] lists them.
pub fn local_zone_group_names(region: &str) -> Vec<String> {
    PUBLISHED_LOCAL_ZONE_GROUPS
        .iter()
        .filter(|group| group.region == region)
        .map(LocalZoneGroupSpec::group_name)
        .collect()
}

impl ComAmazonawsEc2Amazonec2State {
    /// Whether this account has opted the zone group `group_name` in.
    ///
    /// The ONE reading of that fact, so the describe that reports a zone's `OptInStatus` and
    /// the modify that moves it cannot disagree.
    pub fn zone_group_is_opted_in(&self, group_name: &str) -> bool {
        self.opted_in_zone_groups.contains(group_name)
    }

    /// Opt the zone group `group_name` in, or out, and answer whether that CHANGED anything.
    ///
    /// "Changes the opt-in status of the specified zone group for your account"
    /// (`ModifyAvailabilityZoneGroup`): the status is a fact of the account, so it is stored
    /// per group rather than per zone — a group's zones are opted in together.
    pub fn set_zone_group_opt_in(&mut self, group_name: &str, opted_in: bool) -> bool {
        if opted_in {
            self.opted_in_zone_groups.insert(group_name.to_string())
        } else {
            self.opted_in_zone_groups.remove(group_name)
        }
    }

    /// Every zone `region` publishes, as this account sees it: the Region's standard
    /// Availability Zones ([`availability_zones_in_region`]) followed by the Local Zones of
    /// each group it publishes, each carrying the opt-in status this account's
    /// `ModifyAvailabilityZoneGroup` calls left its group in.
    ///
    /// The ONE place the two kinds of zone are brought together, so a request that asks for
    /// every zone and one that asks for the opted-in ones read the same list.
    pub fn zones_in_region(&self, region: &str) -> Vec<AvailabilityZoneRecord> {
        let mut zones = availability_zones_in_region(region);
        let prefix = zone_id_prefix(region);
        // The zone that "handles some of the Local Zone … control plane operations"
        // (`AvailabilityZone$ParentZoneName`) is a standard zone of the parent Region; the
        // first one it publishes is the one every group of this catalog hangs off.
        let parent = zones
            .first()
            .map(|zone| (zone.zone_name.clone(), zone.zone_id.clone()));
        for group in PUBLISHED_LOCAL_ZONE_GROUPS
            .iter()
            .filter(|group| group.region == region)
        {
            let group_name = group.group_name();
            // A group is opted in WHOLE, so every zone of it reports the one status the
            // account holds for the group.
            let opt_in_status = if self.zone_group_is_opted_in(&group_name) {
                OPT_IN_STATUS_OPTED_IN
            } else {
                OPT_IN_STATUS_NOT_OPTED_IN
            };
            for (index, letter) in group.letters.iter().enumerate() {
                zones.push(AvailabilityZoneRecord {
                    zone_name: format!("{group_name}{letter}"),
                    zone_id: format!(
                        "{prefix}-{component}-az{number}",
                        component = group.id_component,
                        number = index + 1
                    ),
                    region_name: region.to_string(),
                    group_long_name: None,
                    group_name: group_name.clone(),
                    // A Local Zone advertises its addresses from its OWN network border
                    // group, which is the zone group itself rather than the Region — that is
                    // what `AllocateAddress`'s `NetworkBorderGroup` names for one.
                    network_border_group: group_name.clone(),
                    zone_type: ZONE_TYPE_LOCAL_ZONE,
                    parent_zone: parent.clone(),
                    state: ZONE_STATE_AVAILABLE,
                    opt_in_status,
                    messages: Vec::new(),
                    geography: None,
                    sub_geography: None,
                });
            }
        }
        zones
    }
}

/// Whether a zone of `opt_in_status` is reported to a request that did not ask for every
/// zone.
///
/// "Include all Availability Zones, Local Zones, and Wavelength Zones regardless of your opt
/// in status. If you do not use this parameter, the results include only the zones for the
/// Regions where you have chosen the option to opt in"
/// (`DescribeAvailabilityZonesRequest$AllAvailabilityZones`): a zone whose group this account
/// has not opted in to is withheld, and a zone that needs no opt-in at all is always
/// reported.
pub fn zone_is_reported_without_opt_in(opt_in_status: &str) -> bool {
    opt_in_status != OPT_IN_STATUS_NOT_OPTED_IN
}

/// The code EC2 answers for a zone group name that names no opt-in zone group of this
/// account's Region: `InvalidAZGroup.NotFound`, the `Invalid<Kind>.NotFound` convention this
/// service words every unresolvable resource with.
pub const INVALID_AZ_GROUP_NOT_FOUND: &str = "InvalidAZGroup.NotFound";

/// The sentence EC2 answers beside [`INVALID_AZ_GROUP_NOT_FOUND`].
///
/// RECORDED: FIXED text — it names neither the group nor the Region, so nothing of the
/// request is interpolated into it, which is also why this is the whole wording for this
/// error class rather than a template.
pub const INVALID_AZ_GROUP_MESSAGE: &str = "Invalid Availability Zone group provided";

/// Refuse a zone group name that names no opt-in zone group of this Region: HTTP 400
/// [`INVALID_AZ_GROUP_NOT_FOUND`] with [`INVALID_AZ_GROUP_MESSAGE`].
pub fn invalid_az_group_not_found<E>() -> Scaffolded<E> {
    Scaffolded::service_error(INVALID_AZ_GROUP_NOT_FOUND, 400, INVALID_AZ_GROUP_MESSAGE)
}
