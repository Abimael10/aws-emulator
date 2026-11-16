// SPDX-License-Identifier: CC0-1.0

//! Where the AMI an image reports comes from: Amazon EC2, the service that owns an AMI.
//!
//! An AMI is not a resource of this service. "The Amazon EC2 AMIs created by this image"
//! (`OutputResources`' `amis`) are images of the account's own EC2, so an AMI this service
//! reports is one EC2 holds and answered for, and the two questions an image's output
//! resources raise are both asked of EC2 here:
//!
//! * a build this account ran produced an AMI, so the call that queues the build registers
//!   one ([`produce_output_ami`]) — this emulator runs no build instance, so the image the
//!   build will hand back is registered by the one call that mutates anything for that build
//!   — and the build version records what EC2 answered about it. A build whose AMI EC2 did
//!   not register records none, and a read of the image reports none: nothing here invents an
//!   id EC2 would not resolve.
//! * an image AWS PUBLISHES is backed by an AMI Amazon publishes
//!   ([`managed_image_output_resources`]), which no build of this account ever registered, so
//!   it is RESOLVED against EC2's own published catalog by the name Amazon publishes that
//!   base image under ([`super::ManagedImage::base_image_name_pattern`]) rather than recorded
//!   anywhere.
//!
//! EC2 is reached the way any emulated service reaches another: the AWS SDK driven over the
//! in-process operation callback ([`super::ec2_client`]), so this crate names no other
//! service crate and nothing leaves the process. Where no EC2 answers at all — an emulator
//! assembled without it, or a Region label no endpoint can be addressed in — nothing about an
//! AMI is established, and an image reports no output AMI rather than one nothing confirmed.

use crate::ops::types::OutputResources;

use super::{
    ComAmazonawsImagebuilderImagebuilderState, ManagedImage, OutputAmiRecord, QueuedImageBuild,
    IMAGE_STATUS_AVAILABLE, IMAGE_STATUS_FAILED, IMAGE_STATUS_PENDING, IMAGE_TYPE_AMI,
    PLATFORM_WINDOWS,
};

/// The owner alias EC2 publishes Amazon's own images under, which is the owner a base image
/// AWS publishes is looked for among ("The images available to you include public images …",
/// `ec2:DescribeImages`' `Owners`: "self | amazon | aws-backbone | aws-marketplace").
const AMAZON_OWNER_ALIAS: &str = "amazon";

/// The filter `ec2:DescribeImages` selects an image by its name with.
const IMAGE_NAME_FILTER: &str = "name";

/// The root device of a Linux AMI, which is the device an image an Image Builder build
/// produces boots from ("The device name of the root device volume (for example,
/// `/dev/sda1`)", `ec2:RegisterImage`'s `RootDeviceName`; `/dev/xvda` is the name the Linux
/// AMIs this emulator's EC2 publishes carry).
const LINUX_ROOT_DEVICE_NAME: &str = "/dev/xvda";

/// The root device of a Windows AMI, the name that member's own example gives.
const WINDOWS_ROOT_DEVICE_NAME: &str = "/dev/sda1";

/// What the AMI of one build is registered as: the members of it that the build, its recipe
/// and its distribution configuration decide.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedOutputAmi {
    /// The name the output AMI carries ([`super::output_ami_name`]).
    name: String,
    /// The description it carries, when the build's configuration or its recipe gives one.
    description: Option<String>,
    /// The device the image boots from, from the platform the build built for.
    root_device_name: &'static str,
}

/// Register the AMI the build version `queued` names produces with Amazon EC2 and record what
/// EC2 answered about it on that build version.
///
/// Called by the operations that QUEUE a build, after the build version has been filed: this
/// emulator runs no build instance, so the image the build hands back is registered by that
/// same call, and the build version reports it from the instant the build is due on
/// ([`super::ImageBuildVersionRecord::output_resources`]).
///
/// Nothing is reported as produced that EC2 did not confirm: the AMI is read back through
/// `ec2:DescribeImages` after it is registered, and a lookup nothing answers records nothing.
/// A build of a CONTAINER recipe registers no AMI at all.
pub async fn produce_output_ami<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    queued: &QueuedImageBuild,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    // The plan is read under the guard and the guard is dropped before EC2 is asked, so the
    // store is not held across the hop.
    let planned = {
        let store = state.lock();
        planned_output_ami(&store, queued)
    };
    let Some(planned) = planned else {
        return;
    };
    let Some(registered) = registered_ami(ctx, &queued.region, &planned).await else {
        return;
    };
    state
        .lock()
        .record_output_amis(&queued.arn, vec![registered]);
}

/// The output resources an image AWS publishes reports: the Amazon-owned AMI that backs it in
/// `region`, as the modelled `OutputResources` a read reports.
///
/// The member is always reported, since a published base image is an AMI-type image AWS
/// serves as available; the LIST is what EC2 answered — one entry for the base image Amazon
/// publishes under this catalog entry's name, and none when EC2 publishes no image under it,
/// which is the same answer an unreachable EC2 gives.
pub async fn managed_image_output_resources(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    image: &ManagedImage,
) -> OutputResources {
    OutputResources {
        amis: Some(
            published_base_image(ctx, region, image.base_image_name_pattern)
                .await
                .iter()
                .map(OutputAmiRecord::view)
                .collect(),
        ),
        containers: None,
    }
}

/// What the AMI of the build version `queued` names is registered as, or `None` when that
/// build version produces no AMI at all.
///
/// The name is the one the build's distribution configuration names for the Region the build
/// runs in, with its build-date tag expanded ([`super::output_ami_name`]). The description is
/// that configuration's, "and if you don't specify a description, the AMI in the build Region
/// uses the image recipe's description, if the recipe has one"
/// (`smithy_query(query="doc",shape="AmiDistributionConfiguration",member="description")`) —
/// so each is read from the resources the build referenced, as this store still holds them.
fn planned_output_ami(
    store: &ComAmazonawsImagebuilderImagebuilderState,
    queued: &QueuedImageBuild,
) -> Option<PlannedOutputAmi> {
    let region = queued.region.as_str();
    let record = store.images.get(&queued.arn)?;
    // "For an image recipe, this is always AMI": only a build of an image recipe produces an
    // AMI, and a container build's output is an image in a repository instead.
    if record.image_type.as_deref() != Some(IMAGE_TYPE_AMI) {
        return None;
    }
    // "Each entry defines how output images are distributed in one target Amazon Web Services
    // Region" (`DistributionConfiguration`'s `distributions`), so the entry that configures
    // the AMI of THIS build is the one naming the Region the build runs in.
    let configured = record
        .distribution_configuration_arn
        .as_deref()
        .and_then(|reference| store.distribution_configuration(reference, region))
        .and_then(|configuration| {
            configuration
                .distributions
                .iter()
                .find(|entry| entry.region == region)
                .and_then(|entry| entry.ami_distribution_configuration.clone())
        });
    let recipe_description = record
        .image_recipe_arn
        .as_deref()
        .and_then(|reference| store.image_recipe(reference, region))
        .and_then(|recipe| recipe.description.clone());
    Some(PlannedOutputAmi {
        name: super::output_ami_name(
            configured
                .as_ref()
                .and_then(|settings| settings.name.as_deref()),
            &record.name,
            queued.queued_at,
        ),
        description: configured
            .and_then(|settings| settings.description)
            .or(recipe_description),
        root_device_name: if record.platform.as_deref() == Some(PLATFORM_WINDOWS) {
            WINDOWS_ROOT_DEVICE_NAME
        } else {
            LINUX_ROOT_DEVICE_NAME
        },
    })
}

/// The AMI `planned` describes, registered with EC2 in `region` and read back from it, or
/// `None` when EC2 registered none.
///
/// The read-back is what makes a reported AMI one EC2 really holds: the register answers an
/// id, and every member the image reports — its name, its description, the state it is in and
/// the account that owns it — is what EC2 answers for that id.
async fn registered_ami(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    planned: &PlannedOutputAmi,
) -> Option<OutputAmiRecord> {
    let client = super::ec2_client(ctx, region)?;
    let registered = client
        .register_image()
        .name(&planned.name)
        .set_description(planned.description.clone())
        .root_device_name(planned.root_device_name)
        .send()
        .await;
    let image_id = match registered {
        Ok(answer) => answer.image_id,
        Err(error) => {
            // At `warn`: a deployment where this hop fails is one where no build produces an
            // AMI, which is worth seeing, and it IS why an image reports none. The name the
            // register carried is rendered in its escaped `Debug` form, so a name carrying a
            // newline cannot forge a log line.
            ::tracing::warn!(
                ami_name = ?planned.name,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the output AMI of an image build could not be registered with the \
                 emulator's EC2"
            );
            return None;
        }
    }?;
    let described = client
        .describe_images()
        .image_ids(&image_id)
        .send()
        .await;
    let images = match described {
        Ok(answer) => answer.images,
        Err(error) => {
            ::tracing::warn!(
                image_id = ?image_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the output AMI of an image build could not be read back from the \
                 emulator's EC2"
            );
            return None;
        }
    };
    images
        .unwrap_or_default()
        .first()
        .and_then(|image| output_ami(region, image))
}

/// The Amazon-published base image whose name matches `name_pattern`, as EC2 answers for it
/// in `region`, or `None` when EC2 publishes none under that name.
///
/// "The images available to you include public images" (`ec2:DescribeImages`): the base images
/// AWS publishes are Amazon's own, so the catalog is asked for Amazon-owned images under the
/// published name. A name pattern may reach several builds of one published image, and the
/// one it names is the newest of them — the choice a client resolving an AMI by name pattern
/// makes, read off the `CreationDate` EC2 answers.
async fn published_base_image(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name_pattern: &str,
) -> Option<OutputAmiRecord> {
    let client = super::ec2_client(ctx, region)?;
    let described = client
        .describe_images()
        .owners(AMAZON_OWNER_ALIAS)
        .filters(
            ::aws_sdk_ec2::types::Filter::builder()
                .name(IMAGE_NAME_FILTER)
                .values(name_pattern)
                .build(),
        )
        .send()
        .await;
    let images = match described {
        Ok(answer) => answer.images.unwrap_or_default(),
        Err(error) => {
            ::tracing::warn!(
                name_pattern = ?name_pattern,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the base image of a published Image Builder image could not be resolved \
                 against the emulator's EC2"
            );
            return None;
        }
    };
    images
        .iter()
        .max_by(|one, other| one.creation_date().cmp(&other.creation_date()))
        .and_then(|image| output_ami(region, image))
}

/// What EC2 answered about `image`, as this service holds an AMI an image reports, or `None`
/// when the answer carries no AMI id — nothing an image could report an AMI from.
///
/// Every member of the modelled `Ami` is a `NonEmptyString`, so a member EC2 holds nothing
/// for is reported as absent rather than as an empty value: an AMI with no description is one
/// of those, since EC2 answers the description of an image registered without one as empty.
fn output_ami(region: &str, image: &::aws_sdk_ec2::types::Image) -> Option<OutputAmiRecord> {
    let image_id = non_empty(image.image_id())?;
    Some(OutputAmiRecord {
        region: region.to_string(),
        image: image_id,
        name: non_empty(image.name()).unwrap_or_default(),
        description: non_empty(image.description()),
        status: ami_status(image.state()).to_string(),
        account_id: non_empty(image.owner_id()).unwrap_or_default(),
    })
}

/// `value` as an owned string, or `None` when EC2 answered nothing or an empty value for it.
fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// The modelled `ImageStatus` an AMI in EC2 image state `state` is reported under.
///
/// `Ami`'s `state` is an `ImageState`, whose `status` is an `ImageStatus`
/// (`smithy_query(query="shape",shape="Ami")`), while EC2 reports an image's own
/// `Image$State`: "`available` - The AMI is successfully registered and can be used to launch
/// an instance", `pending` while a register is still completing, and the remaining states an
/// image that cannot be launched is in. So an AMI EC2 can launch is
/// [`IMAGE_STATUS_AVAILABLE`], one still being registered is [`IMAGE_STATUS_PENDING`], and
/// anything else produced no usable image, which is what [`IMAGE_STATUS_FAILED`] says.
fn ami_status(state: Option<&::aws_sdk_ec2::types::ImageState>) -> &'static str {
    match state {
        Some(::aws_sdk_ec2::types::ImageState::Available) => IMAGE_STATUS_AVAILABLE,
        Some(::aws_sdk_ec2::types::ImageState::Pending | ::aws_sdk_ec2::types::ImageState::Transient) => {
            IMAGE_STATUS_PENDING
        }
        _ => IMAGE_STATUS_FAILED,
    }
}
