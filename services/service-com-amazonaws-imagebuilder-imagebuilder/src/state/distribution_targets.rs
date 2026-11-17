// SPDX-License-Identifier: CC0-1.0

//! Which target Region a distribution configuration may name, and how a target this service
//! cannot distribute through is worded.
//!
//! A `Distribution` entry is "the settings for a specific Region"
//! (`smithy_query(query="shape",shape="Distribution")`) and its `region` member — "The
//! target Region" — is a bare `NonEmptyString`, so the model cannot say that the value has
//! to NAME a Region at all. Image Builder resolves it to the endpoint it distributes the
//! output AMI through, so a value that names no Region of this partition has nowhere to
//! distribute to, and the entry is refused rather than stored.
//!
//! MEMBERSHIP is read against this tree's ONE Region catalog
//! (`emulator_telemetry::policy::KNOWN_REGIONS`, "every AWS region the wire may carry",
//! "sorted for binary search"), the same catalog every other Region-valued input of this
//! emulator is read against, rather than a second list of Region codes kept here: a value
//! shaped like a Region label but naming no Region — a Region code with a digit changed, a
//! label of the right form that AWS has never opened — reaches no endpoint either, so the
//! grammar alone cannot decide a target.
//!
//! The catalog spans every partition, and a distribution reaches the Regions of the
//! partition the request itself was signed for, which for this emulator is the commercial
//! one its endpoints and its signing scope name; the Regions of the other partitions are
//! told apart by their own codes ([`in_commercial_partition`]).

/// The target Region of `region` when it names one at all, and `None` otherwise.
///
/// A target is a Region of the commercial partition that this tree's catalog holds, compared
/// as the request spelled it: a Region code in another case, a value carrying a separator an
/// endpoint authority would re-point on, a label longer than a Region field admits, a
/// well-formed label that names no Region and a Region of another partition all name no
/// target of this one.
#[must_use]
pub fn distribution_target(region: &str) -> Option<&'static str> {
    let catalog = ::emulator_telemetry::policy::KNOWN_REGIONS;
    let at = catalog.binary_search(&region).ok()?;
    let known = catalog[at];
    in_commercial_partition(known).then_some(known)
}

/// Whether the Region `region` belongs to the commercial (`aws`) partition — the partition
/// this emulator's endpoints and signing scope name.
///
/// A Region code names its partition itself: the China Regions open on `cn-`, the GovCloud
/// ones carry a `gov` field, and the isolated ones carry a field opening `iso` (`us-iso-`,
/// `us-isob-`, `eu-isoe-`). Every other code in the catalog is a commercial Region, so a
/// Region added to the catalog needs no second table kept in step here.
#[must_use]
pub fn in_commercial_partition(region: &str) -> bool {
    !(region.starts_with("cn-")
        || region
            .split('-')
            .any(|field| field == "gov" || field.starts_with("iso")))
}

/// The requirement clause this service states for a target Region it cannot distribute
/// through: that AMI distribution is not available there, quoting the target as the request
/// spelled it.
///
/// The caller has to see WHICH of its targets was refused, and the target is the only part
/// of the request the clause carries.
#[must_use]
pub fn unavailable_distribution_target_requirement(region: &str) -> String {
    format!("AMI Distribution is not available in '{region}'.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a target is a Region of the commercial partition that the catalog holds, and
    /// every other value names none.
    #[test]
    fn only_a_commercial_region_of_the_catalog_names_a_target() {
        // (case, the value the entry carried, whether it names a target)
        let rows = [
            ("a Region code", "us-west-2", true),
            ("a Region code of three hyphenated words", "ap-southeast-3", true),
            // Regions the fixtures of this service never name, read straight off the shared
            // catalog: a target is decided by that catalog, not by one recording.
            ("a Region in Africa", "af-south-1", true),
            ("a Region in the Middle East", "me-central-1", true),
            ("a Region in Mexico", "mx-central-1", true),
            ("a Region in Asia Pacific the catalog added", "ap-southeast-6", true),
            ("a label of a Region that does not exist", "us-nowhere-1", false),
            ("a Region code with its number changed", "us-west-9", false),
            ("a Region code in upper case", "US-EAST-2", false),
            ("a Region code in mixed case", "Eu-West-1", false),
            ("a value carrying a path separator", "us-west-2/../elsewhere", false),
            ("a value carrying a port separator", "us-west-2:443", false),
            ("a label longer than a Region field admits", "us-west-2-us-west-201", false),
            ("an empty value", "", false),
            ("a word that is not a Region code", "Oregon", false),
            // Another partition's Regions: the catalog holds them, and a distribution of
            // this one reaches none of them.
            ("a China Region", "cn-north-1", false),
            ("a GovCloud Region", "us-gov-west-1", false),
            ("an isolated Region", "us-iso-east-1", false),
            ("another isolated Region", "us-isob-east-1", false),
            ("an isolated Region in Europe", "eu-isoe-west-1", false),
        ];

        for (case, value, expected) in rows {
            let actual = distribution_target(value).is_some();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected names-a-target {expected}, got {actual}"
            );
        }
    }

    /// RULE: every Region the catalog holds is read as being of the commercial partition
    /// except the ones whose own code names another, so the two readings cover the catalog
    /// between them and neither needs a list of its own.
    #[test]
    fn the_catalog_is_sorted_and_splits_into_the_partitions_it_names() {
        let catalog = ::emulator_telemetry::policy::KNOWN_REGIONS;
        let mut sorted = catalog.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, catalog,
            "expected the catalog sorted and free of duplicates, so a target is found by one \
             ordered search, got {catalog:?}"
        );

        let commercial = catalog
            .iter()
            .filter(|region| in_commercial_partition(region))
            .count();
        let other = catalog.len() - commercial;
        assert!(
            commercial > 20 && other > 5,
            "expected the catalog to hold both the commercial Regions and the other \
             partitions', read {commercial} commercial and {other} other of {}",
            catalog.len()
        );
        for region in catalog.iter().filter(|region| !in_commercial_partition(region)) {
            assert!(
                region.starts_with("cn-")
                    || region.contains("-gov-")
                    || region.contains("-iso"),
                "case {region:?}: expected a Region outside the commercial partition to name \
                 it in its own code, got {region:?}"
            );
        }
    }

    /// RULE: the clause states that AMI distribution is unavailable there and quotes the
    /// target as the request spelled it.
    #[test]
    fn the_clause_quotes_the_target_it_refuses() {
        for region in ["US-EAST-2", "Eu-West-1", "Oregon", "us-nowhere-1", "cn-north-1"] {
            let actual = unavailable_distribution_target_requirement(region);
            let expected = format!("AMI Distribution is not available in '{region}'.");
            assert_eq!(
                actual, expected,
                "case {region:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
