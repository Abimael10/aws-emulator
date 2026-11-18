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

use std::collections::HashMap;

use crate::ops::types::ResourceNotFoundException;

use super::{recovery_plans, ComAmazonawsDrsElasticdisasterrecoveryserviceState};

/// The resource part of a replication configuration template's ARN, as that resource's
/// own ARN template spells it (the model's `ReplicationConfigurationTemplateResource`:
/// `replication-configuration-template/{replicationConfigurationTemplateID}`).
pub const REPLICATION_CONFIGURATION_TEMPLATE_ARN_PREFIX: &str =
    "replication-configuration-template/";

/// The resource part of a source server's ARN (the model's `SourceServerResource`:
/// `source-server/{sourceServerID}`).
pub const SOURCE_SERVER_ARN_PREFIX: &str = "source-server/";

/// How this service names the source-server resource when it reports one in an error.
pub const SOURCE_SERVER_RESOURCE_TYPE: &str = "Source Server";

/// How a refusal names a resource whose KIND the ARN does not say: a `resourceArn` the
/// tagging route resolved to nothing and whose resource part names none of this
/// service's resource templates.
pub const ANY_RESOURCE_TYPE: &str = "Resource";

/// What a `resourceArn` the tagging route was handed addresses: one kind of resource of
/// this service, carrying the id the store of that kind is keyed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedResource<'a> {
    /// `recovery-plan/{recoveryPlanID}`.
    RecoveryPlan(&'a str),
    /// `replication-configuration-template/{replicationConfigurationTemplateID}`.
    ReplicationConfigurationTemplate(&'a str),
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
        id_under(resource, SOURCE_SERVER_ARN_PREFIX).map(Self::SourceServer)
    }

    /// How this service names this kind of resource when it reports one in an error:
    /// the `resourceType` of a refusal about it.
    pub fn resource_type(self) -> &'static str {
        match self {
            Self::RecoveryPlan(_) => recovery_plans::RESOURCE_TYPE,
            Self::ReplicationConfigurationTemplate(_) => {
                super::REPLICATION_CONFIGURATION_TEMPLATE_RESOURCE_TYPE
            }
            Self::SourceServer(_) => SOURCE_SERVER_RESOURCE_TYPE,
        }
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
            TaggedResource::SourceServer(id) => self
                .source_servers
                .get(id)
                .filter(|server| server.arn == resource_arn)
                .map(|server| &server.tags),
        }
    }
}

/// The modelled `ResourceNotFoundException` (HTTP 404) for a `resourceArn` the tagging
/// route resolved to no resource of this service.
///
/// The ONE place that refusal is built, so the read and the writes of the tagging route
/// report the same thing. The route is handed an ARN and nothing else, so the refusal
/// reports that ARN verbatim as its `resourceId` and names the kind of resource the ARN
/// addresses — [`ANY_RESOURCE_TYPE`] when the ARN names no kind this service holds — in
/// the sentence form this service's other not-found refusals carry: what was asked for,
/// then that it was not found.
pub fn not_found(resource_arn: &str) -> ResourceNotFoundException {
    let resource_type = TaggedResource::in_arn(resource_arn).map(TaggedResource::resource_type);
    ResourceNotFoundException {
        message: Some(format!(
            "{} with Arn: '{resource_arn}' was not found",
            resource_type.unwrap_or(ANY_RESOURCE_TYPE)
        )),
        code: None,
        resource_id: Some(resource_arn.to_string()),
        resource_type: resource_type.map(std::string::ToString::to_string),
    }
}
