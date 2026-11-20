// SPDX-License-Identifier: CC0-1.0

//! The resource the tagging route addresses, and the tag set that resource carries.
//!
//! The tagging operations share ONE route (`/tags/{resourceArn}`, by method) and address
//! a resource by its ARN ALONE: nothing else in such a request says which kind of
//! resource it is about. So the resolution they all perform lives here, once — read the
//! kind off the ARN's resource part, look the resource up in the store that holds that
//! kind, and answer the tag set it carries — and a tag read cannot disagree with a tag
//! write about which resource an ARN names, or with the resource's own operations about
//! whether it exists.
//!
//! A resource of this service is addressed by an ARN in full: the stored resource's own
//! ARN has to equal the one the request named, so an id of a stored resource under
//! another partition, Region or account addresses nothing here — the same reading the
//! id-keyed operations perform (`crate::ops::get_recovery_plan`).
//!
//! What the route can do with an ARN it cannot serve is decided here too, once for the
//! whole route — and it is TWO answers rather than one ([`TagArnRefusal`]):
//!
//! * an ARN whose resource part names no resource type this route ADDRESSES is a fault in
//!   the value the request supplied, refused as the route's own `BadRequestException`
//!   ([`invalid_resource_arn`]) before any resource is looked up, because there is nothing
//!   such an ARN could be asking for;
//! * an ARN of a type the route does address, naming no resource this service holds, is
//!   the route's own not-found ([`not_found`]) — its own error class rather than the
//!   `ResourceNotFoundException` an id-keyed operation of a resource raises.
//!
//! Both are the LAYER's classes rather than the service's, which is what a client of this
//! route branches on.

use std::collections::HashMap;

use super::{recovery_plans, ComAmazonawsDrsElasticdisasterrecoveryserviceState};

/// The resource part of a replication configuration template's ARN, as that resource's
/// own ARN template spells it (the model's `ReplicationConfigurationTemplateResource`:
/// `replication-configuration-template/{replicationConfigurationTemplateID}`).
pub const REPLICATION_CONFIGURATION_TEMPLATE_ARN_PREFIX: &str =
    "replication-configuration-template/";

/// The resource part of a source server's ARN (the model's `SourceServerResource`:
/// `source-server/{sourceServerID}`).
pub const SOURCE_SERVER_ARN_PREFIX: &str = "source-server/";

/// The resource part of a job's ARN (the model's `JobResource`: `job/{jobID}`).
pub const JOB_ARN_PREFIX: &str = "job/";

/// The resource part of a recovery instance's ARN (the model's
/// `RecoveryInstanceResource`: `recovery-instance/{recoveryInstanceID}`).
pub const RECOVERY_INSTANCE_ARN_PREFIX: &str = "recovery-instance/";

/// The resource part of a source network's ARN (the model's `SourceNetworkResource`:
/// `source-network/{sourceNetworkID}`).
pub const SOURCE_NETWORK_ARN_PREFIX: &str = "source-network/";

/// What a `resourceArn` the tagging route was handed addresses: one of the resource types
/// the route addresses, carrying the id the store of that type is keyed by.
///
/// Every variant is a resource type the tagging route can be asked about, and its name
/// comes from that resource's own ARN template in the model
/// (`smithy_query(query="shape",shape="JobResource,RecoveryInstanceResource,\
/// RecoveryPlanResource,ReplicationConfigurationTemplateResource,SourceNetworkResource,\
/// SourceServerResource")`, each resource's `arn.template`). The ONE resource type of this
/// service that is NOT here is the launch configuration template: it is this service's
/// account-level configuration resource ("Account level Launch Configuration Template",
/// the model's own documentation of the `LaunchConfigurationTemplate` shape) whose tags are
/// set by the create that mints it and by nothing else — `CreateLaunchConfigurationTemplate`
/// takes `tags`, `UpdateLaunchConfigurationTemplate` does not — so the tagging route does
/// not address it and an ARN naming one is refused as a value it cannot act on (see
/// [`TagArnRefusal::NotAddressable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedResource<'a> {
    /// `recovery-plan/{recoveryPlanID}`.
    RecoveryPlan(&'a str),
    /// `replication-configuration-template/{replicationConfigurationTemplateID}`.
    ReplicationConfigurationTemplate(&'a str),
    /// `source-server/{sourceServerID}`.
    SourceServer(&'a str),
    /// `job/{jobID}`.
    Job(&'a str),
    /// `recovery-instance/{recoveryInstanceID}`.
    RecoveryInstance(&'a str),
    /// `source-network/{sourceNetworkID}`.
    SourceNetwork(&'a str),
}

impl<'a> TaggedResource<'a> {
    /// What `arn` addresses, or `None` for an ARN the tagging route does not address at
    /// all: another service's resource, a resource type of this service the route does not
    /// address, or a resource part that is not `<type>/<id>`.
    pub fn in_arn(arn: &'a str) -> Option<Self> {
        // An ARN is `arn:<partition>:<service>:<region>:<account>:<resource>`. The route
        // addresses resources OF THIS SERVICE, so the service segment has to be this
        // service's ARN namespace (`crate::state::SERVICE_CODE`), and the resource part —
        // everything after the fifth colon — is what a resource's ARN template describes.
        // The partition, Region and account are not read here: a stored resource's own ARN
        // carries them, so an ARN naming another one is answered by the resource the
        // lookups below do not hold.
        let mut segments = arn.splitn(6, ':');
        if segments.next()? != "arn" {
            return None;
        }
        let _partition = segments.next()?;
        if segments.next()? != super::SERVICE_CODE {
            return None;
        }
        let _region = segments.next()?;
        let _account = segments.next()?;
        let resource = segments.next()?;
        if let Some(id) = id_under(resource, recovery_plans::ARN_RESOURCE_PREFIX) {
            return Some(Self::RecoveryPlan(id));
        }
        if let Some(id) = id_under(resource, REPLICATION_CONFIGURATION_TEMPLATE_ARN_PREFIX) {
            return Some(Self::ReplicationConfigurationTemplate(id));
        }
        if let Some(id) = id_under(resource, SOURCE_SERVER_ARN_PREFIX) {
            return Some(Self::SourceServer(id));
        }
        if let Some(id) = id_under(resource, JOB_ARN_PREFIX) {
            return Some(Self::Job(id));
        }
        if let Some(id) = id_under(resource, RECOVERY_INSTANCE_ARN_PREFIX) {
            return Some(Self::RecoveryInstance(id));
        }
        id_under(resource, SOURCE_NETWORK_ARN_PREFIX).map(Self::SourceNetwork)
    }
}

/// Why the tagging route cannot serve a `resourceArn` — the two answers the route decides
/// for itself, which every one of its operations answers alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagArnRefusal {
    /// The ARN's resource part names no resource type this route addresses (see
    /// [`TaggedResource`]), so there is no resource it could be asking about: a fault in
    /// the value itself, decided before any resource is looked up.
    NotAddressable,
    /// The ARN names a resource type the route addresses, but this service holds no such
    /// resource — an id nothing minted, or a stored resource's id under another partition,
    /// Region or account.
    NoSuchResource,
}

/// The id `resource` names under `prefix`, or `None` when it names another kind of
/// resource or carries no id at all.
fn id_under<'a>(resource: &'a str, prefix: &str) -> Option<&'a str> {
    resource
        .strip_prefix(prefix)
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

impl ComAmazonawsDrsElasticdisasterrecoveryserviceState {
    /// The tags of the resource `resource_arn` addresses, or why the route cannot serve
    /// that ARN.
    ///
    /// The ONE lookup the tagging route performs, so every tagging operation resolves an
    /// ARN the same way, reads the tag set the resource's own operations report, and
    /// refuses an ARN it cannot serve in the same class.
    pub fn resource_tags(
        &self,
        resource_arn: &str,
    ) -> Result<&HashMap<String, String>, TagArnRefusal> {
        let addressed =
            TaggedResource::in_arn(resource_arn).ok_or(TagArnRefusal::NotAddressable)?;
        let tags = match addressed {
            TaggedResource::RecoveryPlan(id) => self
                .recovery_plans
                .get(id)
                .filter(|plan| plan.arn == resource_arn)
                .map(|plan| &plan.tags),
            TaggedResource::ReplicationConfigurationTemplate(id) => self
                .replication_configuration_templates
                .get(id)
                .filter(|template| template.arn == resource_arn)
                .map(|template| &template.tags),
            TaggedResource::SourceServer(id) => self
                .source_servers
                .get(id)
                .filter(|server| server.arn == resource_arn)
                .map(|server| &server.tags),
            // No operation of this emulator registers a job, a recovery instance or a
            // source network yet, so there is no store to look one up in: an ARN naming
            // one addresses a type the route serves and a resource this service does not
            // hold, which is the same answer as an id of any other addressed type that
            // nothing minted.
            TaggedResource::Job(_)
            | TaggedResource::RecoveryInstance(_)
            | TaggedResource::SourceNetwork(_) => None,
        };
        tags.ok_or(TagArnRefusal::NoSuchResource)
    }

    /// The same tag set, to CHANGE: the set the tagging route's write operations add to
    /// and remove from, held by the resource itself.
    ///
    /// The write resolves an ARN through the same [`TaggedResource::in_arn`] reading as
    /// the read above and hands back the resource's OWN set, so a tag written through
    /// this route is the tag the resource's own reader reports — there is no second copy
    /// of a resource's tags anywhere.
    pub fn resource_tags_mut(
        &mut self,
        resource_arn: &str,
    ) -> Result<&mut HashMap<String, String>, TagArnRefusal> {
        let addressed =
            TaggedResource::in_arn(resource_arn).ok_or(TagArnRefusal::NotAddressable)?;
        let tags = match addressed {
            TaggedResource::RecoveryPlan(id) => self
                .recovery_plans
                .get_mut(id)
                .filter(|plan| plan.arn == resource_arn)
                .map(|plan| &mut plan.tags),
            TaggedResource::ReplicationConfigurationTemplate(id) => self
                .replication_configuration_templates
                .get_mut(id)
                .filter(|template| template.arn == resource_arn)
                .map(|template| &mut template.tags),
            TaggedResource::SourceServer(id) => self
                .source_servers
                .get_mut(id)
                .filter(|server| server.arn == resource_arn)
                .map(|server| &mut server.tags),
            // The same three types the read above holds no store for, answered the same
            // way: there is nothing to write to.
            TaggedResource::Job(_)
            | TaggedResource::RecoveryInstance(_)
            | TaggedResource::SourceNetwork(_) => None,
        };
        tags.ok_or(TagArnRefusal::NoSuchResource)
    }
}

/// The whole message every operation of the tagging route answers a `resourceArn` it
/// resolved to no resource with: fixed text, carrying nothing of the request — the route
/// is handed an ARN and nothing else, and it reports neither the ARN nor the kind of
/// resource it addresses.
pub const NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The refusal (HTTP 404, error code `NotFoundException`) for a `resourceArn` the
/// tagging route resolved to no resource of this service.
///
/// The ONE place that refusal is built, so the read and the writes of the route report
/// the same thing for the same missing resource. It is NOT the
/// `ResourceNotFoundException` the id-keyed operations of a resource raise: this route
/// is served ahead of any one resource's operations — it takes no resource type, and one
/// path serves every kind — and it states a resource it could not resolve as its own
/// error class, which is what a client of the tagging route matches on
/// (see [`crate::scaffolded::Scaffolded::not_found`]).
pub fn not_found<E>() -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::not_found(NOT_FOUND_MESSAGE)
}

/// The whole message every operation of the tagging route answers a `resourceArn` it does
/// not address with: fixed text, carrying nothing of the request — neither the ARN, nor
/// the resource type it named, nor the member it arrived on.
pub const INVALID_RESOURCE_ARN_MESSAGE: &str = "Invalid input resource arn";

/// The refusal (HTTP 400, error code `BadRequestException`) for a `resourceArn` whose
/// resource part names no resource type this route addresses.
///
/// It is the sibling of [`not_found`] and belongs to the same layer: the tagging route is
/// served ahead of any one resource's operations, so it states what it decided about the
/// one value it was handed in its OWN error classes — a value it cannot act on as a bad
/// request, a resource it could not resolve as not found — rather than as the
/// `ValidationException` and `ResourceNotFoundException` the service's own operations
/// raise about a request body and an id.
pub fn invalid_resource_arn<E>() -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::bad_request(INVALID_RESOURCE_ARN_MESSAGE)
}

/// The refusal one of this route's operations answers `refusal` with.
///
/// The ONE place the class of a refused `resourceArn` becomes a response, so the read, the
/// write and the removal of the route cannot answer the same ARN with different statuses.
pub fn refusal_error<E>(refusal: TagArnRefusal) -> crate::scaffolded::Scaffolded<E> {
    match refusal {
        TagArnRefusal::NotAddressable => invalid_resource_arn(),
        TagArnRefusal::NoSuchResource => not_found(),
    }
}

/// How many tags one resource carries before the tagging route refuses to add another:
/// "Each resource can have a maximum of 50 tags" (`TagResource`'s documentation, see
/// `smithy_query(query="doc",shape="TagResource")`).
///
/// It is also what stops a caller growing one resource's tag set without end.
pub const TAGS_PER_RESOURCE_LIMIT: usize = 50;
