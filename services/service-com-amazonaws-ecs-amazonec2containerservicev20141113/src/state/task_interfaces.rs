// SPDX-License-Identifier: CC0-1.0

//! The elastic network interface an `awsvpc` task is allocated: what it carries, and when.
//!
//! `RegisterTaskDefinition$networkMode`: "If the network mode is `awsvpc`, the task is
//! allocated an elastic network interface" — Amazon ECS creates that interface while the
//! task provisions and attaches it before the task runs. `Attachment$details` "for elastic
//! network interfaces ... includes the network interface ID, the MAC address, the subnet ID,
//! and the private IPv4 address": the SUBNET is known the instant the attachment exists,
//! because the launch supplied it, and the other three are the interface's own and are
//! reported once that interface is IN PLACE — the attachment's own status is what moves
//! until then: `PRECREATED` the moment Amazon ECS creates the attachment, `ATTACHING` while
//! the interface is being attached, `ATTACHED` once it is.
//!
//! The subnet comes from the launch (`AwsVpcConfiguration$subnets`); the other three are
//! the interface's own and are derived from the counter its attachment id was minted out
//! of, so each interface keeps its own values across every read and no two interfaces share
//! one.
//!
//! The attachment's own STATUS is what the clock moves, one documented step at a time
//! ([`INTERFACE_ATTACH_LIFECYCLE`]), and the task it belongs to does not reach `PENDING`
//! before it is in place ([`placement_schedule`]) — which is also the instant the task is
//! `CONNECTED` ([`TaskRecord::report_connected`]).
//!
//! The interface belongs to the task, so it is RELEASED with it: Amazon ECS detaches and
//! deletes it as part of STOPPING the task, so the clock takes the attachment down the other
//! documented progression ([`INTERFACE_DETACH_LIFECYCLE`]) one step at a time while the task
//! is on its way out, the task's own `STOPPED` falls due at the instant that release finishes
//! ([`TaskRecord::interfaces_release_due_at`]), and the Amazon VPC record is torn down with
//! it ([`super::awsvpc_interfaces`]).

use super::*;

use crate::ops::types::NetworkInterface;

/// `Attachment$status` of an attachment whose resource is in place. From the member's own
/// list of statuses ("`PRECREATED`, `CREATED`, `ATTACHING`, `ATTACHED`, `DETACHING`,
/// `DETACHED`, `DELETED`, and `FAILED`").
pub const ATTACHMENT_STATUS_ATTACHED: &str = "ATTACHED";

/// `Attachment$status` of an attachment Amazon ECS is in the middle of attaching to the
/// task it created it for, from the same list of statuses.
pub const ATTACHMENT_STATUS_ATTACHING: &str = "ATTACHING";

/// `Attachment$status` of an attachment whose resource is GONE: the interface Amazon ECS
/// created for a task is torn down with the task, so once the task has stopped the
/// attachment that stood for it reports the last of the statuses the member lists.
pub const ATTACHMENT_STATUS_DELETED: &str = "DELETED";

/// The statuses Amazon ECS takes an interface it is attaching THROUGH, in the order it
/// takes them: created for the task that is provisioning, being attached to it, and in
/// place. They are three of the values `Attachment$status` lists; the ones a RELEASE passes
/// through are [`INTERFACE_DETACH_LIFECYCLE`], and a failure is not something this
/// emulator's clock owes anybody.
pub const INTERFACE_ATTACH_LIFECYCLE: [&str; 3] = [
    ATTACHMENT_STATUS_PRECREATED,
    ATTACHMENT_STATUS_ATTACHING,
    ATTACHMENT_STATUS_ATTACHED,
];

/// `Attachment$status` of an attachment Amazon ECS is in the middle of detaching from the
/// task it created it for, from the same list of statuses.
pub const ATTACHMENT_STATUS_DETACHING: &str = "DETACHING";

/// `Attachment$status` of an attachment whose resource is no longer attached to the task
/// but has not been torn down yet, from the same list of statuses.
pub const ATTACHMENT_STATUS_DETACHED: &str = "DETACHED";

/// The statuses Amazon ECS takes an interface it is RELEASING through, in the order it
/// takes them: in place, being detached from the task, detached, and torn down. They are
/// four of the values `Attachment$status` lists, and they begin where
/// [`INTERFACE_ATTACH_LIFECYCLE`] ends — the release of a resource is a progression of its
/// own, so a client watching a task stop observes each step of it rather than one jump.
pub const INTERFACE_DETACH_LIFECYCLE: [&str; 4] = [
    ATTACHMENT_STATUS_ATTACHED,
    ATTACHMENT_STATUS_DETACHING,
    ATTACHMENT_STATUS_DETACHED,
    ATTACHMENT_STATUS_DELETED,
];

/// How long Amazon ECS takes over ONE step of [`INTERFACE_DETACH_LIFECYCLE`].
///
/// EMULATOR-CHOSEN, as [`INTERFACE_STEP_SPAN`] is, and one task transition
/// ([`TRANSITION_SPAN`]): releasing the interface is part of the task's own STOP — Amazon ECS
/// takes these steps before the task is stopped, which is what a task on its way out is
/// waiting for — so a step of the release is a step of that stop and the stop as a whole is
/// the sum of them.
pub const INTERFACE_RELEASE_STEP_SPAN: Duration = TRANSITION_SPAN;

/// How long Amazon ECS takes over ONE step of [`INTERFACE_ATTACH_LIFECYCLE`].
///
/// EMULATOR-CHOSEN — AWS publishes no duration for creating and attaching a task's
/// interface — and one task transition ([`TRANSITION_SPAN`]), the span a step of the RELEASE
/// already takes ([`INTERFACE_RELEASE_STEP_SPAN`]): creating and attaching the interface is
/// part of the task's own LAUNCH — it is what `PROVISIONING` reports and what the task's
/// `PENDING` waits for ([`placement_schedule`]) — so a step of the attach is a step of that
/// launch and the launch as a whole is the sum of them, exactly as the stop is the sum of the
/// release's.
///
/// One transition span is a span a reader really observes each status across: it is the span
/// every other step of the task's own lifecycle takes, including the `PENDING` an agent
/// acknowledges with `SubmitTaskStateChange` and the `ATTACHED` it acknowledges with
/// `SubmitAttachmentStateChanges` ("Sent to acknowledge that an attachment changed states").
/// It is also short enough that the whole lifecycle of a task — placed, started, its work
/// done, its interface released, stopped — is a handful of spans, which is what lets a client
/// run SEVERAL tasks one after another, each waiting out the one before it (the shape a
/// scenario that hands data from one task to the next over a shared volume takes), and get its
/// answer inside an ordinary poll budget.
pub const INTERFACE_STEP_SPAN: Duration = TRANSITION_SPAN;

/// What the clock owes a task the moment it is PLACED: the `lastStatus` it is placed in,
/// and when each of its next two transitions falls due.
///
/// A task placed onto capacity that already exists is `PENDING` with only its container
/// start owed ([`TRANSITION_SPAN`] away). A task that is allocated its own elastic network
/// interface is one step earlier in the lifecycle: Amazon ECS has to create and attach
/// that interface before the task can be launched, which is what `PROVISIONING` reports,
/// so such a task crosses `PROVISIONING` -> `PENDING` at the instant its interface is
/// attached ([`AttachmentRecord::attached_due_at`]) and `PENDING` -> `RUNNING` one
/// transition span after that. A task whose capacity still has to be ACQUIRED is
/// `PROVISIONING` with nothing owed at all: the placement itself is outstanding
/// ([`awaits_provisioned_capacity`]).
#[derive(Debug, Clone, Copy)]
pub struct PlacementSchedule {
    /// The `lastStatus` the task — and every container of it — is placed in.
    pub last_status: &'static str,
    /// When `PROVISIONING` -> `PENDING` falls due, for a task that is placed but whose
    /// networking is not in place yet.
    pub pending_due_at: Option<SystemTime>,
    /// When `PENDING` -> `RUNNING` falls due, for a task that is already `PENDING`.
    pub running_due_at: Option<SystemTime>,
}

/// The schedule a placement made at `now` leaves on the task, given whether its capacity
/// still has to be acquired and the attachments it was allocated. See
/// [`PlacementSchedule`].
pub fn placement_schedule(
    awaiting_capacity: bool,
    attachments: &[AttachmentRecord],
    now: SystemTime,
) -> PlacementSchedule {
    if awaiting_capacity {
        return PlacementSchedule {
            last_status: initial_task_status(true),
            pending_due_at: None,
            running_due_at: None,
        };
    }
    match attachments
        .iter()
        .filter_map(AttachmentRecord::attached_due_at)
        .max()
    {
        Some(attached_at) => PlacementSchedule {
            last_status: STATUS_PROVISIONING,
            pending_due_at: Some(attached_at),
            running_due_at: None,
        },
        None => PlacementSchedule {
            last_status: initial_task_status(false),
            pending_due_at: None,
            running_due_at: Some(now + TRANSITION_SPAN),
        },
    }
}

/// The `Attachment$details` key the interface's own id is reported under.
pub const ATTACHMENT_DETAIL_NETWORK_INTERFACE_ID: &str = "networkInterfaceId";

/// The `Attachment$details` key the interface's MAC address is reported under.
pub const ATTACHMENT_DETAIL_MAC_ADDRESS: &str = "macAddress";

/// The `Attachment$details` key the interface's private IPv4 address is reported under.
/// Amazon ECS spells this key with the `IP` of `IPv4` capitalised, unlike
/// `NetworkInterface$privateIpv4Address` on the container's own view.
pub const ATTACHMENT_DETAIL_PRIVATE_IPV4_ADDRESS: &str = "privateIPv4Address";

// The interface's own id, MAC address and private IPv4 address are AMAZON VPC's to mint:
// the interface is a resource of that service, created in the subnet the launch named
// ([`super::awsvpc_interfaces`]), so what this attachment reports for it is read off that
// record rather than derived here.

impl AttachmentRecord {
    /// Report, on this attachment, the three details that belong to the interface ITSELF:
    /// its id, its MAC address and its private IPv4 address.
    ///
    /// They are reported once the interface is IN PLACE — the instant the attachment
    /// reports [`ATTACHMENT_STATUS_ATTACHED`], whoever took it there ([`Self::advance_interface`],
    /// an agent's own acknowledgement) — and not while Amazon ECS is still creating and
    /// attaching it: a client that watches the attachment to learn when the task's
    /// networking is usable would otherwise be given an interface id, a MAC address and an
    /// address for an interface that is not attached to anything yet. The subnet the launch
    /// supplied is reported from the start, because the launch supplied it
    /// ([`ATTACHMENT_DETAIL_SUBNET_ID`]).
    ///
    /// The three values are the AMAZON VPC record's, read off the interface this task was
    /// allocated ([`super::awsvpc_interfaces`]) rather than derived here: the id the
    /// attachment names is an interface Amazon EC2's own `DescribeNetworkInterfaces`
    /// returns, and the address it reports is the one that interface holds out of its
    /// subnet. An attachment whose interface has not been created yet reports NONE of them
    /// — the allocation pass reports them when it records the interface
    /// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::record_allocated_interface`])
    /// — because an id for an interface that does not exist is one no client could resolve.
    ///
    /// Each is written once — a detail already reported is left as it is — and nothing but
    /// an elastic network interface gets any of them.
    ///
    /// Answers whether anything changed.
    pub fn report_allocated_interface(&mut self) -> bool {
        if self.attachment_type != ATTACHMENT_TYPE_ELASTIC_NETWORK_INTERFACE {
            return false;
        }
        let Some(allocated) = self.allocated.clone() else {
            return false;
        };
        let mut changed = false;
        for (name, value) in [
            (
                ATTACHMENT_DETAIL_NETWORK_INTERFACE_ID,
                allocated.network_interface_id,
            ),
            (ATTACHMENT_DETAIL_MAC_ADDRESS, allocated.mac_address),
            (
                ATTACHMENT_DETAIL_PRIVATE_IPV4_ADDRESS,
                allocated.private_ipv4_address,
            ),
        ] {
            if self
                .details
                .iter()
                .any(|detail| detail.name.as_deref() == Some(name))
            {
                continue;
            }
            self.details.push(KeyValuePair {
                name: Some(name.to_string()),
                value: Some(value),
            });
            changed = true;
        }
        changed
    }

    /// Report the interface Amazon ECS allocated for this attachment as in place: the end
    /// of [`INTERFACE_ATTACH_LIFECYCLE`], which the task reaching `RUNNING` means has
    /// already happened.
    ///
    /// Only an attachment the clock is still taking through that progression has its status
    /// moved: once an agent has spoken for it with a status off the progression
    /// (`SubmitAttachmentStateChanges`: "Sent to acknowledge that an attachment changed
    /// states") the agent's word is the one the record holds. The interface's own details
    /// are reported either way ([`Self::report_allocated_interface`]), because they are the
    /// interface's and not the attachment's.
    ///
    /// Answers whether anything changed, which is what makes the task's `version` counter
    /// move ("Every time a task experiences a change ... the version counter is
    /// incremented").
    pub fn attach_allocated_interface(&mut self) -> bool {
        if !self.is_elastic_network_interface() {
            return false;
        }
        let mut changed = self.report_allocated_interface();
        if INTERFACE_ATTACH_LIFECYCLE
            .iter()
            .any(|status| *status == self.status)
            && self.status != ATTACHMENT_STATUS_ATTACHED
        {
            self.status = ATTACHMENT_STATUS_ATTACHED.to_string();
            self.next_status_due_at = None;
            changed = true;
        }
        changed
    }

    /// Whether this attachment is the elastic network interface an `awsvpc` task is
    /// allocated, rather than some other kind of attachment a task can carry.
    pub fn is_elastic_network_interface(&self) -> bool {
        self.attachment_type == ATTACHMENT_TYPE_ELASTIC_NETWORK_INTERFACE
    }

    /// Report the interface Amazon ECS allocated for this attachment as GONE, without
    /// passing through the release progression — what a task that stopped before Amazon ECS
    /// had ATTACHED the interface leaves behind.
    ///
    /// The interface an `awsvpc` task is allocated belongs to that task — Amazon ECS creates
    /// it while the task provisions and tears it down when the task stops — so a stopped
    /// task's attachment stands for a resource that no longer exists, and
    /// [`ATTACHMENT_STATUS_DELETED`] is what the member's own list of statuses words that
    /// with. An interface that was never in place has nothing to be detached FROM, which is
    /// why this is the whole of its release: one that WAS in place is released a documented
    /// step at a time instead ([`Self::begin_release`]). It reports none of the three
    /// details that belong to the interface, for the reason it reported none while it was
    /// being attached ([`Self::report_allocated_interface`]).
    ///
    /// Answers whether anything changed, which is what makes the task's `version` counter
    /// move ("Every time a task experiences a change ... the version counter is
    /// incremented").
    pub fn delete_allocated_interface(&mut self) -> bool {
        if !self.is_elastic_network_interface() || self.status == ATTACHMENT_STATUS_DELETED {
            return false;
        }
        self.status = ATTACHMENT_STATUS_DELETED.to_string();
        self.next_status_due_at = None;
        true
    }

    /// Begin releasing the interface Amazon ECS allocated for this attachment, for a task
    /// that began stopping at `stop_began_at`: schedule the first step of the release, or
    /// report an interface that was never in place as simply gone.
    ///
    /// Amazon ECS tears the interface of a stopping task down, and that teardown is a
    /// PROGRESSION of the attachment's own ([`INTERFACE_DETACH_LIFECYCLE`]): the interface
    /// that was in place is detached from the task and then deleted, so a client watching the
    /// task stop sees the attachment leave [`ATTACHMENT_STATUS_ATTACHED`] and reach
    /// [`ATTACHMENT_STATUS_DELETED`] instead of seeing one status stand for both. Each step
    /// takes [`INTERFACE_RELEASE_STEP_SPAN`] and is owed from the instant the task began
    /// stopping, whether or not anybody calls to look ([`Self::advance_release`]), so the
    /// statuses in between are ones a caller can really observe — and the task is not STOPPED
    /// before the last of them ([`TaskRecord::interfaces_release_due_at`]), so a client
    /// reading a stopped task finds the interface it held already gone. The interface that WAS
    /// in place keeps the details it reported throughout: they are what a client reading a
    /// stopped task resolves the interface it HELD by.
    ///
    /// An attachment Amazon ECS had not attached yet has nothing to detach from, so it is
    /// simply gone ([`Self::delete_allocated_interface`]) and holds the stop open for
    /// nothing. The Amazon VPC record of a deleted interface is torn down by the pass that
    /// owns it
    /// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::interfaces_awaiting_release`]).
    ///
    /// The schedule is computed from the STORED instant the stop began, so it is the same
    /// whenever a pass gets to it, and re-stating it is what makes this safe to call on every
    /// pass. Answers whether anything a caller reads changed, which is what makes the task's
    /// `version` counter move ("Every time a task experiences a change ... the version
    /// counter is incremented").
    pub fn begin_release(&mut self, stop_began_at: SystemTime) -> bool {
        if !self.is_elastic_network_interface() || self.status == ATTACHMENT_STATUS_DELETED {
            return false;
        }
        if !self.is_attached() && !self.is_detaching() {
            return self.delete_allocated_interface();
        }
        // The first step is owed one span from the instant the stop began, so the attachment
        // an in-place interface stands for is still ATTACHED to a caller reading straight
        // after the stop was asked for. An attachment the release has already moved — or one
        // an agent's own report left at a release status — is re-armed only when nothing is
        // owed on it, so a progression under way keeps the schedule it is on and one that
        // lost its due instant is picked up again rather than stalling.
        if self.is_attached() || self.next_status_due_at.is_none() {
            self.next_status_due_at = Some(stop_began_at + INTERFACE_RELEASE_STEP_SPAN);
        }
        false
    }

    /// Whether the clock is in the middle of releasing this attachment's resource: it has
    /// left [`ATTACHMENT_STATUS_ATTACHED`] but has not reached
    /// [`ATTACHMENT_STATUS_DELETED`].
    pub fn is_detaching(&self) -> bool {
        self.status == ATTACHMENT_STATUS_DETACHING || self.status == ATTACHMENT_STATUS_DETACHED
    }

    /// The instant this attachment is expected to report [`ATTACHMENT_STATUS_DELETED`] at,
    /// from where its release has got to, and `None` once nothing more is owed — what the
    /// stop of the task holding it waits for ([`TaskRecord::interfaces_release_due_at`]).
    ///
    /// Only an elastic network interface has a release of its own here, so nothing else a task
    /// carries holds its stop open ([`super::managed_ebs_volumes`]).
    pub fn release_due_at(&self) -> Option<SystemTime> {
        if !self.is_elastic_network_interface() {
            return None;
        }
        let due = self.next_status_due_at?;
        let reached = INTERFACE_DETACH_LIFECYCLE
            .iter()
            .position(|status| *status == self.status)?;
        // The next step falls due at `due`, and every step after it one span further on.
        let steps_left = INTERFACE_DETACH_LIFECYCLE.len() - 1 - reached;
        let after_next = steps_left.checked_sub(1)?;
        Some(due + INTERFACE_RELEASE_STEP_SPAN * after_next as u32)
    }

    /// The status [`INTERFACE_DETACH_LIFECYCLE`] takes this attachment to next, or `None`
    /// when the clock owes it nothing more: the resource is gone, or the attachment is at a
    /// status that progression does not pass through at all.
    fn next_release_status(&self) -> Option<&'static str> {
        let reached = INTERFACE_DETACH_LIFECYCLE
            .iter()
            .position(|status| *status == self.status)?;
        INTERFACE_DETACH_LIFECYCLE.get(reached + 1).copied()
    }

    /// Materialise every step of this attachment's release that the clock has already made
    /// due as of `now`, and answer whether any of them moved the status — the progression
    /// [`Self::begin_release`] put on the record, and nothing else: an attachment of another
    /// kind, or one still being attached, is left where it is.
    pub fn advance_release(&mut self, now: SystemTime) -> bool {
        if !self.is_elastic_network_interface() {
            return false;
        }
        let mut changed = false;
        while let Some(due) = self.next_status_due_at.filter(|due| *due <= now) {
            match self.next_release_status() {
                Some(next) => {
                    self.status = next.to_string();
                    changed = true;
                    self.next_status_due_at = self
                        .next_release_status()
                        .map(|_| due + INTERFACE_RELEASE_STEP_SPAN);
                }
                // Nothing further is owed: the release is over.
                None => self.next_status_due_at = None,
            }
        }
        changed
    }

    /// Whether the resource this attachment stands for is in place.
    pub fn is_attached(&self) -> bool {
        self.status == ATTACHMENT_STATUS_ATTACHED
    }

    /// The status [`INTERFACE_ATTACH_LIFECYCLE`] takes this attachment to next, or `None`
    /// when the clock owes it nothing more: it is already attached, or an agent has
    /// reported a status that is not on that progression at all.
    fn next_attach_status(&self) -> Option<&'static str> {
        let reached = INTERFACE_ATTACH_LIFECYCLE
            .iter()
            .position(|status| *status == self.status)?;
        INTERFACE_ATTACH_LIFECYCLE.get(reached + 1).copied()
    }

    /// The instant this attachment is expected to report [`ATTACHMENT_STATUS_ATTACHED`],
    /// from where its progression has got to, and `None` once nothing more is owed — the
    /// instant a task that cannot be launched before its interface is in place is placed
    /// at ([`placement_schedule`]).
    pub fn attached_due_at(&self) -> Option<SystemTime> {
        let due = self.next_status_due_at?;
        let reached = INTERFACE_ATTACH_LIFECYCLE
            .iter()
            .position(|status| *status == self.status)?;
        // The next step falls due at `due`, and every step after it one span further on.
        let steps_left = INTERFACE_ATTACH_LIFECYCLE.len() - 1 - reached;
        let after_next = steps_left.checked_sub(1)?;
        Some(due + INTERFACE_STEP_SPAN * after_next as u32)
    }

    /// Materialise every step of this attachment's own progression that the clock has
    /// already made due as of `now`, and answer the instant of the last one it took (or
    /// `None` when nothing was due).
    ///
    /// The progression continues from whatever status the record HOLDS, so an agent that
    /// acknowledged a status takes the attachment to the step it reported and Amazon ECS
    /// carries on from there; a status that is not on the progression at all ends it.
    pub fn advance_interface(&mut self, now: SystemTime) -> Option<SystemTime> {
        let mut last_step = None;
        while let Some(due) = self.next_status_due_at.filter(|due| *due <= now) {
            match self.next_attach_status() {
                Some(next) => {
                    self.status = next.to_string();
                    last_step = Some(due);
                    self.next_status_due_at = self
                        .next_attach_status()
                        .map(|_| due + INTERFACE_STEP_SPAN);
                    // The interface is in place from this step on, so its own identity is
                    // what the attachment reports beside the subnet it sits in
                    // ([`Self::report_allocated_interface`]).
                    if self.status == ATTACHMENT_STATUS_ATTACHED {
                        self.report_allocated_interface();
                    }
                }
                // Nothing further is owed: the progression is over, whoever ended it.
                None => self.next_status_due_at = None,
            }
        }
        last_step
    }

    /// Start this attachment's progression over from `now`, for a task Amazon ECS is only
    /// now placing: nothing was attaching an interface for a task that had nowhere to run,
    /// so the first step is owed one span from the placement rather than from the instant
    /// the task was accepted. An attachment that is already in place, or whose progression
    /// an agent has ended, is left alone.
    pub fn begin_attaching(&mut self, now: SystemTime) {
        if self.next_status_due_at.is_some() {
            self.next_status_due_at = Some(now + INTERFACE_STEP_SPAN);
        }
    }

    /// The private IPv4 address this attachment reports, or `None` while the interface has
    /// not been attached yet — read off the stored details rather than recomputed, so it is
    /// the value a caller of `DescribeTasks` sees.
    pub fn private_ipv4_address(&self) -> Option<&str> {
        self.detail(ATTACHMENT_DETAIL_PRIVATE_IPV4_ADDRESS)
    }

    /// The subnet this attachment's interface was placed in, read off the stored details
    /// the same way — the subnet the launch supplied
    /// (`AwsVpcConfiguration$subnets`), and `None` for an attachment carrying none.
    pub fn subnet_id(&self) -> Option<&str> {
        self.detail(ATTACHMENT_DETAIL_SUBNET_ID)
    }

    /// The value the `Attachment$details` entry named `name` carries, and `None` when this
    /// attachment carries no such entry.
    fn detail(&self, name: &str) -> Option<&str> {
        self.details
            .iter()
            .find(|detail| detail.name.as_deref() == Some(name))
            .and_then(|detail| detail.value.as_deref())
    }

    /// The `NetworkInterface` a container of the task holding this attachment reports for
    /// it, or `None` when this attachment is not an interface, or when the interface it
    /// stands for is not in place yet.
    ///
    /// `Container$networkInterfaces` is "the network interfaces associated with the
    /// container": a container of an `awsvpc` task is reached on the one interface its task
    /// was allocated, so it reports that interface once the interface EXISTS — which is what
    /// its own identity being reported means ([`Self::report_allocated_interface`]) — and
    /// reports none while Amazon ECS is still attaching it.
    pub fn container_network_interface(&self) -> Option<NetworkInterface> {
        if self.attachment_type != ATTACHMENT_TYPE_ELASTIC_NETWORK_INTERFACE {
            return None;
        }
        Some(NetworkInterface {
            attachment_id: Some(self.id.clone()),
            private_ipv4_address: Some(self.private_ipv4_address()?.to_string()),
            // No IPv6 address: nothing here allocates one, and reporting an address the
            // interface does not hold would be reporting a reachable address that is not.
            ipv6_address: None,
        })
    }
}

impl TaskRecord {
    /// The instant the release of the interfaces this task holds is expected to finish, and
    /// `None` once none of them is owed another step.
    ///
    /// What the task's own `STOPPED` waits for: Amazon ECS detaches and deletes the interface
    /// an `awsvpc` task was allocated as part of stopping that task
    /// ([`AttachmentRecord::begin_release`]), so a task that still holds one has that step
    /// outstanding and is not stopped yet — the mirror of the attach, which a task's
    /// `PENDING` waits for ([`Self::interfaces_attached`]).
    pub fn interfaces_release_due_at(&self) -> Option<SystemTime> {
        self.attachments
            .iter()
            .filter_map(AttachmentRecord::release_due_at)
            .max()
    }
}
