// SPDX-License-Identifier: CC0-1.0

//! What one entry of a distribution configuration has to satisfy, and how an entry this
//! service cannot accept is refused.
//!
//! Two requirements of a `Distribution` entry live in the model's prose rather than in a
//! constraint, so the validating walk cannot refuse them and this module reads them:
//!
//! * the entry has to name a target Region this service can distribute through
//!   ([`super::distribution_target`]);
//! * `amiDistributionConfiguration.name` is "the name of the output AMI. The name must
//!   include the `{{ imagebuilder:buildDate }}` dynamic tag so that each build produces a
//!   uniquely named AMI"
//!   (`smithy_query(query="doc",shape="AmiDistributionConfiguration",member="name")`) — a
//!   requirement on the value's CONTENT, which the member's `@pattern` admits either way, so
//!   a name the tag is missing from is refused rather than stored.
//!
//! Each is a value of ONE member of ONE entry, so a refusal names that member at the entry's
//! own position ([`RefusedEntry::message`]) and the entries are read in the order the request
//! wrote them.

use crate::ops::types::Distribution;

/// The member the per-Region settings of a distribution configuration arrive in, as the API
/// addresses it.
const DISTRIBUTIONS_MEMBER: &str = "distributions";

/// The member a target Region's AMI settings sit at, as the API addresses it.
///
/// Every target Region of an image distribution carries AMI settings — the entry's
/// `amiDistributionConfiguration`, which Image Builder resolves whether or not the request
/// spelled it out — so that is the member whose settings a target this service cannot reach
/// cannot be applied to, and the member a refusal of such a target names.
const AMI_DISTRIBUTION_CONFIGURATION_MEMBER: &str = "amiDistributionConfiguration";

/// The member an output AMI's name sits at inside those settings, as the API addresses it.
const AMI_NAME_MEMBER: &str = "name";

/// The dynamic tag an output AMI's name has to include, as the member's own documentation
/// writes it.
const BUILD_DATE_TAG: &str = "{{ imagebuilder:buildDate }}";

/// That tag with the whitespace inside its braces removed, which is the form a name is
/// compared in: the tag is a template expression, so that whitespace is not part of it.
const COMPACT_BUILD_DATE_TAG: &str = "{{imagebuilder:buildDate}}";

/// One entry of `distributions` this service cannot accept: where it sits, and which of its
/// requirements it broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefusedEntry<'entry> {
    /// The zero-based position of the entry on the `distributions` member, which is how the
    /// API addresses an element of a list.
    pub index: usize,
    /// The requirement that entry broke.
    pub why: RefusedEntryRequirement<'entry>,
}

/// Which requirement of a distribution entry a request broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedEntryRequirement<'entry> {
    /// `region` names no Region of this partition, so the entry has no target to distribute
    /// through. Carries the value the request spelled.
    UnreachableTarget(&'entry str),
    /// `amiDistributionConfiguration.name` does not include the build-date tag, so the output
    /// AMIs of this target would not be uniquely named.
    AmiNameWithoutBuildDate,
}

/// Which requirement of an entry of `distributions` a request broke, with the position of the
/// entry that broke it, or `None` when every entry satisfies both.
///
/// Entries are read in order, and within one entry the TARGET is read first: an entry naming
/// no Region has no target for any of its settings to apply to.
#[must_use]
pub fn refused_distribution_entry(distributions: &[Distribution]) -> Option<RefusedEntry<'_>> {
    distributions.iter().enumerate().find_map(|(index, entry)| {
        broken_requirement(entry).map(|why| RefusedEntry { index, why })
    })
}

impl RefusedEntry<'_> {
    /// The sentence this service refuses the entry in.
    ///
    /// Stated in the sentence every refused parameter of this service is stated in
    /// (`service_common::validation_message::parameter_value_mismatch`): the member of the
    /// entry whose value was refused, at the position the entry sits at, and then the
    /// requirement that value had to satisfy.
    #[must_use]
    pub fn message(&self) -> String {
        let (member, requirement) = match self.why {
            RefusedEntryRequirement::UnreachableTarget(region) => (
                AMI_DISTRIBUTION_CONFIGURATION_MEMBER.to_owned(),
                super::unavailable_distribution_target_requirement(region),
            ),
            RefusedEntryRequirement::AmiNameWithoutBuildDate => (
                format!("{AMI_DISTRIBUTION_CONFIGURATION_MEMBER}.{AMI_NAME_MEMBER}"),
                format!(
                    "The EC2 AMI name must contain '{BUILD_DATE_TAG}' to ensure that the name \
                     will be unique."
                ),
            ),
        };
        ::service_common::validation_message::parameter_value_mismatch(
            &format!("{DISTRIBUTIONS_MEMBER}[{}].{member}", self.index),
            &requirement,
        )
    }
}

/// Which requirement `entry` broke, or `None` when it satisfies both.
fn broken_requirement(entry: &Distribution) -> Option<RefusedEntryRequirement<'_>> {
    if super::distribution_target(entry.region.as_str()).is_none() {
        return Some(RefusedEntryRequirement::UnreachableTarget(
            entry.region.as_str(),
        ));
    }
    let named = entry
        .ami_distribution_configuration
        .as_ref()
        .and_then(|settings| settings.name.as_deref());
    match named {
        Some(name) if !names_each_build(name) => {
            Some(RefusedEntryRequirement::AmiNameWithoutBuildDate)
        }
        _ => None,
    }
}

/// Whether `name` includes the build-date tag, so each build of the image produces a
/// uniquely named AMI.
///
/// Compared with the whitespace inside the braces removed, so a name writing the tag
/// compactly names each build as surely as one writing it the way the documentation does.
fn names_each_build(name: &str) -> bool {
    let compact: String = name.chars().filter(|value| !value.is_whitespace()).collect();
    compact.contains(COMPACT_BUILD_DATE_TAG)
}

#[cfg(test)]
mod tests {
    //! The requirements an entry is read by and the sentences they are refused in. The wire
    //! behaviour — that `CreateDistributionConfiguration` answers them — is asserted through
    //! the router in `tests/all/distribution_target_regions.rs` and
    //! `tests/all/distribution_ami_names.rs`.

    use super::*;
    use crate::ops::types::AmiDistributionConfiguration;

    /// One distribution entry naming `region` as its target, with `ami_name` as its output
    /// AMI name when it names one at all.
    fn entry(region: &str, ami_name: Option<&str>) -> Distribution {
        Distribution {
            region: region.to_owned(),
            ami_distribution_configuration: ami_name.map(|name| AmiDistributionConfiguration {
                name: Some(name.to_owned()),
                ..AmiDistributionConfiguration::default()
            }),
            ..Distribution::default()
        }
    }

    /// One row of the table below: the case, the entries as (target, AMI name), and the
    /// entry a refusal addresses.
    type Row<'row> = (
        &'row str,
        Vec<(&'row str, Option<&'row str>)>,
        Option<RefusedEntry<'row>>,
    );

    /// RULE: entries are read in order and the first one that breaks a requirement is the one
    /// a refusal addresses, by its own position; within an entry the target is read first.
    #[test]
    fn the_first_entry_that_breaks_a_requirement_is_the_one_reported() {
        // (case, the entries as (target, AMI name), the entry a refusal addresses)
        let rows: Vec<Row<'_>> = vec![
            ("one well-formed entry", vec![("us-west-2", None)], None),
            (
                "three well-formed entries",
                vec![
                    ("us-east-1", Some("golden-{{ imagebuilder:buildDate }}")),
                    ("eu-central-1", None),
                    ("ap-southeast-3", Some("{{imagebuilder:buildDate}}-app")),
                ],
                None,
            ),
            (
                "a target in upper case",
                vec![("US-EAST-2", None)],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("US-EAST-2"),
                }),
            ),
            (
                "a well-formed entry before a target in mixed case",
                vec![("us-west-2", None), ("Eu-West-1", None)],
                Some(RefusedEntry {
                    index: 1,
                    why: RefusedEntryRequirement::UnreachableTarget("Eu-West-1"),
                }),
            ),
            (
                "a target carrying a separator an authority would re-point on",
                vec![("us-west-2/../elsewhere", None)],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("us-west-2/../elsewhere"),
                }),
            ),
            (
                "a target longer than the field admits",
                vec![("us-west-2-us-west-2-us", None)],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("us-west-2-us-west-2-us"),
                }),
            ),
            (
                "an AMI name without the build-date tag",
                vec![("us-west-2", Some("golden"))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                }),
            ),
            (
                "an AMI name naming another dynamic tag only",
                vec![("us-west-2", Some("golden-{{ imagebuilder:buildVersion }}"))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                }),
            ),
            (
                "the second entry's AMI name, after a well-formed one",
                vec![
                    ("us-west-2", Some("first-{{ imagebuilder:buildDate }}")),
                    ("eu-west-1", Some("second")),
                ],
                Some(RefusedEntry {
                    index: 1,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                }),
            ),
            (
                "an entry breaking both requirements, whose target is reported",
                vec![("US-EAST-2", Some("golden"))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("US-EAST-2"),
                }),
            ),
        ];

        for (case, entries, expected) in rows {
            let distributions: Vec<Distribution> = entries
                .iter()
                .map(|(region, ami_name)| entry(region, *ami_name))
                .collect();
            let actual = refused_distribution_entry(&distributions);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the refusal names the refused member of the entry at the entry's own position,
    /// and states the requirement that member's value had to satisfy.
    #[test]
    fn the_refusal_names_the_entrys_member_and_its_requirement() {
        // (case, the refused entry, the sentence)
        let rows = [
            (
                "the first entry's target",
                RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("US-EAST-2"),
                },
                "The value supplied for parameter \
                 'distributions[0].amiDistributionConfiguration' is not valid. AMI \
                 Distribution is not available in 'US-EAST-2'.",
            ),
            (
                "the third entry's target",
                RefusedEntry {
                    index: 2,
                    why: RefusedEntryRequirement::UnreachableTarget("Eu-West-1"),
                },
                "The value supplied for parameter \
                 'distributions[2].amiDistributionConfiguration' is not valid. AMI \
                 Distribution is not available in 'Eu-West-1'.",
            ),
            (
                "the first entry's AMI name",
                RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                },
                "The value supplied for parameter \
                 'distributions[0].amiDistributionConfiguration.name' is not valid. The EC2 \
                 AMI name must contain '{{ imagebuilder:buildDate }}' to ensure that the name \
                 will be unique.",
            ),
            (
                "the second entry's AMI name",
                RefusedEntry {
                    index: 1,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                },
                "The value supplied for parameter \
                 'distributions[1].amiDistributionConfiguration.name' is not valid. The EC2 \
                 AMI name must contain '{{ imagebuilder:buildDate }}' to ensure that the name \
                 will be unique.",
            ),
        ];

        for (case, refused, expected) in rows {
            let actual = refused.message();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
