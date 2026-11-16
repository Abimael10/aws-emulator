// SPDX-License-Identifier: CC0-1.0

//! The AMIs an image build produces: what this store holds for one, what a read of an image
//! reports them as, and the name the AMI of a build carries.
//!
//! `Image`'s `outputResources` is "the output resources that Image Builder produces for this
//! image", whose `amis` member is "the Amazon EC2 AMIs created by this image. The list
//! contains one entry per AMI, including copies that distribution created in each target
//! Amazon Web Services Region and account", and each entry carries the Region the AMI lives
//! in, its id, its name, its description, its state and the account that owns it
//! (`smithy_query(query="shape",shape="Ami")`). So:
//!
//! * an AMI an image produced is Amazon EC2's resource, and this store holds only what EC2
//!   ANSWERED about the one it registered for a build
//!   ([`OutputAmiRecord`], written by [`super::produce_output_ami`]) — an id nothing
//!   registered is reported by nobody;
//! * only an AMI-type image produces AMIs at all ("For an image recipe, this is always AMI",
//!   the modelled `type` member of `ImageRecipe`), so a container image reports no output
//!   resources of this kind;
//! * the output resources are what the build PRODUCED, so they are reported from the instant
//!   the build reached its terminal available status on
//!   ([`ImageBuildVersionRecord::status_at`]) and the member reports an empty list while the
//!   build is still outstanding.
//!
//! The NAME of an output AMI is the one member of it a configuration decides:
//! `amiDistributionConfiguration.name` is "the name of the output AMI. The name must include
//! the `{{ imagebuilder:buildDate }}` dynamic tag so that each build produces a uniquely
//! named AMI. If you don't specify a name, Image Builder names the output AMI with the image
//! name followed by the build timestamp, for example `my-image 2022-10-26T22-30-05.912619Z`"
//! (`smithy_query(query="doc",shape="AmiDistributionConfiguration",member="name")`) — which
//! is both halves of [`output_ami_name`]: the default name, and the expansion of that dynamic
//! tag in a configured one.

use std::time::SystemTime;

use crate::ops::types::{Ami, ImageState, OutputResources};

use super::{
    ComAmazonawsImagebuilderImagebuilderState, ImageBuildVersionRecord, IMAGE_STATUS_AVAILABLE,
    IMAGE_TYPE_AMI,
};

/// The expression inside the braces of the dynamic tag an output AMI's name may carry, as
/// the `name` member's own documentation writes it (`{{ imagebuilder:buildDate }}`).
///
/// A template expression, so the whitespace inside the braces is not part of it: a name
/// writing the tag compactly carries the same tag ([`carries_build_date_tag`]).
const BUILD_DATE_EXPRESSION: &str = "imagebuilder:buildDate";

/// The braces a dynamic tag opens with.
const TAG_OPEN: &str = "{{";

/// The braces a dynamic tag closes with.
const TAG_CLOSE: &str = "}}";

/// One AMI an image build produced, as this store holds it: what Amazon EC2 answered about
/// the image Image Builder registered for the build, in the Region the build ran in.
///
/// Every member is EC2's own answer rather than anything derived here, so what a read of an
/// image reports about an AMI it produced cannot drift from the image EC2 holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputAmiRecord {
    /// "The Amazon Web Services Region of the Amazon EC2 AMI" — the Region the build ran in,
    /// which is where "Image Builder creates the output image … where the build runs"
    /// (`CreateImage`'s `distributionConfigurationArn`).
    pub region: String,
    /// "The AMI ID of the Amazon EC2 AMI", as EC2 minted it.
    pub image: String,
    /// "The name of the Amazon EC2 AMI", as EC2 holds it.
    pub name: String,
    /// "The description of the Amazon EC2 AMI", when the AMI carries one.
    pub description: Option<String>,
    /// The status this AMI was in when EC2 was asked, from the modelled `ImageStatus` enum —
    /// read from the EC2 image state where the AMI is resolved.
    pub status: String,
    /// "The account ID of the owner of the AMI", as EC2 reports it.
    pub account_id: String,
}

/// The build version one call queued, as the step that produces its AMI needs it.
///
/// What the two calls that queue a build — a build a caller asks for by hand and a pipeline
/// execution — hand to [`super::produce_output_ami`] once the build version is filed and this
/// store's guard is released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedImageBuild {
    /// The ARN of the build version that was filed, which the AMI is recorded on.
    pub arn: String,
    /// The Region the build runs in, which is the Region its AMI is registered in.
    pub region: String,
    /// The instant the build was queued, which the name of its output AMI carries.
    pub queued_at: SystemTime,
}

impl OutputAmiRecord {
    /// This record as the modelled `Ami` one entry of an image's output resources carries.
    #[must_use]
    pub fn view(&self) -> Ami {
        Ami {
            region: Some(self.region.clone()),
            image: Some(self.image.clone()),
            name: Some(self.name.clone()),
            description: self.description.clone(),
            // "The state of the AMI, which includes the status and, if applicable, the
            // reason for that status": an AMI EC2 holds carries no reason for being in the
            // state it is in, so none is reported.
            state: Some(ImageState {
                status: Some(self.status.clone()),
                reason: None,
                failure_context: None,
            }),
            account_id: Some(self.account_id.clone()),
        }
    }
}

impl ImageBuildVersionRecord {
    /// What this build version PRODUCED at the instant `at`, as the modelled
    /// `OutputResources` a read reports, or `None` when this kind of image produces none of
    /// them.
    ///
    /// The three readings the module docs state: a container image reports none of this kind,
    /// an AMI-type image reports the AMIs it registered once its build has reached its
    /// terminal available status, and one whose build is still outstanding reports the empty
    /// list a build that has produced nothing yet has produced.
    #[must_use]
    pub fn output_resources(&self, at: SystemTime) -> Option<OutputResources> {
        if self.image_type.as_deref() != Some(IMAGE_TYPE_AMI) {
            return None;
        }
        let produced = self.status_at(at) == IMAGE_STATUS_AVAILABLE;
        Some(OutputResources {
            amis: Some(if produced {
                self.output_amis.iter().map(OutputAmiRecord::view).collect()
            } else {
                Vec::new()
            }),
            // "The container images that Image Builder created when it built this image,
            // stored in the output Amazon ECR repository": an AMI-type image pushes none.
            containers: None,
        })
    }
}

impl ComAmazonawsImagebuilderImagebuilderState {
    /// Record `amis` as the AMIs the build version `arn` produced, replacing whatever its
    /// last run produced, and answer whether the build version is still one this store holds.
    ///
    /// What is held is the output of the run that produced it, so one record keeps as many
    /// entries as that run registered rather than growing with every call: a build version
    /// that runs again ([`Self::rerun_image_build`]) reports what the new run produced.
    /// A build version that was deleted while its AMI was being registered records nothing:
    /// the AMI is EC2's resource either way, and this store reports output resources only for
    /// a build version it still holds.
    pub fn record_output_amis(&mut self, arn: &str, amis: Vec<OutputAmiRecord>) -> bool {
        match self.images.get_mut(arn) {
            Some(record) => {
                record.output_amis = amis;
                true
            }
            None => false,
        }
    }
}

/// Whether `name` carries the build-date dynamic tag, so each build of the image produces a
/// uniquely named AMI.
///
/// The tag is a template expression, so the whitespace inside its braces is not part of it: a
/// name writing it compactly names each build as surely as one writing it the way the
/// documentation does.
#[must_use]
pub fn carries_build_date_tag(name: &str) -> bool {
    expand_build_date(name, "") != name
}

/// The name the output AMI of the image `image_name` carries for a build queued at `at`.
///
/// The two halves of `amiDistributionConfiguration.name` (see the module docs): a build whose
/// distribution configuration names an output AMI carries that name with its build-date tag
/// expanded to the build timestamp, and a build that configures no name carries "the image
/// name followed by the build timestamp".
#[must_use]
pub fn output_ami_name(configured: Option<&str>, image_name: &str, at: SystemTime) -> String {
    let timestamp = build_timestamp(at);
    match configured {
        Some(configured) => expand_build_date(configured, &timestamp),
        None => format!("{image_name} {timestamp}"),
    }
}

/// The build timestamp an output AMI's name carries: the instant `at` in the form the
/// documented example name writes it (`2022-10-26T22-30-05.912619Z`).
///
/// The instant this crate renders every modelled `DateTime` with ([`super::iso8601`]), with
/// the microseconds of the instant filled in and the `:` of the time written as `-`: an AMI
/// name may not carry a colon ("3-128 alphanumeric characters, parentheses, square brackets,
/// spaces, periods, slashes, dashes, single quotes, at-signs, or underscores",
/// `ec2:RegisterImage`'s `Name`), which is why the documented example writes the time that
/// way.
#[must_use]
pub fn build_timestamp(at: SystemTime) -> String {
    let rendered = super::iso8601(at);
    let instant = rendered.trim_end_matches('Z');
    // The rendering carries a fractional part only where the instant has one, and the name's
    // own form always carries one, so the seconds are taken without it and the microseconds
    // are filled in below.
    let seconds = instant
        .split_once('.')
        .map_or(instant, |(seconds, _)| seconds)
        .replace(':', "-");
    let microseconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_micros());
    format!("{seconds}.{microseconds:06}Z")
}

/// `name` with every build-date dynamic tag expanded to `timestamp`, leaving every other
/// dynamic tag as the name wrote it.
fn expand_build_date(name: &str, timestamp: &str) -> String {
    let mut expanded = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(open) = rest.find(TAG_OPEN) {
        let (before, from_open) = rest.split_at(open);
        expanded.push_str(before);
        let Some(close) = from_open.find(TAG_CLOSE) else {
            rest = from_open;
            break;
        };
        let inside: String = from_open[TAG_OPEN.len()..close]
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        if inside == BUILD_DATE_EXPRESSION {
            expanded.push_str(timestamp);
        } else {
            expanded.push_str(&from_open[..close + TAG_CLOSE.len()]);
        }
        rest = &from_open[close + TAG_CLOSE.len()..];
    }
    expanded.push_str(rest);
    expanded
}

#[cfg(test)]
mod tests {
    //! The name an output AMI carries and the output resources a build version reports. The
    //! wire behaviour — that a build registers an AMI and a read of the image reports it — is
    //! asserted through the router in `tests/all/image_output_resources.rs`.

    use std::time::Duration;

    use super::*;

    /// An instant with microseconds in it, so the timestamp a name carries is asserted on a
    /// sub-second instant rather than on a whole second.
    fn instant() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_micros(1_666_823_405_912_619)
    }

    /// RULE: a configured output AMI name carries that name with its build-date tag expanded
    /// to the build timestamp, whichever way the tag is written, and a build that configures
    /// no name carries the image name followed by that timestamp.
    #[test]
    fn an_output_ami_name_expands_the_build_date_tag_of_the_configured_name() {
        // (case, the configured name, the name the AMI carries)
        let rows: &[(&str, Option<&str>, &str)] = &[
            (
                "no configured name",
                None,
                "my-image 2022-10-26T22-30-05.912619Z",
            ),
            (
                "the tag as the documentation writes it",
                Some("golden-{{ imagebuilder:buildDate }}"),
                "golden-2022-10-26T22-30-05.912619Z",
            ),
            (
                "the tag written compactly",
                Some("{{imagebuilder:buildDate}}-app"),
                "2022-10-26T22-30-05.912619Z-app",
            ),
            (
                "the tag twice",
                Some("{{ imagebuilder:buildDate }}-{{imagebuilder:buildDate}}"),
                "2022-10-26T22-30-05.912619Z-2022-10-26T22-30-05.912619Z",
            ),
            (
                "another dynamic tag beside it",
                Some("{{ imagebuilder:buildVersion }}-{{ imagebuilder:buildDate }}"),
                "{{ imagebuilder:buildVersion }}-2022-10-26T22-30-05.912619Z",
            ),
            (
                "unbalanced braces",
                Some("golden-{{ imagebuilder:buildDate"),
                "golden-{{ imagebuilder:buildDate",
            ),
        ];
        for (case, configured, expected) in rows {
            let actual = output_ami_name(*configured, "my-image", instant());
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a name carries the build-date tag exactly when one of its dynamic tags IS that
    /// tag, whichever way it is written.
    #[test]
    fn the_build_date_tag_is_recognized_however_it_is_written() {
        // (case, the name, whether it carries the tag)
        let rows: &[(&str, &str, bool)] = &[
            ("the documented spelling", "golden-{{ imagebuilder:buildDate }}", true),
            ("the compact spelling", "golden-{{imagebuilder:buildDate}}", true),
            ("another dynamic tag", "golden-{{ imagebuilder:buildVersion }}", false),
            ("no tag at all", "golden", false),
            ("unbalanced braces", "golden-{{ imagebuilder:buildDate", false),
        ];
        for (case, name, expected) in rows {
            let actual = carries_build_date_tag(name);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual} for {name:?}"
            );
        }
    }

    /// RULE: an AMI-type build version reports the AMIs it registered once its build has
    /// reached its terminal available status, the empty list while that build is outstanding,
    /// and a container image reports no output resources of this kind.
    #[test]
    fn the_output_amis_of_a_build_are_reported_from_its_available_instant() {
        let at = instant();
        let ami = OutputAmiRecord {
            region: "us-west-2".to_string(),
            image: "ami-0123456789abcdef0".to_string(),
            name: "my-image 2022-10-26T22-30-05.912619Z".to_string(),
            description: None,
            status: IMAGE_STATUS_AVAILABLE.to_string(),
            account_id: ::engine::EMULATOR_ACCOUNT_ID.to_string(),
        };
        // (case, the image type, the instant the build is due, how many AMIs are reported)
        type Row<'row> = (
            &'row str,
            Option<&'row str>,
            Option<SystemTime>,
            Option<usize>,
        );
        let rows: &[Row<'_>] = &[
            ("an available AMI-type build", Some(IMAGE_TYPE_AMI), None, Some(1)),
            (
                "an outstanding AMI-type build",
                Some(IMAGE_TYPE_AMI),
                Some(at + Duration::from_secs(3)),
                Some(0),
            ),
            (
                "an AMI-type build due at this instant",
                Some(IMAGE_TYPE_AMI),
                Some(at),
                Some(1),
            ),
            ("a container image", Some(super::super::IMAGE_TYPE_DOCKER), None, None),
            ("an image of no recorded type", None, None, None),
        ];
        for (case, image_type, available_at, expected) in rows {
            let record = ImageBuildVersionRecord {
                image_type: image_type.map(ToString::to_string),
                available_at: *available_at,
                output_amis: vec![ami.clone()],
                ..ImageBuildVersionRecord::default()
            };
            let reported = record.output_resources(at);
            let actual = reported
                .as_ref()
                .map(|resources| resources.amis.as_ref().map_or(0, Vec::len));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?} AMIs, got {reported:?}"
            );
        }
    }
}
