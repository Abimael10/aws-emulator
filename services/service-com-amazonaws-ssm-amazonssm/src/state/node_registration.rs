// SPDX-License-Identifier: CC0-1.0

//! Which of the account's Amazon EC2 instances are REGISTERED with Systems Manager as
//! managed nodes, and the node record that registration writes.
//!
//! A managed node is not created by any call of this service: "SSM Agent is preinstalled …
//! on Amazon Linux 2023" and an instance whose INSTANCE PROFILE lets the agent call
//! Systems Manager registers itself shortly after it boots — which is what makes it a
//! managed node, what `DescribeInstanceInformation` then reports `Online`, and what the
//! `AWS:InstanceInformation` inventory item of [`super::inventory`] is the projection of.
//! So the registration is derived here from the three components that OWN its evidence,
//! each read through its own state accessors rather than through a second copy of them:
//!
//! 1. **the instance** — Amazon EC2's own store
//!    (`ComAmazonawsEc2Amazonec2State::instances`): an instance exists, is `running` and
//!    carries the address, the host name, the type and the Availability Zone a node
//!    reports. An instance EC2 does not hold is no node here, and an instance it reports
//!    `stopped` or `terminated` is one "this operation does not return information for"
//!    (`DescribeInstanceInformation`), so the node it registered goes with it.
//! 2. **the instance profile** — the association EC2 holds between the instance and an IAM
//!    instance profile (`iam_instance_profile_of_instance`), which is how an agent on the
//!    instance gets credentials at all.
//! 3. **what that profile's role may do** — IAM's own records for the role the profile
//!    carries: the role has to be one Amazon EC2 can act as (its trust policy), and its
//!    identity-based policies have to let the agent perform the call it registers with
//!    ([`REGISTRATION_ACTION`], which the AWS-managed `AmazonSSMManagedInstanceCore` policy
//!    grants). The documents come from IAM's own collection
//!    (`principal_policies`) and the decision from the tree's one evaluation of an IAM
//!    document ([`::service_common::iam_authorization`]), so this service and IAM's own
//!    authorizer cannot disagree about what a role allows. That role is also WHAT THE NODE
//!    REPORTS as its `IamRole`: "the role assigned to an Amazon EC2 instance configured with
//!    a Systems Manager Quick Setup host management configuration or the role assigned to an
//!    on-premises managed node … This call doesn't return the IAM role for *unmanaged*
//!    Amazon EC2 instances" (`InstanceInformation$IamRole`) — an instance that registered is
//!    a managed one, so the role whose credentials its agent registered with is reported,
//!    under the name IAM holds it by, which is the form a hybrid activation's node reports
//!    too (`CreateActivationRequest$IamRole` is a role NAME).
//!
//! An instance whose role's policies this emulator cannot READ — an attachment of AWS's own
//! catalog whose document IAM holds nothing for — registers: refusing on evidence this
//! emulator does not have would withhold a node AWS manages, the same reading
//! [`super::identity_authorization`] takes of such an attachment for the caller's own
//! grants. A role whose documents ARE readable and allow nothing registers no node.
//!
//! ## What is written, and why a report may write it
//!
//! Registration takes a while after the launch — the agent has to come up and reach the
//! service — so it falls due [`REGISTRATION_DELAY`] after the instance was launched,
//! whether or not anybody calls in between, and a caller that reads straight after
//! `RunInstances` sees the instance not yet managed, as it would against the real service.
//! [`settle_registrations`] is what materialises the registrations the clock has already
//! made due: that write was owed at the same instant whether or not anybody called, which
//! is why a REPORTING operation of this service takes it before it reads the node store.
//! Nothing else about a node is rewritten afterwards — a registration happens once, and
//! what the agent reported then is what the record keeps — except the TAGS the node
//! reports, which are the instance's own tag set rather than a copy frozen at registration.
//! What the node store can hold this way is bounded by the instance store it is derived from
//! (Amazon EC2's own `MAX_STORED_INSTANCES`): at most one node per instance of that store,
//! per Region.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime};

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use ::service_common::iam_authorization::{self as authorization, Decision};

use super::{ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE};

/// How long after its launch an instance's registration with Systems Manager falls due.
///
/// AWS publishes no span for it — it is however long the agent takes to come up and reach
/// the service — so this is the emulator's CHOSEN transient window: three seconds, the span
/// this emulator gives a transition nobody documents (see
/// [`super::DOCUMENT_TRANSITION_DELAY`]). It is deliberately never zero: a caller that
/// describes managed nodes straight after a launch sees the instance not managed yet, the
/// way it would against the real service.
pub const REGISTRATION_DELAY: Duration = Duration::from_secs(3);

/// The IAM action an agent registers its node with, and so the one a profile's role has to
/// allow for the instance to become a managed node: `ssm:UpdateInstanceInformation`
/// ("Actions defined by AWS Systems Manager", IAM Service Authorization Reference) — the
/// call the agent makes to report itself, and the permission the AWS-managed
/// `AmazonSSMManagedInstanceCore` policy exists to grant.
pub const REGISTRATION_ACTION: &str = "ssm:UpdateInstanceInformation";

/// The service principal an instance profile's role has to trust for Amazon EC2 to act as
/// it on an instance's behalf, so that the agent on the instance has credentials at all.
pub const EC2_SERVICE_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The SSM Agent build the managed nodes of this emulator report
/// (`InstanceInformation$AgentVersion`, pattern `^[0-9]{1,6}(\.[0-9]{1,6}){2,3}$`).
///
/// One build for the whole emulator, so every node reports the agent this emulator
/// emulates rather than a per-node guess — and the version it reports is the latest one
/// this emulator publishes, which is why a node also reports
/// `InstanceInformation$IsLatestVersion`.
pub const SSM_AGENT_VERSION: &str = "3.3.2746.0";

/// The node status a registered node whose instance is running reports
/// (`InstanceProperty$InstanceState`, the `InstanceStatus` attribute of the
/// `AWS:InstanceInformation` inventory type): the agent is responding, which is the status
/// Systems Manager reports beside the `Stopped` and `Terminated` ones `GetInventory`
/// documents — states whose instances are not reported at all here, because
/// `DescribeInstanceInformation` "does not return information for nodes that are either
/// Stopped or Terminated".
pub const NODE_STATUS_ACTIVE: &str = "Active";

/// The `PlatformType` of a node running Windows (from the modelled enum,
/// `crate::ops::types::PLATFORM_TYPE_VALUES`), which is the platform of an instance whose
/// AMI declares `Image$Platform`.
pub const PLATFORM_TYPE_WINDOWS: &str = "Windows";

/// The `PlatformType` of a node running Linux (same enum): the platform of an instance
/// whose AMI declares no `Image$Platform` at all, which is how Amazon EC2 reports every
/// non-Windows image ("The value is `windows` for Windows AMIs; otherwise blank").
pub const PLATFORM_TYPE_LINUX: &str = "Linux";

/// Materialise every managed-node registration of `region` the clock already owes, so a
/// read of the node store reports what was due whether or not anybody called.
///
/// Call it with NO guard of this service's store held: it reads Amazon EC2's and IAM's
/// stores and then writes this service's.
///
/// The two writes are the ones the clock owes: a node whose instance was launched more than
/// [`REGISTRATION_DELAY`] ago and whose profile lets the agent register is added, and a node
/// THIS projection registered whose instance no longer qualifies — gone, stopped,
/// terminated, or no longer running as such a profile — is dropped, because
/// `DescribeInstanceInformation` reports no node for it. A node registered any other way (a
/// hybrid activation's) is left alone: its registration is not this instance's to withdraw.
///
/// A node this projection already registered also has the TAGS it reports brought in line
/// with the instance's own, because they are the instance's tags rather than something the
/// registration froze: that is what a `tag:` target selects a node by and what Patch Manager
/// reads a node's patch group from.
pub fn settle_registrations<Eng: Send + Sync + 'static>(engine: &Eng, region: &str) {
    let registered = registered_nodes(engine, region, SystemTime::now());
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let ids: BTreeSet<&str> = registered
        .iter()
        .filter_map(ManagedNodeRecord::node_id)
        .collect();
    let mut store = state.lock();
    store.managed_nodes.retain(|(held_region, id), node| {
        held_region != region
            || !node.registered_from_instance
            || ids.contains(id.as_str())
    });
    for node in registered {
        let Some(id) = node.node_id().map(str::to_string) else {
            continue;
        };
        match store.managed_node_mut(region, &id) {
            None => {
                store.insert_managed_node(region, node);
            }
            Some(held) if held.registered_from_instance => {
                // A registration happens once, so nothing else about the node is rewritten
                // — except the TAGS it reports, which are the instance's own: an instance
                // tagged after its agent registered carries that tag, and a tag only
                // Systems Manager holds for the node (`AddTagsToResource`) is left alone.
                //
                // A tag REMOVED from the instance is not reconciled: the two sources are
                // held in one map, so dropping what the instance no longer carries would
                // drop the tags Systems Manager holds of its own with it. Telling them
                // apart needs a per-tag source on the record, which is left to the change
                // that gives the node store one.
                for (key, value) in node.tags {
                    held.tags.insert(key, value);
                }
            }
            Some(_) => {}
        }
    }
}

/// The managed nodes `region` holds by REGISTRATION at `now`: one record per EC2 instance
/// of that Region whose registration has fallen due and whose instance profile lets the
/// agent register.
///
/// A read of two other services' stores and nothing else; each guard is released before the
/// next is taken.
fn registered_nodes<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    now: SystemTime,
) -> Vec<ManagedNodeRecord> {
    let Some(ec2_state) = ec2::service_state(engine) else {
        // No Amazon EC2 store on this engine is no instance to register, so this endpoint
        // reports the nodes it holds and invents none.
        return Vec::new();
    };
    let candidates = {
        let store = ec2_state.lock();
        let settled = Instant::now();
        store
            .instances
            .values()
            .filter(|instance| ec2::region_of_zone_name(&instance.placement_az) == region)
            .filter(|instance| is_running(instance, settled))
            .filter(|instance| registered_at(instance) <= now)
            .filter_map(|instance| ManagedInstance::of(&store, instance))
            .collect::<Vec<ManagedInstance>>()
    };
    if candidates.is_empty() {
        return Vec::new();
    }
    let Some(iam_state) = iam::service_state(engine) else {
        // No IAM store on this engine is no record of what a profile's role may do, and a
        // grant this emulator cannot read is not evidence against the registration (see
        // the module docs), so every profile-bearing instance registers — reporting no role,
        // because which role the profile carries is a fact only IAM holds.
        return candidates
            .into_iter()
            .map(|candidate| candidate.node(None))
            .collect();
    };
    let iam_store = iam_state.lock();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let role = registration_role(&iam_store, &candidate.profile_arn)?;
            Some(candidate.node(Some(role)))
        })
        .collect()
}

/// What one candidate instance carries into the node record it registers: the facts a
/// managed node reports about itself, read off Amazon EC2's own record of the instance.
#[derive(Debug, Clone)]
struct ManagedInstance {
    /// The instance id, which is the managed node's id.
    id: String,
    /// The ARN of the IAM instance profile the instance runs as.
    profile_arn: String,
    /// When the registration fell due, which is when the node registered.
    registered_at: SystemTime,
    /// `InstanceInformation$PlatformType`, from the AMI the instance was launched from.
    platform_type: &'static str,
    /// `InstanceInformation$IPAddress`: the instance's private address.
    ip_address: String,
    /// `InstanceInformation$ComputerName`: "the fully qualified host name of the managed
    /// node", which is the instance's private DNS name.
    computer_name: String,
    /// `InstanceProperty$InstanceType`.
    instance_type: String,
    /// `InstanceProperty$KeyName`, for an instance launched with a key pair.
    key_name: Option<String>,
    /// `InstanceProperty$Architecture`.
    architecture: String,
    /// `InstanceProperty$LaunchTime`.
    launch_time: SystemTime,
    /// `InstanceProperty$AvailabilityZone`.
    availability_zone: String,
    /// The instance's own tags, which are the tags the managed node it registers carries:
    /// the node IS that instance, so a `tag:` target and a node's patch group are read off
    /// the tags the instance holds (`Target`: "you could specify `tag:ServerRole` … to run
    /// a command on instances that include EC2 tags of `ServerRole,WebServer`").
    tags: BTreeMap<String, String>,
}

impl ManagedInstance {
    /// The candidate `instance` is, or `None` for an instance that runs as no IAM instance
    /// profile — which is an instance whose agent has no credentials to register with.
    fn of(
        store: &ec2::ComAmazonawsEc2Amazonec2State,
        instance: &ec2::InstanceRecord,
    ) -> Option<Self> {
        let profile_arn = store
            .iam_instance_profile_of_instance(&instance.instance_id)
            .and_then(|profile| profile.arn)?;
        Some(Self {
            id: instance.instance_id.clone(),
            profile_arn,
            registered_at: registered_at(instance),
            platform_type: platform_type(store, instance),
            ip_address: instance.private_ip.clone(),
            computer_name: instance.private_dns_name.clone(),
            instance_type: instance.instance_type.clone(),
            key_name: instance.key_name.clone(),
            architecture: instance.architecture.clone(),
            launch_time: instance.launch_time,
            availability_zone: instance.placement_az.clone(),
            tags: instance.tags.clone(),
        })
    }

    /// The node record this instance registered with.
    ///
    /// Every member is a fact of the instance or of the agent this emulator runs on it. The
    /// members an agent reports from INSIDE the guest that nothing here reads — the
    /// operating system's own name and version (`PlatformName`, `PlatformVersion`) — are
    /// left unset rather than invented, and so are the ones Systems Manager fills in for a
    /// node of another kind: the `ActivationId` and `Name` of a hybrid activation's node,
    /// and the `SourceId`/`SourceType`/`SourceLocation` of one in another cloud.
    ///
    /// `iam_role` is the name of the role the instance's instance profile carries, which is
    /// the role the node registered as and so what it reports as its own `IamRole`
    /// ([`registration_role`]); `None` only where which role that is cannot be read at all.
    fn node(self, iam_role: Option<String>) -> ManagedNodeRecord {
        let mut node =
            ManagedNodeRecord::registering(self.id, RESOURCE_TYPE_EC2_INSTANCE, self.registered_at);
        node.registered_from_instance = true;
        // The node runs the one agent build this emulator publishes, which is therefore the
        // latest one it knows of.
        node.is_latest_version = Some(true);
        // The tags of the node are the INSTANCE's own tags: it is the same resource, which
        // is why a tag target selects it and why Patch Manager reads a node's patch group
        // off them ([`super::patch_runs::PATCH_GROUP_TAG_KEYS`]).
        node.tags = self.tags;
        let reported = &mut node.properties;
        reported.agent_version = Some(SSM_AGENT_VERSION.to_string());
        reported.iam_role = iam_role;
        reported.platform_type = Some(self.platform_type.to_string());
        reported.ip_address = Some(self.ip_address);
        reported.computer_name = Some(self.computer_name);
        reported.instance_state = Some(NODE_STATUS_ACTIVE.to_string());
        reported.instance_type = Some(self.instance_type);
        reported.key_name = self.key_name;
        reported.architecture = Some(self.architecture);
        reported.launch_time = Some(::aws_smithy_types::DateTime::from(self.launch_time));
        reported.availability_zone = Some(self.availability_zone);
        node
    }
}

/// When `instance`'s registration with Systems Manager fell due: [`REGISTRATION_DELAY`]
/// after the instant it was launched.
///
/// Derived from the stored launch instant rather than from the clock, so it is the same
/// instant on every read — which is what lets the node record report one `RegistrationDate`
/// however often it is materialised.
fn registered_at(instance: &ec2::InstanceRecord) -> SystemTime {
    instance.launch_time + REGISTRATION_DELAY
}

/// Whether Amazon EC2 holds `instance` in the one state a managed node is reported in.
///
/// The state is the one that service owes the instance at `settled`: an instance whose move
/// to `running` has already fallen due IS running, whether or not anybody has called Amazon
/// EC2 since — materialising it is that service's own write, not this one's, so the state
/// is derived here rather than written.
fn is_running(instance: &ec2::InstanceRecord, settled: Instant) -> bool {
    let state = match instance.pending_transition.as_ref() {
        Some(pending) if pending.due_at <= settled => pending.to,
        _ => instance.state,
    };
    state == ec2::InstanceStateName::Running
}

/// The `PlatformType` of the node `instance` registers: the platform of the AMI it was
/// launched from, which Amazon EC2 records as `Image$Platform` — "the value is `windows`
/// for Windows AMIs; otherwise blank".
///
/// An instance whose image this emulator no longer holds reports the non-Windows platform
/// its absent image would have: the image record is the only place that fact lives, and an
/// image can be deregistered after an instance was launched from it.
fn platform_type(
    store: &ec2::ComAmazonawsEc2Amazonec2State,
    instance: &ec2::InstanceRecord,
) -> &'static str {
    let windows = instance
        .image_id
        .as_deref()
        .and_then(|image_id| store.images.get(image_id))
        .and_then(|image| image.platform.as_deref())
        .is_some_and(|platform| platform.eq_ignore_ascii_case(ec2::IMAGE_PLATFORM_WINDOWS));
    if windows {
        PLATFORM_TYPE_WINDOWS
    } else {
        PLATFORM_TYPE_LINUX
    }
}

/// The NAME of the role an agent on an instance running as the instance profile
/// `profile_arn` names registers its node as — IAM holds the profile, it carries a role
/// Amazon EC2 may act as, and that role's identity-based policies allow
/// [`REGISTRATION_ACTION`] — or `None` when that profile lets the agent register no node at
/// all.
///
/// The name is the one IAM holds the role by rather than its ARN or its path: that is what a
/// managed node reports in `IamRole`, and what a filter on that key is matched against.
///
/// The collection of a role's documents is IAM's own
/// (`principal_policies`) and the decision over them is the tree's one evaluation of an IAM
/// document, so what a role allows here is what IAM's own authorizer would say. IAM's order:
/// an explicit `Deny` in a document that CAN be read withholds the registration whatever
/// else the role carries; otherwise a grant is needed, which a role carrying an attachment
/// this emulator holds no document for has by definition (see the module docs).
fn registration_role(
    store: &iam::ComAmazonawsIamAwsidentitymanagementv20100508State,
    profile_arn: &str,
) -> Option<String> {
    let name = ::service_common::iam_role::instance_profile_name(profile_arn)?;
    // A profile IAM holds no record of carries no role, so nothing says the agent may
    // register — the same position an instance with no profile at all is in.
    let profile = store.instance_profiles.get(&iam::name_key(name))?;
    let role = profile
        .role_keys
        .first()
        .and_then(|role_key| store.roles.get(role_key))?;
    // "An instance profile can contain only one role" (`AddRoleToInstanceProfile`), so the
    // role above is the one the instance runs as.
    let admits_ec2 = role
        .assume_role_policy_document
        .as_deref()
        .and_then(|document| {
            ::service_common::iam_trust_policy::admits_service_principal(
                document,
                EC2_SERVICE_PRINCIPAL,
            )
        })
        // A trust document this emulator cannot read is no evidence that Amazon EC2 may not
        // act as the role, the same reading the grants below take of an unreadable one.
        .is_none_or(|admits| admits);
    if !admits_ec2 {
        return None;
    }
    let policies = store.principal_policies(&role.arn)?;
    let request = authorization::Request::new(
        REGISTRATION_ACTION,
        super::identity_authorization::ANY_RESOURCE,
        &[],
    );
    let registers = match authorization::identity_decision(&policies.documents, &request) {
        Decision::Allowed => true,
        Decision::Denied => false,
        // An attachment the collection resolved no document for is an entry of AWS's own
        // catalog this emulator does not carry: a grant it cannot read.
        Decision::NotStated => !policies.uncarried.is_empty(),
    };
    registers.then(|| role.role_name.clone())
}
