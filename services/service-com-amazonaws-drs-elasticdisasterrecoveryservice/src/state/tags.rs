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
//! An ARN that resolves to no resource is refused here too ([`not_found`]), once for the
//! whole route: it is served ahead of any one resource's operations, so what it answers
//! is its own not-found class rather than the `ResourceNotFoundException` an id-keyed
//! operation of a resource raises.

use std::collections::HashMap;

use super::{
    launch_configuration_templates, recovery_plans,
    ComAmazonawsDrsElasticdisasterrecoveryserviceState,
};

/// The resource part of a replication configuration template's ARN, as that resource's
/// own ARN template spells it (the model's `ReplicationConfigurationTemplateResource`:
/// `replication-configuration-template/{replicationConfigurationTemplateID}`).
pub const REPLICATION_CONFIGURATION_TEMPLATE_ARN_PREFIX: &str =
    "replication-configuration-template/";

/// The resource part of a source server's ARN (the model's `SourceServerResource`:
/// `source-server/{sourceServerID}`).
pub const SOURCE_SERVER_ARN_PREFIX: &str = "source-server/";

/// What a `resourceArn` the tagging route was handed addresses: one kind of resource of
/// this service, carrying the id the store of that kind is keyed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedResource<'a> {
    /// `recovery-plan/{recoveryPlanID}`.
    RecoveryPlan(&'a str),
    /// `replication-configuration-template/{replicationConfigurationTemplateID}`.
    ReplicationConfigurationTemplate(&'a str),
    /// `launch-configuration-template/{launchConfigurationTemplateID}`.
    LaunchConfigurationTemplate(&'a str),
    /// `source-server/{sourceServerID}`.
    SourceServer(&'a str),
}

impl<'a> TaggedResource<'a> {
    /// What `arn` addresses, or `None` for an ARN that names no resource kind this
    /// service holds: another service's resource, or a kind of this service's that no
    /// operation of this emulator mints yet.
    pub fn in_arn(arn: &'a str) -> Option<Self> {
        // An ARN is `arn:<partition>:<service>:<region>:<account>:<resource>`, so its
        // resource part is everything after the fifth colon — and the resource part is
        // what the resource's ARN template describes.
        let resource = arn.splitn(6, ':').nth(5)?;
        if let Some(id) = id_under(resource, recovery_plans::ARN_RESOURCE_PREFIX) {
            return Some(Self::RecoveryPlan(id));
        }
        if let Some(id) = id_under(resource, REPLICATION_CONFIGURATION_TEMPLATE_ARN_PREFIX) {
            return Some(Self::ReplicationConfigurationTemplate(id));
        }
        if let Some(id) = id_under(resource, launch_configuration_templates::ARN_RESOURCE_PREFIX) {
            return Some(Self::LaunchConfigurationTemplate(id));
        }
        id_under(resource, SOURCE_SERVER_ARN_PREFIX).map(Self::SourceServer)
    }
}

/// The id `resource` names under `prefix`, or `None` when it names another kind of
/// resource or carries no id at all.
fn id_under<'a>(resource: &'a str, prefix: &str) -> Option<&'a str> {
    resource
        .strip_prefix(prefix)
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

impl ComAmazonawsDrsElasticdisasterrecoveryserviceState {
    /// The tags of the resource `resource_arn` addresses, or `None` when this service
    /// holds no such resource.
    ///
    /// The ONE lookup the tagging route performs, so every tagging operation resolves an
    /// ARN the same way and reads the tag set the resource's own operations report.
    pub fn resource_tags(&self, resource_arn: &str) -> Option<&HashMap<String, String>> {
        match TaggedResource::in_arn(resource_arn)? {
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
            TaggedResource::LaunchConfigurationTemplate(id) => self
                .launch_configuration_templates
                .get(id)
                .filter(|template| template.arn == resource_arn)
                .map(|template| &template.tags),
            TaggedResource::SourceServer(id) => self
                .source_servers
                .get(id)
                .filter(|server| server.arn == resource_arn)
                .map(|server| &server.tags),
        }
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
    ) -> Option<&mut HashMap<String, String>> {
        match TaggedResource::in_arn(resource_arn)? {
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
            TaggedResource::LaunchConfigurationTemplate(id) => self
                .launch_configuration_templates
                .get_mut(id)
                .filter(|template| template.arn == resource_arn)
                .map(|template| &mut template.tags),
            TaggedResource::SourceServer(id) => self
                .source_servers
                .get_mut(id)
                .filter(|server| server.arn == resource_arn)
                .map(|server| &mut server.tags),
        }
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

/// How many tags one resource carries before the tagging route refuses to add another:
/// "Each resource can have a maximum of 50 tags" (`TagResource`'s documentation, see
/// `smithy_query(query="doc",shape="TagResource")`).
///
/// It is also what stops a caller growing one resource's tag set without end.
pub const TAGS_PER_RESOURCE_LIMIT: usize = 50;
