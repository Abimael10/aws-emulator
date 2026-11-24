// SPDX-License-Identifier: CC0-1.0

//! What one entry of a distribution configuration has to satisfy, and how an entry this
//! service cannot accept is refused.
//!
//! Three requirements of a `Distribution` entry live in the model's prose rather than in a
//! constraint, so the validating walk cannot refuse them and this module reads them:
//!
//! * the entry has to configure at least one OUTPUT for its target: `Distribution` "defines
//!   the settings for a specific Region" and each entry of `distributions` "defines how
//!   output images are distributed in one target Amazon Web Services Region"
//!   (`smithy_query(query="shape",shape="Distribution")`), so an entry carrying its target
//!   Region and none of the members that distribute an output there describes no distribution
//!   at all and is refused ([`configures_an_output`]);
//! * the entry has to name a target Region this service can distribute through
//!   ([`super::distribution_target`]);
//! * `amiDistributionConfiguration.name` is "the name of the output AMI. The name must
//!   include the `{{ imagebuilder:buildDate }}` dynamic tag so that each build produces a
//!   uniquely named AMI"
//!   (`smithy_query(query="doc",shape="AmiDistributionConfiguration",member="name")`) — a
//!   requirement on the value's CONTENT, which the member's `@pattern` admits either way, so
//!   a name the tag is missing from is refused rather than stored.
//!
//! `distributions` also carries a requirement of its own, which holds over the list as a WHOLE
//! rather than over any one entry: "A Region can appear at most once in the list"
//! (`smithy_query(query="operation",operation="CreateDistributionConfiguration")`), read once
//! every entry has been read on its own ([`refused_distributions`]).
//!
//! A refusal names the parameter whose value it refused at the offending member's own path
//! ([`RefusedDistributions::message`]) — the list itself when the list as a whole breaks its
//! own requirement, the entry when the entry as a whole configures no output, and the entry's
//! own member when one member's value is what broke a requirement — and the entries are read in
//! the order the request wrote them.
//!
//! Which CLASS of parameter refusal carries that sentence follows what is wrong with the value
//! ([`RefusedParameterClass`]), not which requirement it broke.

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

/// The requirement the `distributions` list as a whole has to satisfy, as the member's own
/// documentation states it: "A Region can appear at most once in the list"
/// (`smithy_query(query="operation",operation="CreateDistributionConfiguration")`).
///
/// FIXED text: the requirement is what the member admits, so neither the Region the request
/// repeated nor the position it repeated it at appears in it.
const AT_MOST_ONE_ENTRY_PER_REGION_REQUIREMENT: &str =
    "A region may appear in a distribution configuration at most once.";

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
    /// The entry carries none of the members that distribute an output
    /// ([`configures_an_output`]), so it distributes nothing to the target it names.
    WithoutOutputs,
    /// `region` names no Region of this partition, so the entry has no target to distribute
    /// through. Carries the value the request spelled.
    UnreachableTarget(&'entry str),
    /// `amiDistributionConfiguration.name` does not include the build-date tag, so the output
    /// AMIs of this target would not be uniquely named.
    AmiNameWithoutBuildDate,
}

/// Why this service cannot accept the `distributions` a request carried: one of the
/// requirements an ENTRY is read by, or the requirement the list as a whole is read by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedDistributions<'list> {
    /// One entry broke one of its own requirements ([`refused_distribution_entry`]).
    Entry(RefusedEntry<'list>),
    /// The list names one target Region more than once, which "A Region can appear at most
    /// once in the list" does not admit. Carries nothing of the request: the sentence names
    /// the member and the requirement only.
    TargetNamedTwice,
}

/// Which CLASS of parameter refusal a `distributions` value this service cannot accept is
/// answered in.
///
/// Read from what is wrong with the value rather than from which requirement it broke: a value
/// that breaks a requirement of the member it sits on is refused as that VALUE, while a value
/// admissible on its own that cannot be combined with another member of the request is refused
/// as that COMBINATION — "two mutually exclusive parameters, or a parameter without its
/// required companion parameter"
/// (`smithy_query(query="shape",shape="InvalidParameterCombinationException")`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedParameterClass {
    /// The parameter's own value is one this service does not accept.
    Value,
    /// The parameter's value is admissible on its own and cannot be combined with another
    /// member of the request.
    Combination,
}

/// Why this service cannot accept `distributions`, or `None` when every entry satisfies the
/// requirements an entry is read by and the list satisfies its own.
///
/// The ENTRIES are read first, each on its own and in the order the request wrote them, and the
/// list is read as a SET only once every entry has been accepted: an entry that describes no
/// distribution at all is refused as that entry, whether or not another entry names its target
/// too. A create and an update read their lists through this one reading, so one wording rule
/// and one refusal class are applied on every path.
#[must_use]
pub fn refused_distributions(distributions: &[Distribution]) -> Option<RefusedDistributions<'_>> {
    if let Some(refused) = refused_distribution_entry(distributions) {
        return Some(RefusedDistributions::Entry(refused));
    }
    duplicate_distribution_region(distributions).map(|_| RefusedDistributions::TargetNamedTwice)
}

impl RefusedDistributions<'_> {
    /// The sentence this service refuses the list in, stated in the ONE sentence every refused
    /// parameter of this service is stated in
    /// (`service_common::validation_message::parameter_value_mismatch`): the path of the member
    /// whose value was refused, and then the requirement that value had to satisfy.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Entry(entry) => entry.message(),
            Self::TargetNamedTwice => ::service_common::validation_message::parameter_value_mismatch(
                DISTRIBUTIONS_MEMBER,
                AT_MOST_ONE_ENTRY_PER_REGION_REQUIREMENT,
            ),
        }
    }

    /// Which class of parameter refusal carries that sentence.
    #[must_use]
    pub fn class(&self) -> RefusedParameterClass {
        match self {
            // A target this service cannot distribute through refuses the entry's AMI
            // settings, which are admissible on their own, TOGETHER with the Region they would
            // apply in.
            Self::Entry(RefusedEntry {
                why: RefusedEntryRequirement::UnreachableTarget(_),
                ..
            }) => RefusedParameterClass::Combination,
            // Everything else is a value of `distributions` — of one entry, of a member inside
            // one entry, or of the list as a whole — that breaks a requirement of the member it
            // sits on.
            Self::Entry(_) | Self::TargetNamedTwice => RefusedParameterClass::Value,
        }
    }
}

/// Which requirement of an entry of `distributions` a request broke, with the position of the
/// entry that broke it, or `None` when every entry satisfies all three.
///
/// Entries are read in order, and within one entry the OUTPUTS are read first: an entry that
/// configures no output has no setting for a target to apply to, and the target is read
/// before the values of the outputs that would apply in it.
#[must_use]
pub fn refused_distribution_entry(distributions: &[Distribution]) -> Option<RefusedEntry<'_>> {
    distributions.iter().enumerate().find_map(|(index, entry)| {
        broken_requirement(entry).map(|why| RefusedEntry { index, why })
    })
}

/// The target Region `distributions` names more than once, when it names one twice at all.
///
/// `distributions`' own rule: "Each entry defines how output images are distributed in one
/// target Amazon Web Services Region. A Region can appear at most once in the list." Two
/// entries for one Region are a value of `distributions` that breaks the list's own
/// requirement — so a create and an update read their lists as a SET through this one reading,
/// after each entry has been read on its own ([`refused_distributions`]).
#[must_use]
pub fn duplicate_distribution_region(distributions: &[Distribution]) -> Option<&str> {
    let mut targets: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();
    distributions
        .iter()
        .find(|entry| !targets.insert(entry.region.as_str()))
        .map(|entry| entry.region.as_str())
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
        let entry = format!("{DISTRIBUTIONS_MEMBER}[{}]", self.index);
        let (parameter, requirement) = match self.why {
            RefusedEntryRequirement::WithoutOutputs => (
                entry,
                "One or more outputs should be provided for each region in a distribution \
                 configuration."
                    .to_owned(),
            ),
            RefusedEntryRequirement::UnreachableTarget(region) => (
                format!("{entry}.{AMI_DISTRIBUTION_CONFIGURATION_MEMBER}"),
                super::unavailable_distribution_target_requirement(region),
            ),
            RefusedEntryRequirement::AmiNameWithoutBuildDate => (
                format!("{entry}.{AMI_DISTRIBUTION_CONFIGURATION_MEMBER}.{AMI_NAME_MEMBER}"),
                format!(
                    "The EC2 AMI name must contain '{BUILD_DATE_TAG}' to ensure that the name \
                     will be unique."
                ),
            ),
        };
        ::service_common::validation_message::parameter_value_mismatch(&parameter, &requirement)
    }
}

/// Which requirement `entry` broke, or `None` when it satisfies all of them.
fn broken_requirement(entry: &Distribution) -> Option<RefusedEntryRequirement<'_>> {
    if !configures_an_output(entry) {
        return Some(RefusedEntryRequirement::WithoutOutputs);
    }
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

/// Whether `entry` carries at least one of the members that distribute an output of a build to
/// its target Region: every member of `Distribution` but `region`, which names the target
/// those outputs are distributed to rather than an output of its own —
/// `amiDistributionConfiguration`, `containerDistributionConfiguration`,
/// `licenseConfigurationArns`, `launchTemplateConfigurations`, `s3ExportConfiguration`,
/// `fastLaunchConfigurations` and `ssmParameterConfigurations`
/// (`smithy_query(query="shape",shape="Distribution")`).
///
/// A member whose value is a STRUCTURE names that output whatever it holds: every member
/// inside `amiDistributionConfiguration`, `containerDistributionConfiguration`'s description
/// and tags and `s3ExportConfiguration`'s prefix are optional, and Image Builder resolves the
/// settings a request leaves out, so settings carrying none of their own members still name
/// the output they are the settings for. A member whose value is a LIST names as many outputs
/// as it holds, so one that holds nothing names none — a list of no launch templates
/// configures no launch template, the reading the modelled `length` bound on each of those
/// lists (min 1) takes of a list that holds nothing.
fn configures_an_output(entry: &Distribution) -> bool {
    entry.ami_distribution_configuration.is_some()
        || entry.container_distribution_configuration.is_some()
        || entry.s3_export_configuration.is_some()
        || entry
            .license_configuration_arns
            .as_ref()
            .is_some_and(|arns| !arns.is_empty())
        || entry
            .launch_template_configurations
            .as_ref()
            .is_some_and(|configurations| !configurations.is_empty())
        || entry
            .fast_launch_configurations
            .as_ref()
            .is_some_and(|configurations| !configurations.is_empty())
        || entry
            .ssm_parameter_configurations
            .as_ref()
            .is_some_and(|configurations| !configurations.is_empty())
}

/// Whether `name` includes the build-date tag, so each build of the image produces a
/// uniquely named AMI.
///
/// Read through this crate's ONE reading of that tag ([`super::carries_build_date_tag`]),
/// which is also what expands it into the name of the AMI a build produces — so a name this
/// create accepts is one the build path names each build by.
fn names_each_build(name: &str) -> bool {
    super::carries_build_date_tag(name)
}

#[cfg(test)]
mod tests {
    //! The requirements an entry is read by and the sentences they are refused in. The wire
    //! behaviour — that `CreateDistributionConfiguration` answers them — is asserted through
    //! the router in `tests/all/distribution_target_regions.rs` and
    //! `tests/all/distribution_ami_names.rs`.

    use super::*;
    use crate::ops::types::{
        AmiDistributionConfiguration, ContainerDistributionConfiguration, FastLaunchConfiguration,
        LaunchTemplateConfiguration, S3ExportConfiguration, SsmParameterConfiguration,
        TargetContainerRepository,
    };

    /// What one entry of the tables below carries besides its target Region: which member
    /// that distributes an output it names, and how many entries that member holds when it is
    /// a list.
    #[derive(Debug, Clone, Copy)]
    enum Outputs<'case> {
        /// None of them: the entry names a target and distributes nothing to it.
        Nothing,
        /// `amiDistributionConfiguration`, carrying the output AMI name when it names one.
        Ami(Option<&'case str>),
        /// `containerDistributionConfiguration`.
        Container,
        /// `licenseConfigurationArns`, holding that many ARNs.
        Licenses(usize),
        /// `launchTemplateConfigurations`, holding that many configurations.
        LaunchTemplates(usize),
        /// `s3ExportConfiguration`.
        S3Export,
        /// `fastLaunchConfigurations`, holding that many configurations.
        FastLaunch(usize),
        /// `ssmParameterConfigurations`, holding that many configurations.
        SsmParameters(usize),
    }

    /// One distribution entry naming `region` as its target and carrying `outputs`.
    fn entry(region: &str, outputs: Outputs<'_>) -> Distribution {
        let mut entry = Distribution {
            region: region.to_owned(),
            ..Distribution::default()
        };
        match outputs {
            Outputs::Nothing => {}
            Outputs::Ami(name) => {
                entry.ami_distribution_configuration = Some(AmiDistributionConfiguration {
                    name: name.map(str::to_owned),
                    ..AmiDistributionConfiguration::default()
                });
            }
            Outputs::Container => {
                entry.container_distribution_configuration =
                    Some(ContainerDistributionConfiguration {
                        target_repository: TargetContainerRepository {
                            service: "ECR".to_owned(),
                            repository_name: "images".to_owned(),
                        },
                        ..ContainerDistributionConfiguration::default()
                    });
            }
            Outputs::Licenses(count) => {
                entry.license_configuration_arns =
                    Some((0..count).map(|index| format!("license-{index}")).collect());
            }
            Outputs::LaunchTemplates(count) => {
                entry.launch_template_configurations = Some(
                    (0..count)
                        .map(|index| LaunchTemplateConfiguration {
                            launch_template_id: format!("lt-{index}"),
                            ..LaunchTemplateConfiguration::default()
                        })
                        .collect(),
                );
            }
            Outputs::S3Export => {
                entry.s3_export_configuration = Some(S3ExportConfiguration {
                    role_name: "export".to_owned(),
                    disk_image_format: "VMDK".to_owned(),
                    s3_bucket: "bucket".to_owned(),
                    s3_prefix: None,
                });
            }
            Outputs::FastLaunch(count) => {
                entry.fast_launch_configurations = Some(
                    (0..count)
                        .map(|_| FastLaunchConfiguration {
                            enabled: true,
                            ..FastLaunchConfiguration::default()
                        })
                        .collect(),
                );
            }
            Outputs::SsmParameters(count) => {
                entry.ssm_parameter_configurations = Some(
                    (0..count)
                        .map(|index| SsmParameterConfiguration {
                            parameter_name: format!("/images/{index}"),
                            ..SsmParameterConfiguration::default()
                        })
                        .collect(),
                );
            }
        }
        entry
    }

    /// One row of the tables below: the case, the entries as (target, outputs), and the
    /// entry a refusal addresses.
    type Row<'row> = (
        &'row str,
        Vec<(&'row str, Outputs<'row>)>,
        Option<RefusedEntry<'row>>,
    );

    /// One row of the list-wide table: the case, the entries as (target, outputs), and what a
    /// refusal of the list as a whole reports.
    type ListRow<'row> = (
        &'row str,
        Vec<(&'row str, Outputs<'row>)>,
        Option<RefusedDistributions<'row>>,
    );

    /// RULE: an entry that carries none of the members that distribute an output to its
    /// target is refused, whichever of them it could have carried, and a member present but
    /// holding nothing carries no output.
    #[test]
    fn an_entry_has_to_configure_at_least_one_output() {
        // (case, the entries as (target, outputs), the entry a refusal addresses)
        let rows: Vec<Row<'_>> = vec![
            (
                "an entry naming only its target",
                vec![("us-west-2", Outputs::Nothing)],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
            ),
            (
                "an entry whose AMI settings carry no member of their own",
                vec![("us-west-2", Outputs::Ami(None))],
                None,
            ),
            (
                "container settings only",
                vec![("us-west-2", Outputs::Container)],
                None,
            ),
            (
                "one license configuration only",
                vec![("us-west-2", Outputs::Licenses(1))],
                None,
            ),
            (
                "two launch template configurations only",
                vec![("us-west-2", Outputs::LaunchTemplates(2))],
                None,
            ),
            (
                "an export to Amazon S3 only",
                vec![("us-west-2", Outputs::S3Export)],
                None,
            ),
            (
                "fast-launch settings only",
                vec![("us-west-2", Outputs::FastLaunch(1))],
                None,
            ),
            (
                "SSM parameter settings only",
                vec![("us-west-2", Outputs::SsmParameters(3))],
                None,
            ),
            (
                "a launch template list holding nothing",
                vec![("us-west-2", Outputs::LaunchTemplates(0))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
            ),
            (
                "a license list holding nothing",
                vec![("us-west-2", Outputs::Licenses(0))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
            ),
            (
                "a fast-launch list holding nothing",
                vec![("us-west-2", Outputs::FastLaunch(0))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
            ),
            (
                "an SSM parameter list holding nothing",
                vec![("us-west-2", Outputs::SsmParameters(0))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
            ),
            (
                "the third entry, after two that carry an output each",
                vec![
                    ("us-west-2", Outputs::S3Export),
                    ("eu-west-1", Outputs::Ami(None)),
                    ("ap-south-1", Outputs::Nothing),
                ],
                Some(RefusedEntry {
                    index: 2,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
            ),
            (
                "an entry with no output whose target is unreachable too",
                vec![("US-EAST-2", Outputs::Nothing)],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
            ),
        ];

        assert_table(rows);
    }

    /// RULE: entries are read in order and the first one that breaks a requirement is the one
    /// a refusal addresses, by its own position; within an entry its outputs are read first,
    /// then its target, then the values its outputs carry.
    #[test]
    fn the_first_entry_that_breaks_a_requirement_is_the_one_reported() {
        // (case, the entries as (target, outputs), the entry a refusal addresses)
        let rows: Vec<Row<'_>> = vec![
            (
                "one well-formed entry",
                vec![("us-west-2", Outputs::Ami(None))],
                None,
            ),
            (
                "three well-formed entries",
                vec![
                    (
                        "us-east-1",
                        Outputs::Ami(Some("golden-{{ imagebuilder:buildDate }}")),
                    ),
                    ("eu-central-1", Outputs::Container),
                    (
                        "ap-southeast-3",
                        Outputs::Ami(Some("{{imagebuilder:buildDate}}-app")),
                    ),
                ],
                None,
            ),
            (
                "a target in upper case",
                vec![("US-EAST-2", Outputs::Ami(None))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("US-EAST-2"),
                }),
            ),
            (
                "a well-formed entry before a target in mixed case",
                vec![
                    ("us-west-2", Outputs::Ami(None)),
                    ("Eu-West-1", Outputs::S3Export),
                ],
                Some(RefusedEntry {
                    index: 1,
                    why: RefusedEntryRequirement::UnreachableTarget("Eu-West-1"),
                }),
            ),
            (
                "a target carrying a separator an authority would re-point on",
                vec![("us-west-2/../elsewhere", Outputs::Ami(None))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("us-west-2/../elsewhere"),
                }),
            ),
            (
                "a target longer than the field admits",
                vec![("us-west-2-us-west-2-us", Outputs::Ami(None))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("us-west-2-us-west-2-us"),
                }),
            ),
            (
                "an AMI name without the build-date tag",
                vec![("us-west-2", Outputs::Ami(Some("golden")))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                }),
            ),
            (
                "an AMI name naming another dynamic tag only",
                vec![(
                    "us-west-2",
                    Outputs::Ami(Some("golden-{{ imagebuilder:buildVersion }}")),
                )],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                }),
            ),
            (
                "the second entry's AMI name, after a well-formed one",
                vec![
                    (
                        "us-west-2",
                        Outputs::Ami(Some("first-{{ imagebuilder:buildDate }}")),
                    ),
                    ("eu-west-1", Outputs::Ami(Some("second"))),
                ],
                Some(RefusedEntry {
                    index: 1,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                }),
            ),
            (
                "an entry breaking two requirements, whose target is reported",
                vec![("US-EAST-2", Outputs::Ami(Some("golden")))],
                Some(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("US-EAST-2"),
                }),
            ),
        ];

        assert_table(rows);
    }

    /// Read every row of a table: the entries it describes, and the entry a refusal of them
    /// addresses.
    fn assert_table(rows: Vec<Row<'_>>) {
        for (case, entries, expected) in rows {
            let distributions: Vec<Distribution> = entries
                .iter()
                .map(|(region, outputs)| entry(region, *outputs))
                .collect();
            let actual = refused_distribution_entry(&distributions);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the refusal names the parameter whose value it refused at the entry's own
    /// position, and states the requirement that value had to satisfy.
    #[test]
    fn the_refusal_names_the_entrys_member_and_its_requirement() {
        // (case, the refused entry, the sentence)
        let rows = [
            (
                "the first entry, carrying no output",
                RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::WithoutOutputs,
                },
                "The value supplied for parameter 'distributions[0]' is not valid. One or more \
                 outputs should be provided for each region in a distribution configuration.",
            ),
            (
                "the fourth entry, carrying no output",
                RefusedEntry {
                    index: 3,
                    why: RefusedEntryRequirement::WithoutOutputs,
                },
                "The value supplied for parameter 'distributions[3]' is not valid. One or more \
                 outputs should be provided for each region in a distribution configuration.",
            ),
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

    /// RULE: the entries are read first, each on its own, and the list is read as a SET only
    /// once every entry has been accepted — so an entry that breaks one of its own
    /// requirements is reported as that entry even when another entry names its target too.
    #[test]
    fn the_entries_are_read_before_the_list_is_read_as_a_set() {
        // (case, the entries as (target, outputs), what a refusal of the list reports)
        let rows: Vec<ListRow<'_>> = vec![
            (
                "one entry naming one target",
                vec![("us-west-2", Outputs::Ami(None))],
                None,
            ),
            (
                "three targets, each named once",
                vec![
                    ("us-west-2", Outputs::Ami(None)),
                    ("eu-west-1", Outputs::Container),
                    ("ap-south-1", Outputs::S3Export),
                ],
                None,
            ),
            (
                "one target named twice, adjacent",
                vec![
                    ("us-west-2", Outputs::Ami(None)),
                    ("us-west-2", Outputs::Container),
                ],
                Some(RefusedDistributions::TargetNamedTwice),
            ),
            (
                "one target named twice, with another between them",
                vec![
                    ("eu-west-1", Outputs::Ami(None)),
                    ("us-east-1", Outputs::S3Export),
                    ("eu-west-1", Outputs::FastLaunch(1)),
                ],
                Some(RefusedDistributions::TargetNamedTwice),
            ),
            (
                "one target named three times",
                vec![
                    ("us-east-1", Outputs::Ami(None)),
                    ("us-east-1", Outputs::Ami(None)),
                    ("us-east-1", Outputs::Ami(None)),
                ],
                Some(RefusedDistributions::TargetNamedTwice),
            ),
            (
                "a repeated target whose second entry configures no output",
                vec![
                    ("us-west-2", Outputs::Ami(None)),
                    ("us-west-2", Outputs::Nothing),
                ],
                Some(RefusedDistributions::Entry(RefusedEntry {
                    index: 1,
                    why: RefusedEntryRequirement::WithoutOutputs,
                })),
            ),
            (
                "a repeated target neither of whose entries this service can reach",
                vec![
                    ("us-nowhere-1", Outputs::Ami(None)),
                    ("us-nowhere-1", Outputs::Ami(None)),
                ],
                Some(RefusedDistributions::Entry(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::UnreachableTarget("us-nowhere-1"),
                })),
            ),
            (
                "a repeated target whose first entry names its AMI by no build",
                vec![
                    ("eu-central-1", Outputs::Ami(Some("golden"))),
                    ("eu-central-1", Outputs::Ami(None)),
                ],
                Some(RefusedDistributions::Entry(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                })),
            ),
            (
                "an empty list, which names no target twice",
                vec![],
                None,
            ),
        ];

        for (case, entries, expected) in rows {
            let distributions: Vec<Distribution> = entries
                .iter()
                .map(|(region, outputs)| entry(region, *outputs))
                .collect();
            let actual = refused_distributions(&distributions);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a `distributions` value that breaks a requirement of the member it sits on is
    /// refused as that VALUE and the list's own requirement is one of them; only a value
    /// admissible on its own that cannot be combined with another member of the request is
    /// refused as a COMBINATION.
    #[test]
    fn the_refusal_class_follows_what_is_wrong_with_the_value() {
        // (case, the refusal, its class, the sentence it carries)
        let rows = [
            (
                "a list naming one target twice",
                RefusedDistributions::TargetNamedTwice,
                RefusedParameterClass::Value,
                "The value supplied for parameter 'distributions' is not valid. A region may \
                 appear in a distribution configuration at most once.",
            ),
            (
                "an entry configuring no output",
                RefusedDistributions::Entry(RefusedEntry {
                    index: 2,
                    why: RefusedEntryRequirement::WithoutOutputs,
                }),
                RefusedParameterClass::Value,
                "The value supplied for parameter 'distributions[2]' is not valid. One or more \
                 outputs should be provided for each region in a distribution configuration.",
            ),
            (
                "an entry naming its output AMI by no build",
                RefusedDistributions::Entry(RefusedEntry {
                    index: 0,
                    why: RefusedEntryRequirement::AmiNameWithoutBuildDate,
                }),
                RefusedParameterClass::Value,
                "The value supplied for parameter \
                 'distributions[0].amiDistributionConfiguration.name' is not valid. The EC2 \
                 AMI name must contain '{{ imagebuilder:buildDate }}' to ensure that the name \
                 will be unique.",
            ),
            (
                "an entry naming a target this service cannot reach",
                RefusedDistributions::Entry(RefusedEntry {
                    index: 1,
                    why: RefusedEntryRequirement::UnreachableTarget("us-nowhere-1"),
                }),
                RefusedParameterClass::Combination,
                "The value supplied for parameter \
                 'distributions[1].amiDistributionConfiguration' is not valid. AMI \
                 Distribution is not available in 'us-nowhere-1'.",
            ),
        ];

        for (case, refused, class, expected) in rows {
            let actual = refused.class();
            assert_eq!(
                actual, class,
                "case {case:?}: expected the class {class:?}, got {actual:?}"
            );
            let actual = refused.message();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
