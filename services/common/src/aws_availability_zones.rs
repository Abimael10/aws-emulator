// SPDX-License-Identifier: CC0-1.0

//! The Availability Zones a Region exposes, by name.
//!
//! A report that offers a resource "in the zones of this Region" — an orderable DB instance
//! option, a cluster's placement — names them, so the names have to be the Region's own:
//! most Regions expose three zones, several expose more, and a Region's zone letters are not
//! always the first letters of the alphabet. A report that assumed three letters refused a
//! zone that is in fact orderable in the Region, and offered one that is not.

/// The zone letters a Region exposes when its own entry does not say otherwise: every AWS
/// Region exposes at least three zones, named `<region><letter>`.
const DEFAULT_ZONE_LETTERS: &[&str] = &["a", "b", "c"];

/// The Regions whose published zone names are NOT the default three, with the letters they
/// expose, in the order AWS lists them.
///
/// Source: the zone names AWS publishes for each Region ("Regions and Zones", and the
/// `DescribeAvailabilityZones` answer for the Region). Only the Regions that differ are
/// listed; every other Region answers [`DEFAULT_ZONE_LETTERS`], so this table states a
/// difference rather than restating the rule.
const ZONE_LETTERS: &[(&str, &[&str])] = &[
    // The oldest Region, which exposes six zones.
    ("us-east-1", &["a", "b", "c", "d", "e", "f"]),
    ("us-west-2", &["a", "b", "c", "d"]),
    // A Region whose second letter names no zone: its three zones are 1a, 1c and 1d.
    ("ap-northeast-1", &["a", "c", "d"]),
];

/// The Availability Zone NAMES of `region`, in the order AWS lists them.
///
/// Matching is case-insensitive, as a Region name read off a credential scope is.
pub fn availability_zones(region: &str) -> Vec<String> {
    zone_letters(region)
        .iter()
        .map(|letter| format!("{region}{letter}"))
        .collect()
}

/// The zone LETTERS of `region`: its own entry when [`ZONE_LETTERS`] carries one, else
/// [`DEFAULT_ZONE_LETTERS`].
pub fn zone_letters(region: &str) -> &'static [&'static str] {
    ZONE_LETTERS
        .iter()
        .find(|(named, _)| named.eq_ignore_ascii_case(region))
        .map(|(_, letters)| *letters)
        .unwrap_or(DEFAULT_ZONE_LETTERS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Region's zones are named `<region><letter>` over the letters that Region exposes:
    /// the published list where there is one, three zones otherwise.
    #[test]
    fn a_regions_zones_are_its_published_ones() {
        let cases: &[(&str, &[&str])] = &[
            (
                "us-east-1",
                &[
                    "us-east-1a",
                    "us-east-1b",
                    "us-east-1c",
                    "us-east-1d",
                    "us-east-1e",
                    "us-east-1f",
                ],
            ),
            (
                "us-west-2",
                &["us-west-2a", "us-west-2b", "us-west-2c", "us-west-2d"],
            ),
            (
                "ap-northeast-1",
                &["ap-northeast-1a", "ap-northeast-1c", "ap-northeast-1d"],
            ),
            // A Region this table does not name exposes the default three.
            ("eu-west-1", &["eu-west-1a", "eu-west-1b", "eu-west-1c"]),
            ("eu-central-1", &["eu-central-1a", "eu-central-1b", "eu-central-1c"]),
        ];
        for (region, expected) in cases {
            let zones = availability_zones(region);
            assert_eq!(
                zones, *expected,
                "case {region}: expected zones {expected:?}, got {zones:?}"
            );
        }
        assert_eq!(
            zone_letters("US-WEST-2"),
            zone_letters("us-west-2"),
            "a Region read off a credential scope matches whatever its case"
        );
    }
}
