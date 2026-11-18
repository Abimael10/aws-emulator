// SPDX-License-Identifier: CC0-1.0

//! The Amazon EBS volume a launch CONFIGURES AT LAUNCH, as Amazon EC2 holds it.
//!
//! # The rule this module implements
//!
//! A task definition may declare a volume it does not describe: `Volume$configuredAtLaunch`
//! is "whether the volume should be configured at launch time ... To configure a volume at
//! launch time, use this task definition revision and specify a `volumeConfigurations`
//! object when calling the `CreateService`, `UpdateService`, `RunTask` or `StartTask` APIs",
//! and `Volume$name` adds that such a name "is required and must also be specified as the
//! volume name in the `ServiceVolumeConfiguration` or `TaskVolumeConfiguration` parameter".
//! So a LAUNCH of such a revision has to carry that volume's configuration, matched by name
//! (`RunTaskRequest$volumeConfigurations`: "The details of the volume that was
//! `configuredAtLaunch` ... The `name` of the volume must match the `name` from the task
//! definition") — a launch that carries none asks for a volume nothing could create, and a
//! configuration naming a volume the revision did not configure at launch describes a volume
//! the task has no mount for. Either way the launch is refused and no task is placed
//! ([`volume_configuration_violation`]).
//!
//! A launch that DOES carry it is launched with that volume: the configuration is "the
//! configuration for the Amazon EBS volume that Amazon ECS creates and manages on your
//! behalf ... with one volume created for each task"
//! (`TaskManagedEBSVolumeConfiguration`), so the volume is a REAL Amazon EBS volume of the
//! caller's own account — the service that owns EBS volumes holds it, that service's own
//! `DescribeVolumes` resolves it and reports the tags the configuration asked for, and the
//! volume is attached to the task Amazon ECS created it for.
//!
//! Nothing here invents a volume. The record is written by the owning service's own writer
//! ([`ec2::ComAmazonawsEc2Amazonec2State::insert_volume`]), which mints the `VolumeId` out of
//! that store's id counter, builds the same `Volume` shape its own `CreateVolume` builds and
//! charges the bytes against its own budget — the seam a volume another service of this
//! account provisions for a resource of its own goes through, and the one
//! [`super::awsvpc_interfaces`] creates a task's elastic network interface through.
//!
//! # What the task reports for it
//!
//! `Attachment$type` names this kind of attachment ("such as `ElasticNetworkInterface`,
//! `Service Connect`, and `AmazonElasticBlockStorage`") and `Attachment$details` says what
//! one carries: "For Elastic Block Storage, this includes `roleArn`,
//! `deleteOnTermination`, `volumeName`, `volumeId`". The first three are known the instant
//! the launch is accepted, because the launch supplied them; the volume id is the owning
//! store's and is reported once that volume EXISTS ([`settle_managed_ebs_volumes`]), so no
//! attachment names a volume no `DescribeVolumes` resolves. The attachment's own status moves
//! the way every other task attachment's does, one documented step at a time
//! ([`INTERFACE_ATTACH_LIFECYCLE`]), and the task is `PROVISIONING` until it is in place:
//! Amazon ECS creates and attaches the volume before the task is launched.
//!
//! LOCK ORDER: the pass below takes this store's lock, releases it, takes the Amazon EC2
//! store's, releases that, and then takes this one's again to record what Amazon EC2 created
//! — so no two store locks are ever held at once, the same contract
//! [`super::awsvpc_interfaces`] and [`evaluate_image_pulls`] have.

use super::*;

use ::service_com_amazonaws_ec2_amazonec2::ops::types::{AttachVolumeOutput, Tag as Ec2Tag};
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

/// The tags the volume one task is given carries: the launch's own, and the two Amazon ECS
/// applies itself.
mod volume_tags;
use volume_tags::volume_tags;

/// `Attachment$type` of the Amazon EBS volume a launch configured at launch — the third type
/// the member's own doc names ("such as `ElasticNetworkInterface`, `Service Connect`, and
/// `AmazonElasticBlockStorage`").
pub const ATTACHMENT_TYPE_AMAZON_ELASTIC_BLOCK_STORAGE: &str = "AmazonElasticBlockStorage";

/// The `Attachment$details` key the volume's own name is reported under — the name the
/// launch matched against the task definition. From the member's own list for this
/// attachment type ("for Elastic Block Storage, this includes `roleArn`,
/// `deleteOnTermination`, `volumeName`, `volumeId`").
pub const ATTACHMENT_DETAIL_VOLUME_NAME: &str = "volumeName";

/// The `Attachment$details` key the Amazon EBS volume's id is reported under, from the same
/// list. Reported once the volume EXISTS in the store that owns it
/// ([`settle_managed_ebs_volumes`]).
pub const ATTACHMENT_DETAIL_VOLUME_ID: &str = "volumeId";

/// The `Attachment$details` key the infrastructure role is reported under, from the same
/// list — `TaskManagedEBSVolumeConfiguration$roleArn`: "The ARN of the IAM role to associate
/// with this volume."
pub const ATTACHMENT_DETAIL_ROLE_ARN: &str = "roleArn";

/// The `Attachment$details` key the termination policy is reported under, from the same list.
pub const ATTACHMENT_DETAIL_DELETE_ON_TERMINATION: &str = "deleteOnTermination";

/// Whether the volume is deleted when the task stops, for a configuration that states no
/// policy. `TaskManagedEBSVolumeTerminationPolicy$deleteOnTermination`: "If no value is
/// specified, the default value is `true` is used."
pub const DEFAULT_DELETE_ON_TERMINATION: bool = true;

/// The service principal Amazon EC2 reports as owning the resource one of these volumes is
/// attached to (`VolumeAttachment$InstanceOwningService`: "The service principal of the
/// Amazon Web Services service that owns the underlying resource to which the volume is
/// attached"): the volume is attached to a task, and Amazon ECS is what holds that task.
pub const ECS_SERVICE_PRINCIPAL: &str = "ecs.amazonaws.com";

/// The tag keys a caller may not put on one of these volumes, which are also the tags
/// Amazon ECS puts on it itself. `EBSTagSpecification$tags`: "The tags applied to this
/// Amazon EBS volume. `AmazonECSCreated` and `AmazonECSManaged` are reserved tags that
/// can't be used", and `TaskManagedEBSVolumeConfiguration$tagSpecifications`: "Amazon ECS
/// applies service-managed tags by default."
pub const RESERVED_VOLUME_TAG_KEYS: [&str; 2] = ["AmazonECSCreated", "AmazonECSManaged"];

/// The `resourceType` an `EBSTagSpecification` of one of these volumes names — the one value
/// `EBSResourceType` carries (`crate::ops::types::E_B_S_RESOURCE_TYPE_VALUES`).
pub const EBS_TAG_RESOURCE_TYPE_VOLUME: &str = "volume";

/// How many volumes ONE launch may configure at launch.
///
/// `Volume`'s own doc: "The volume configuration may contain multiple volumes but only one
/// volume configured at launch is supported" — a revision holds at most one such volume, so a
/// launch has at most one of them to configure and a longer list configures volumes no
/// registration could hold. Checked BEFORE any per-entry work, so the matching below is
/// bounded by the stored revision's own volumes however long a request's list is.
pub const MAX_VOLUME_CONFIGURATIONS: usize = 1;

/// The noun `RunTaskRequest$volumeConfigurations` names its entries by, for the refusal that
/// states that bound ([`at_most_items_message`]).
pub const VOLUME_CONFIGURATIONS_SUBJECT: &str = "volumeConfigurations";

/// The refusal a launch earns for a volume the task definition configured at launch that the
/// launch carries no configuration for — and for one whose configuration describes no volume
/// to create at all.
///
/// The fixed text states the rule `Volume$configuredAtLaunch` and
/// `RunTaskRequest$volumeConfigurations` state together — such a volume is configured by the
/// launch, under the name the task definition gave it — in the words Amazon ECS words the
/// SAME class of refusal for the same operation in: "Network Configuration must be provided
/// when networkMode 'awsvpc' is specified." ([`AWSVPC_NETWORK_CONFIGURATION_REQUIRED_MESSAGE`],
/// the one such sentence this crate holds from a recorded `RunTask` refusal). The only
/// variable part is the volume's name, read off the stored revision this launch runs.
///
/// `TaskVolumeConfiguration$managedEBSVolume` ("the configuration for the Amazon EBS volume
/// that Amazon ECS creates and manages on your behalf") is the only thing one of these
/// carries beside the name, so a configuration without it is a launch that provided no volume
/// configuration for that volume and earns this same sentence rather than a second one.
pub fn missing_volume_configuration_message(volume_name: &str) -> String {
    format!(
        "Volume Configuration must be provided when the volume '{volume_name}' is \
         configuredAtLaunch."
    )
}

/// The refusal a launch earns for a `volumeConfigurations` entry that names no volume the
/// task definition configured at launch.
///
/// `RunTaskRequest$volumeConfigurations`: "The `name` of the volume must match the `name`
/// from the task definition" — a configuration naming anything else describes a volume the
/// task has no mount for. That sentence is the fixed text; the only variable part is the name
/// the REQUEST sent.
pub fn unmatched_volume_configuration_message(volume_name: &str) -> String {
    format!(
        "The name of the volume '{volume_name}' must match the name of a volume that was \
         configuredAtLaunch in the task definition."
    )
}

/// The refusal a launch earns for a managed-volume configuration that states neither a size
/// nor a snapshot to restore.
///
/// `TaskManagedEBSVolumeConfiguration$sizeInGiB` states the rule in a sentence of its own:
/// "You must specify either a volume size or a snapshot ID. If you specify a snapshot ID, the
/// snapshot size is used for the volume size by default" — a configuration stating neither
/// asks for a volume Amazon EBS could not create. The documented sentence IS the refusal, so
/// it carries nothing of the request.
pub const VOLUME_SIZE_REQUIRED_MESSAGE: &str =
    "You must specify either a volume size or a snapshot ID.";

/// The refusal a launch earns for a tag specification that uses one of the keys Amazon ECS
/// keeps for itself.
///
/// `EBSTagSpecification$tags` states that rule in a sentence of its own too:
/// "`AmazonECSCreated` and `AmazonECSManaged` are reserved tags that can't be used" — so that
/// sentence is the refusal, naming the two keys it is about and nothing of the request.
pub const RESERVED_VOLUME_TAG_MESSAGE: &str =
    "AmazonECSCreated and AmazonECSManaged are reserved tags that can't be used.";

impl Volume {
    /// Whether this volume of a task definition is one the LAUNCH configures
    /// (`Volume$configuredAtLaunch`) — the one reading of that member, so the validator and
    /// the provisioning pass cannot disagree about which volumes a launch owes a
    /// configuration for.
    pub fn is_configured_at_launch(&self) -> bool {
        self.configured_at_launch == Some(true)
    }
}

/// The configurations a launch carries, indexed by the name each is matched to a task
/// definition's volume by — built ONCE per call, so a launch's own list is walked once and
/// every match below is a single lookup.
///
/// Both lists are caller-chosen — the launch's configurations and the stored revision's
/// volumes — so matching one by scanning the other would make the work their PRODUCT; this is
/// what keeps it the sum. A name the launch repeats names the volume once: the first
/// configuration of it is the one that configures it.
fn configurations_by_name(
    configurations: &[TaskVolumeConfiguration],
) -> HashMap<&str, &TaskVolumeConfiguration> {
    let mut indexed: HashMap<&str, &TaskVolumeConfiguration> =
        HashMap::with_capacity(configurations.len());
    for configuration in configurations {
        indexed.entry(configuration.name.as_str()).or_insert(configuration);
    }
    indexed
}

/// The names of the volumes `volumes` declares `configuredAtLaunch`, as the set a
/// configuration is matched against — built once, for the reason
/// [`configurations_by_name`] is.
fn names_configured_at_launch(volumes: &[Volume]) -> HashSet<&str> {
    volumes
        .iter()
        .filter(|volume| volume.is_configured_at_launch())
        .map(|volume| volume.name.as_deref().unwrap_or_default())
        .collect()
}

/// Whether one managed-volume configuration asks for a tag key Amazon ECS keeps for itself.
fn asks_for_a_reserved_tag(managed: &TaskManagedEBSVolumeConfiguration) -> bool {
    configuration_tags(managed).into_iter().any(|tag| {
        RESERVED_VOLUME_TAG_KEYS.contains(&tag.key.unwrap_or_default().as_str())
    })
}

/// The refusal a launch of `volumes` earns for the `volumeConfigurations` it carries, or
/// `None` when the two agree.
///
/// The rule, which the model cannot express because `volumeConfigurations` is optional on
/// every launch: a volume the task definition declared `configuredAtLaunch` is configured BY
/// THE LAUNCH, matched by name, so every such volume has to be named by a configuration
/// ([`missing_volume_configuration_message`]) that describes a volume to create (the same
/// sentence: a configuration carrying no `managedEBSVolume` configures nothing), and every
/// configuration has to name one
/// ([`unmatched_volume_configuration_message`]). The volume each configuration describes also
/// has to be one Amazon EBS could create — a size, or the snapshot whose size it takes
/// ([`VOLUME_SIZE_REQUIRED_MESSAGE`]) — and may not carry the tag keys Amazon ECS keeps for
/// itself ([`RESERVED_VOLUME_TAG_MESSAGE`]).
///
/// Shared by every launch that carries `TaskVolumeConfigurations`, so `RunTask` and
/// `StartTask` cannot answer differently about the same pair, and checked before any task is
/// stored: AWS places nothing when it refuses.
///
/// The LENGTH of the launch's own list is gated first, before any per-entry work
/// ([`MAX_VOLUME_CONFIGURATIONS`]), and the matching then runs off two indexes built once
/// each, so what one call costs is the SUM of the two lists rather than their product.
pub fn volume_configuration_violation(
    volumes: &[Volume],
    configurations: &[TaskVolumeConfiguration],
) -> Option<String> {
    if configurations.len() > MAX_VOLUME_CONFIGURATIONS {
        return Some(at_most_items_message(
            VOLUME_CONFIGURATIONS_SUBJECT,
            MAX_VOLUME_CONFIGURATIONS,
        ));
    }
    let configured = configurations_by_name(configurations);
    for volume in volumes
        .iter()
        .filter(|volume| volume.is_configured_at_launch())
    {
        let name = volume.name.as_deref().unwrap_or_default();
        let Some(configuration) = configured.get(name) else {
            return Some(missing_volume_configuration_message(name));
        };
        let Some(managed) = configuration.managed_ebs_volume.as_ref() else {
            return Some(missing_volume_configuration_message(name));
        };
        if managed.size_in_gi_b.is_none() && managed.snapshot_id.is_none() {
            return Some(VOLUME_SIZE_REQUIRED_MESSAGE.to_string());
        }
        if asks_for_a_reserved_tag(managed) {
            return Some(RESERVED_VOLUME_TAG_MESSAGE.to_string());
        }
    }
    let at_launch = names_configured_at_launch(volumes);
    configurations
        .iter()
        .find(|configuration| !at_launch.contains(configuration.name.as_str()))
        .map(|configuration| unmatched_volume_configuration_message(&configuration.name))
}

/// The tags one managed-volume configuration asks for: the entries of every tag
/// specification that names the volume resource type, which is the only resource one of
/// these configurations tags.
fn configuration_tags(managed: &TaskManagedEBSVolumeConfiguration) -> Vec<Tag> {
    managed
        .tag_specifications
        .iter()
        .flatten()
        .filter(|specification| specification.resource_type == EBS_TAG_RESOURCE_TYPE_VOLUME)
        .flat_map(|specification| specification.tags.clone().unwrap_or_default())
        .collect()
}

/// Whether the volume one configuration describes is deleted when the task stops
/// ([`DEFAULT_DELETE_ON_TERMINATION`] for a configuration that states no policy).
fn deletes_on_termination(managed: &TaskManagedEBSVolumeConfiguration) -> bool {
    managed
        .termination_policy
        .as_ref()
        .map_or(DEFAULT_DELETE_ON_TERMINATION, |policy| {
            policy.delete_on_termination
        })
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The attachments Amazon ECS allocates to a task it is placing for the volumes the
    /// launch configured at launch — one per volume the stored revision declared
    /// `configuredAtLaunch` ("one volume created for each task"), in the order the revision
    /// declared them, and none for a launch that configured none.
    ///
    /// Each starts [`ATTACHMENT_STATUS_PRECREATED`] carrying the three details the launch
    /// already supplied — the volume's name, the infrastructure role and the termination
    /// policy — and the CLOCK takes it on from there, one documented step at a time
    /// ([`INTERFACE_ATTACH_LIFECYCLE`]), exactly as a task's interface attachment moves; the
    /// volume's own id is reported once the store that owns Amazon EBS volumes holds it
    /// ([`settle_managed_ebs_volumes`]).
    ///
    /// The launch is validated before this is called ([`volume_configuration_violation`]),
    /// so a volume here has a configuration and a configuration here has a volume.
    ///
    /// The launch's own configurations are indexed once ([`configurations_by_name`]), so the
    /// work is the sum of the two lists rather than their product.
    pub fn mint_managed_ebs_attachments(
        &mut self,
        volumes: &[Volume],
        configurations: &[TaskVolumeConfiguration],
        now: SystemTime,
    ) -> Vec<AttachmentRecord> {
        let configured = configurations_by_name(configurations);
        let mut attachments = Vec::new();
        for volume in volumes
            .iter()
            .filter(|volume| volume.is_configured_at_launch())
        {
            let name = volume.name.clone().unwrap_or_default();
            let Some(managed) = configured
                .get(name.as_str())
                .and_then(|configuration| configuration.managed_ebs_volume.as_ref())
            else {
                continue;
            };
            let details = vec![
                KeyValuePair {
                    name: Some(ATTACHMENT_DETAIL_VOLUME_NAME.to_string()),
                    value: Some(name),
                },
                KeyValuePair {
                    name: Some(ATTACHMENT_DETAIL_ROLE_ARN.to_string()),
                    value: Some(managed.role_arn.clone()),
                },
                KeyValuePair {
                    name: Some(ATTACHMENT_DETAIL_DELETE_ON_TERMINATION.to_string()),
                    value: Some(deletes_on_termination(managed).to_string()),
                },
            ];
            attachments.push(AttachmentRecord {
                id: self.next_attachment_id(),
                sequence: self.next_attachment_seq,
                attachment_type: ATTACHMENT_TYPE_AMAZON_ELASTIC_BLOCK_STORAGE.to_string(),
                status: ATTACHMENT_STATUS_PRECREATED.to_string(),
                details,
                next_status_due_at: Some(now + INTERFACE_STEP_SPAN),
                // Only an elastic network interface carries one of those
                // ([`AttachmentRecord::report_allocated_interface`]); the resource THIS
                // attachment stands for is reported by its volume id.
                allocated: None,
                interface_released: false,
            });
        }
        attachments
    }

    /// Every attachment of a managed Amazon EBS volume that is still owed its volume: one
    /// whose task is still wanted running and which names no volume yet.
    ///
    /// A pass over the task map, which is bounded by [`MAX_TASKS`], so what one call costs
    /// follows what the store HOLDS rather than anything in a request. Each task's own
    /// configurations are indexed once and only the configuration of a volume that is
    /// actually owed one is cloned ([`configurations_by_name`]), so a task's list is walked
    /// once per pass rather than once per attachment.
    pub fn managed_volumes_awaiting_provisioning(&self) -> Vec<PendingManagedVolume> {
        let mut pending = Vec::new();
        for task in self.tasks.values() {
            if task.desired_status != STATUS_RUNNING || task.last_status == STATUS_STOPPED {
                continue;
            }
            let mut outstanding = task
                .attachments
                .iter()
                .filter(|attachment| attachment.is_managed_ebs_volume())
                .filter(|attachment| attachment.volume_id().is_none())
                .peekable();
            if outstanding.peek().is_none() {
                continue;
            }
            let configured = configurations_by_name(&task.volume_configurations);
            let region = ::service_common::arn::parse(&task.task_arn)
                .map(|arn| arn.region.to_string())
                .unwrap_or_default();
            for attachment in outstanding {
                let Some(managed) = attachment
                    .volume_name()
                    .and_then(|name| configured.get(name))
                    .and_then(|configuration| configuration.managed_ebs_volume.clone())
                else {
                    continue;
                };
                pending.push(PendingManagedVolume {
                    task_arn: task.task_arn.clone(),
                    attachment_id: attachment.id.clone(),
                    availability_zone: task.availability_zone.clone(),
                    region: region.clone(),
                    configuration: managed,
                });
            }
        }
        pending
    }

    /// Report, on one task's attachment, the Amazon EBS volume the owning store created for
    /// it: the id is the last of the details that attachment carries
    /// ([`ATTACHMENT_DETAIL_VOLUME_ID`]), and it is reported only once the volume exists.
    ///
    /// `Task$version`: "Every time a task experiences a change ... the version counter is
    /// incremented" — the attachment gaining the resource it stands for is such a change.
    pub fn record_managed_ebs_volume(
        &mut self,
        task_arn: &str,
        attachment_id: &str,
        volume_id: &str,
    ) {
        let Some(task) = self.tasks.get_mut(task_arn) else {
            return;
        };
        let Some(attachment) = task
            .attachments
            .iter_mut()
            .find(|attachment| attachment.id == attachment_id)
        else {
            return;
        };
        if attachment.volume_id().is_some() {
            return;
        }
        attachment.details.push(KeyValuePair {
            name: Some(ATTACHMENT_DETAIL_VOLUME_ID.to_string()),
            value: Some(volume_id.to_string()),
        });
        task.version += 1;
    }
}

impl AttachmentRecord {
    /// Whether this attachment is the managed Amazon EBS volume a launch configured at
    /// launch, rather than some other kind of attachment a task can carry.
    pub fn is_managed_ebs_volume(&self) -> bool {
        self.attachment_type == ATTACHMENT_TYPE_AMAZON_ELASTIC_BLOCK_STORAGE
    }

    /// The Amazon EBS volume this attachment stands for, read off the stored details rather
    /// than recomputed, so it is the value a caller of `DescribeTasks` sees — and `None`
    /// while the volume has not been created yet.
    pub fn volume_id(&self) -> Option<&str> {
        self.detail_value(ATTACHMENT_DETAIL_VOLUME_ID)
    }

    /// The name the launch matched this volume to the task definition by, read off the same
    /// details.
    pub fn volume_name(&self) -> Option<&str> {
        self.detail_value(ATTACHMENT_DETAIL_VOLUME_NAME)
    }

    /// The value the `Attachment$details` entry named `name` carries, and `None` when this
    /// attachment carries no such entry.
    fn detail_value(&self, name: &str) -> Option<&str> {
        self.details
            .iter()
            .find(|detail| detail.name.as_deref() == Some(name))
            .and_then(|detail| detail.value.as_deref())
    }
}

/// What ONE task attachment needs created in Amazon EC2: the volume the launch described,
/// and where it goes.
///
/// Collected under this store's lock
/// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::managed_volumes_awaiting_provisioning`])
/// and created against the Amazon EC2 store under that store's own, so no two locks are held
/// at once.
#[derive(Debug, Clone)]
pub struct PendingManagedVolume {
    /// The task the volume was created for, which is the resource it is attached to.
    pub task_arn: String,
    /// `Attachment$id` of the attachment it stands for.
    pub attachment_id: String,
    /// The Availability Zone the task runs in, which is where its volume has to be created:
    /// an Amazon EBS volume is attached to a resource in its own zone. `None` for a task
    /// this emulator holds no zone for.
    pub availability_zone: Option<String>,
    /// The Region the task is in, read off its own ARN.
    pub region: String,
    /// The volume the launch described.
    pub configuration: TaskManagedEBSVolumeConfiguration,
}

/// Create in Amazon EC2 the managed Amazon EBS volume every task this store placed with a
/// `configuredAtLaunch` volume is owed, and report each one on the attachment it stands for.
///
/// Called with NO guard of this service's store held, from the pass every operation that
/// places a task takes and the clock pump drives ([`settle_task_launches`]): Amazon ECS
/// creates and attaches the volume while the task is `PROVISIONING`, before anything of the
/// task is started, so the volume exists by the time the attachment reports it.
///
/// Nothing is created when the owning store cannot hold another volume: the attachment then
/// reports the name, the role and the policy the launch supplied and no volume id, rather
/// than naming a volume that does not exist.
pub fn settle_managed_ebs_volumes<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let pending = state.lock().managed_volumes_awaiting_provisioning();
    if pending.is_empty() {
        return;
    }
    let Some(ec2_state) = ec2::service_state(engine) else {
        return;
    };
    let created: Vec<(PendingManagedVolume, String)> = {
        let mut ec2_store = ec2_state.lock();
        pending
            .into_iter()
            .filter_map(|wanted| {
                create_managed_volume(&mut ec2_store, &wanted).map(|volume_id| (wanted, volume_id))
            })
            .collect()
    };
    if created.is_empty() {
        return;
    }
    let mut store = state.lock();
    for (wanted, volume_id) in created {
        store.record_managed_ebs_volume(&wanted.task_arn, &wanted.attachment_id, &volume_id);
    }
}

/// Create ONE task's managed Amazon EBS volume in Amazon EC2 and attach it to that task, and
/// answer the `VolumeId` that store minted — `None` when it could not hold another volume.
///
/// EVERY member of the configuration Amazon EBS creates a volume from is carried into that
/// create — the size, the type, the IOPS, the throughput, the encryption state and key, and
/// the snapshot the volume is restored from — so the volume the account holds is the one the
/// launch asked for. The volume is created in the task's own Availability Zone, because an
/// Amazon EBS volume is attached to a resource in its zone; a task this emulator holds no
/// zone for takes the first zone of its Region, which is a zone that Region really has
/// ([`ec2::availability_zones_in_region`]).
///
/// The tags are the ones the configuration asked for plus the two Amazon ECS applies itself
/// ([`RESERVED_VOLUME_TAG_KEYS`]: "Amazon ECS applies service-managed tags by default"), so
/// `DescribeVolumes` resolves the volume by the tags the caller gave it.
///
/// The attachment is the one Amazon EC2 reports for a volume attached to a resource Amazon
/// Web Services manages: `VolumeAttachment$AssociatedResource` is "the ARN of the Amazon Web
/// Services-managed resource to which the volume is attached" — the task — and
/// `InstanceOwningService` the principal of the service that owns it; such an attachment
/// reports no `InstanceId` and no `Device` ("if the volume is attached to an Amazon Web
/// Services-managed resource, this parameter returns null").
fn create_managed_volume(
    ec2_store: &mut ec2::ComAmazonawsEc2Amazonec2State,
    wanted: &PendingManagedVolume,
) -> Option<String> {
    let zone = wanted.availability_zone.clone().or_else(|| {
        ec2::availability_zones_in_region(&wanted.region)
            .first()
            .map(|zone| zone.zone_name.clone())
    })?;
    let managed = &wanted.configuration;
    let volume_id = ec2_store.insert_volume(ec2::VolumeSpec {
        availability_zone: zone,
        size: managed.size_in_gi_b,
        volume_type: managed.volume_type.clone(),
        iops: managed.iops,
        throughput: managed.throughput,
        encrypted: managed.encrypted,
        kms_key_id: managed.kms_key_id.clone(),
        snapshot_id: managed.snapshot_id.clone(),
        tags: volume_tags(wanted),
        // Multi-Attach is asked for at creation and this configuration has no member for
        // it, so the volume Amazon ECS creates for one task does not carry it.
        multi_attach_enabled: None,
        // The volume is created for a task rather than by a `CreateVolume` call, so no
        // client token names it.
        client_token: None,
    })?;
    let attachment = AttachVolumeOutput {
        volume_id: Some(volume_id.clone()),
        // "The ARN of the Amazon Web Services-managed resource to which the volume is
        // attached" — the task Amazon ECS created the volume for.
        associated_resource: Some(wanted.task_arn.clone()),
        instance_owning_service: Some(ECS_SERVICE_PRINCIPAL.to_string()),
        // "If the volume is attached to an Amazon Web Services-managed resource, this
        // parameter returns null" — of both of these.
        instance_id: None,
        device: None,
        state: Some(ec2::VOLUME_ATTACHMENT_STATE_ATTACHED.to_string()),
        attach_time: Some(ec2::system_time_to_date_time(SystemTime::now())),
        delete_on_termination: Some(deletes_on_termination(managed)),
        // The EBS card is an instance's; a volume attached to a managed resource sits on
        // none.
        ebs_card_index: None,
    };
    ec2_store.attach_volume_to_managed_resource(&volume_id, attachment);
    Some(volume_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ops::types::{EBSTagSpecification, HostVolumeProperties};

    /// A task-definition volume the LAUNCH configures.
    fn at_launch(name: &str) -> Volume {
        Volume {
            name: Some(name.to_string()),
            configured_at_launch: Some(true),
            ..Default::default()
        }
    }

    /// A task-definition volume the launch does NOT configure: a bind-mount host volume.
    fn host(name: &str) -> Volume {
        Volume {
            name: Some(name.to_string()),
            host: Some(HostVolumeProperties::default()),
            ..Default::default()
        }
    }

    /// One managed-volume configuration, with the members a row varies.
    fn configuration(
        name: &str,
        size_in_gi_b: Option<i32>,
        snapshot_id: Option<&str>,
        tags: &[(&str, &str)],
    ) -> TaskVolumeConfiguration {
        TaskVolumeConfiguration {
            name: name.to_string(),
            managed_ebs_volume: Some(TaskManagedEBSVolumeConfiguration {
                role_arn: "arn:aws:iam::000000000000:role/infrastructure".to_string(),
                size_in_gi_b,
                snapshot_id: snapshot_id.map(ToString::to_string),
                tag_specifications: (!tags.is_empty()).then(|| {
                    vec![EBSTagSpecification {
                        resource_type: EBS_TAG_RESOURCE_TYPE_VOLUME.to_string(),
                        tags: Some(
                            tags.iter()
                                .map(|(key, value)| Tag {
                                    key: Some((*key).to_string()),
                                    value: Some((*value).to_string()),
                                })
                                .collect(),
                        ),
                        propagate_tags: None,
                    }]
                }),
                ..Default::default()
            }),
        }
    }

    /// One row of the table below: a stored revision's volumes, the launch's configurations
    /// and the refusal the pair earns. A named struct rather than a tuple, so the row reads
    /// at its use site and the table's type stays simple.
    struct ConfigurationRow {
        case: &'static str,
        volumes: Vec<Volume>,
        configurations: Vec<TaskVolumeConfiguration>,
        refusal: Option<String>,
    }

    /// RULE: a volume a revision declared `configuredAtLaunch` is configured by the launch,
    /// matched by name, and the volume a configuration describes has to be one Amazon EBS
    /// could create.
    #[test]
    fn a_launch_configures_every_volume_configured_at_launch_by_name() {
        let rows: Vec<ConfigurationRow> = vec![
            ConfigurationRow {
                case: "no volume configured at launch and no configuration",
                volumes: vec![host("scratch")],
                configurations: Vec::new(),
                refusal: None,
            },
            ConfigurationRow {
                case: "a configuration naming each volume configured at launch",
                volumes: vec![host("scratch"), at_launch("data")],
                configurations: vec![configuration("data", Some(4), None, &[])],
                refusal: None,
            },
            ConfigurationRow {
                case: "a configuration restoring a snapshot instead of naming a size",
                volumes: vec![at_launch("data")],
                configurations: vec![configuration(
                    "data",
                    None,
                    Some("snap-0123456789abcdef0"),
                    &[],
                )],
                refusal: None,
            },
            ConfigurationRow {
                case: "a tag specification carrying only the caller's own keys",
                volumes: vec![at_launch("data")],
                configurations: vec![configuration("data", Some(4), None, &[("Owner", "me")])],
                refusal: None,
            },
            ConfigurationRow {
                case: "a volume configured at launch and no configuration at all",
                volumes: vec![at_launch("data")],
                configurations: Vec::new(),
                refusal: Some(missing_volume_configuration_message("data")),
            },
            ConfigurationRow {
                case: "a configuration naming another volume",
                volumes: vec![at_launch("data")],
                configurations: vec![configuration("other", Some(4), None, &[])],
                refusal: Some(missing_volume_configuration_message("data")),
            },
            ConfigurationRow {
                case: "a configuration carrying no managedEBSVolume",
                volumes: vec![at_launch("data")],
                configurations: vec![TaskVolumeConfiguration {
                    name: "data".to_string(),
                    managed_ebs_volume: None,
                }],
                refusal: Some(missing_volume_configuration_message("data")),
            },
            ConfigurationRow {
                case: "a configuration for a revision that configured no volume at launch",
                volumes: vec![host("scratch")],
                configurations: vec![configuration("scratch", Some(4), None, &[])],
                refusal: Some(unmatched_volume_configuration_message("scratch")),
            },
            ConfigurationRow {
                case: "a configuration naming no volume of the revision",
                volumes: vec![host("scratch")],
                configurations: vec![configuration("nothing", Some(4), None, &[])],
                refusal: Some(unmatched_volume_configuration_message("nothing")),
            },
            ConfigurationRow {
                case: "a managed volume with neither a size nor a snapshot",
                volumes: vec![at_launch("data")],
                configurations: vec![configuration("data", None, None, &[])],
                refusal: Some(VOLUME_SIZE_REQUIRED_MESSAGE.to_string()),
            },
            ConfigurationRow {
                case: "a tag specification using the reserved AmazonECSCreated key",
                volumes: vec![at_launch("data")],
                configurations: vec![configuration(
                    "data",
                    Some(4),
                    None,
                    &[("AmazonECSCreated", "mine")],
                )],
                refusal: Some(RESERVED_VOLUME_TAG_MESSAGE.to_string()),
            },
            ConfigurationRow {
                case: "a tag specification using the reserved AmazonECSManaged key",
                volumes: vec![at_launch("data")],
                configurations: vec![configuration(
                    "data",
                    Some(4),
                    None,
                    &[("Owner", "me"), ("AmazonECSManaged", "true")],
                )],
                refusal: Some(RESERVED_VOLUME_TAG_MESSAGE.to_string()),
            },
            ConfigurationRow {
                // "only one volume configured at launch is supported" (`Volume`), so a
                // launch configures at most one and the bound is stated before any entry of
                // the list is read.
                case: "more configurations than one launch may carry",
                volumes: vec![at_launch("data")],
                configurations: vec![
                    configuration("data", Some(4), None, &[]),
                    configuration("other", Some(4), None, &[]),
                ],
                refusal: Some(at_most_items_message(
                    VOLUME_CONFIGURATIONS_SUBJECT,
                    MAX_VOLUME_CONFIGURATIONS,
                )),
            },
        ];

        for row in rows {
            let ConfigurationRow {
                case,
                volumes,
                configurations,
                refusal: expected,
            } = row;
            let refusal = volume_configuration_violation(&volumes, &configurations);
            assert_eq!(
                refusal, expected,
                "case {case:?}: expected {expected:?}, got {refusal:?}"
            );
        }
    }
}
