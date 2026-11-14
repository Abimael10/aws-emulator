// SPDX-License-Identifier: CC0-1.0

//! The ACCESS CONTROL LISTS a cluster's brokers hold, and the three protocol APIs a
//! client manages them with.
//!
//! A SASL client of an Amazon MSK cluster is a PRINCIPAL, and what that principal may
//! do to a topic is an ACL binding the cluster's brokers keep — created with
//! `CreateAcls`, read with `DescribeAcls` and removed with `DeleteAcls`, each over the
//! same endpoint the records travel on. The bindings live in this service's state, so
//! what a client reads back is what it wrote, and a binding removed is gone for every
//! later reader.
//!
//! What the bindings DECIDE here is a DENY: an Amazon MSK cluster runs with Apache
//! Kafka's `allow.everyone.if.no.acl.found` turned on, so an authenticated principal
//! is served unless a binding withholds the topic from it — which is the one decision
//! this endpoint takes from them ([`Acls::denies`]).

use std::collections::BTreeMap;

use ::kafka_protocol::messages::{
    create_acls_request::AclCreation, create_acls_response::AclCreationResult,
    delete_acls_request::DeleteAclsFilter, delete_acls_response::DeleteAclsFilterResult,
    delete_acls_response::DeleteAclsMatchingAcl, describe_acls_response::AclDescription,
    describe_acls_response::DescribeAclsResource, CreateAclsRequest, CreateAclsResponse,
    DeleteAclsRequest, DeleteAclsResponse, DescribeAclsRequest, DescribeAclsResponse,
};
use ::kafka_protocol::protocol::StrBytes;

/// The `ResourceType` a binding on a TOPIC names (Apache Kafka's `ResourceType`
/// enum), and the `ANY` a filter uses to match every type.
pub const RESOURCE_TYPE_ANY: i8 = 1;
/// See [`RESOURCE_TYPE_ANY`].
pub const RESOURCE_TYPE_TOPIC: i8 = 2;

/// The `PatternType` a binding that names its resource exactly carries, and the `ANY`
/// a filter uses to match every pattern type (Apache Kafka's `PatternType` enum).
pub const PATTERN_TYPE_ANY: i8 = 1;
/// See [`PATTERN_TYPE_ANY`].
pub const PATTERN_TYPE_LITERAL: i8 = 3;

/// The `AclOperation` values this endpoint reads: `ANY` and `ALL` as a filter and a
/// grant over every operation, and the two a client of a topic needs.
pub const OPERATION_ANY: i8 = 1;
/// See [`OPERATION_ANY`].
pub const OPERATION_ALL: i8 = 2;

/// The `AclPermissionType` values: `ANY` as a filter, then the two a binding carries.
pub const PERMISSION_ANY: i8 = 1;
/// See [`PERMISSION_ANY`].
pub const PERMISSION_DENY: i8 = 2;

/// The host a binding that names no particular client carries.
pub const HOST_ANY: &str = "*";

/// The most bindings one cluster's brokers hold. CHOSEN as a process-safety bound, not
/// an Apache Kafka setting: the bindings come from clients, so without a ceiling one
/// client would size the store.
pub const MAX_ACLS_PER_CLUSTER: usize = 512;

/// The most bytes one binding's resource name, principal or host may carry. CHOSEN for
/// the same reason, sized above the longest Apache Kafka topic name (249 characters)
/// and the principal names a client really sends.
pub const MAX_ACL_FIELD_BYTES: usize = 256;

/// One ACL binding, as `CreateAcls` creates it and `DescribeAcls` reports it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AclBinding {
    /// Which KIND of resource the binding is about (`ResourceType`).
    pub resource_type: i8,
    /// The resource's name, or the prefix a `PREFIXED` binding names.
    pub resource_name: String,
    /// How the name is matched (`PatternType`).
    pub pattern_type: i8,
    /// The principal the binding is about, as Apache Kafka writes one
    /// (`User:<name>`).
    pub principal: String,
    /// The client host the binding covers, `*` for every host.
    pub host: String,
    /// The operation the binding covers (`AclOperation`).
    pub operation: i8,
    /// Whether the binding allows or denies it (`AclPermissionType`).
    pub permission_type: i8,
}

impl AclBinding {
    /// Whether this binding is one `filter` selects: a filter field set to its `ANY`
    /// value, or to no name at all, matches every binding.
    fn matched_by(&self, filter: &AclFilter) -> bool {
        (filter.resource_type == RESOURCE_TYPE_ANY || filter.resource_type == self.resource_type)
            && filter
                .resource_name
                .as_ref()
                .is_none_or(|name| *name == self.resource_name)
            && (filter.pattern_type == PATTERN_TYPE_ANY
                || filter.pattern_type == self.pattern_type)
            && filter
                .principal
                .as_ref()
                .is_none_or(|principal| *principal == self.principal)
            && filter.host.as_ref().is_none_or(|host| *host == self.host)
            && (filter.operation == OPERATION_ANY || filter.operation == self.operation)
            && (filter.permission_type == PERMISSION_ANY
                || filter.permission_type == self.permission_type)
    }

    /// The binding as `DescribeAcls` reports it within its resource.
    fn to_description(&self) -> AclDescription {
        AclDescription::default()
            .with_principal(StrBytes::from_string(self.principal.clone()))
            .with_host(StrBytes::from_string(self.host.clone()))
            .with_operation(self.operation)
            .with_permission_type(self.permission_type)
    }

    /// The binding as `DeleteAcls` reports the ones it removed.
    fn to_matching(&self) -> DeleteAclsMatchingAcl {
        DeleteAclsMatchingAcl::default()
            .with_resource_type(self.resource_type)
            .with_resource_name(StrBytes::from_string(self.resource_name.clone()))
            .with_pattern_type(self.pattern_type)
            .with_principal(StrBytes::from_string(self.principal.clone()))
            .with_host(StrBytes::from_string(self.host.clone()))
            .with_operation(self.operation)
            .with_permission_type(self.permission_type)
    }
}

/// A binding selection, as `DescribeAcls` and `DeleteAcls` state one.
struct AclFilter {
    resource_type: i8,
    resource_name: Option<String>,
    pattern_type: i8,
    principal: Option<String>,
    host: Option<String>,
    operation: i8,
    permission_type: i8,
}

/// The bindings one cluster's brokers hold.
#[derive(Debug, Default)]
pub struct Acls {
    /// The bindings, in creation order with duplicates folded: creating a binding that
    /// is already held changes nothing, as Apache Kafka's `CreateAcls` does.
    bindings: Vec<AclBinding>,
}

impl Acls {
    /// Store `binding`, answering whether the store had room for it.
    fn create(&mut self, binding: AclBinding) -> bool {
        if binding.resource_name.len() > MAX_ACL_FIELD_BYTES
            || binding.principal.len() > MAX_ACL_FIELD_BYTES
            || binding.host.len() > MAX_ACL_FIELD_BYTES
        {
            return false;
        }
        if self.bindings.contains(&binding) {
            return true;
        }
        if self.bindings.len() >= MAX_ACLS_PER_CLUSTER {
            return false;
        }
        self.bindings.push(binding);
        true
    }

    /// The bindings `filter` selects.
    fn matching(&self, filter: &AclFilter) -> Vec<AclBinding> {
        self.bindings
            .iter()
            .filter(|binding| binding.matched_by(filter))
            .cloned()
            .collect()
    }

    /// Remove the bindings `filter` selects, answering what was removed.
    fn remove(&mut self, filter: &AclFilter) -> Vec<AclBinding> {
        let removed = self.matching(filter);
        self.bindings.retain(|binding| !removed.contains(binding));
        removed
    }

    /// Whether a stored binding WITHHOLDS the topic `topic` from `principal`.
    ///
    /// An Amazon MSK cluster runs with Apache Kafka's
    /// `allow.everyone.if.no.acl.found` turned on, so an authenticated principal is
    /// served unless a `DENY` binding covers the topic and the operation.
    pub fn denies(&self, principal: Option<&str>, topic: &str, operation: i8) -> bool {
        let principal = format!("User:{}", principal.unwrap_or_default());
        self.bindings.iter().any(|binding| {
            binding.permission_type == PERMISSION_DENY
                && binding.resource_type == RESOURCE_TYPE_TOPIC
                && (binding.resource_name == topic
                    || (binding.pattern_type != PATTERN_TYPE_LITERAL
                        && topic.starts_with(&binding.resource_name)))
                && (binding.principal == "User:*" || binding.principal == principal)
                && (binding.operation == OPERATION_ALL
                    || binding.operation == OPERATION_ANY
                    || binding.operation == operation)
        })
    }
}

/// `CreateAcls`: store every binding the request creates, at most `max_creations` of
/// them — the list comes from the request, and each entry is a scan of what the cluster
/// already holds.
pub fn create(
    acls: &mut Acls,
    request: &CreateAclsRequest,
    max_creations: usize,
) -> CreateAclsResponse {
    let results: Vec<AclCreationResult> = request
        .creations
        .iter()
        .take(max_creations)
        .map(|creation| match acls.create(binding_of(creation)) {
            true => AclCreationResult::default(),
            false => AclCreationResult::default()
                .with_error_code(::kafka_protocol::ResponseError::PolicyViolation.code())
                .with_error_message(Some(StrBytes::from_static_str(
                    "This cluster will not hold that ACL binding.",
                ))),
        })
        .collect();
    CreateAclsResponse::default().with_results(results)
}

/// `DescribeAcls`: the stored bindings the request's filter selects, grouped by the
/// resource they are about — the shape this API reports them in.
pub fn describe(acls: &Acls, request: &DescribeAclsRequest) -> DescribeAclsResponse {
    let filter = AclFilter {
        resource_type: request.resource_type_filter,
        resource_name: request
            .resource_name_filter
            .as_ref()
            .map(ToString::to_string),
        pattern_type: request.pattern_type_filter,
        principal: request.principal_filter.as_ref().map(ToString::to_string),
        host: request.host_filter.as_ref().map(ToString::to_string),
        operation: request.operation,
        permission_type: request.permission_type,
    };
    let mut grouped: BTreeMap<(i8, String, i8), Vec<AclDescription>> = BTreeMap::new();
    for binding in acls.matching(&filter) {
        grouped
            .entry((
                binding.resource_type,
                binding.resource_name.clone(),
                binding.pattern_type,
            ))
            .or_default()
            .push(binding.to_description());
    }
    DescribeAclsResponse::default().with_resources(
        grouped
            .into_iter()
            .map(|((resource_type, resource_name, pattern_type), acls)| {
                DescribeAclsResource::default()
                    .with_resource_type(resource_type)
                    .with_resource_name(StrBytes::from_string(resource_name))
                    .with_pattern_type(pattern_type)
                    .with_acls(acls)
            })
            .collect(),
    )
}

/// `DeleteAcls`: remove the bindings each filter selects, reporting what each one
/// removed.
///
/// At most `max_filters` of them are served: a filter is a scan of every binding the
/// cluster holds, so the work is the product of the filter count and that ceiling, and
/// the filter count comes from the request.
pub fn delete(
    acls: &mut Acls,
    request: &DeleteAclsRequest,
    max_filters: usize,
) -> DeleteAclsResponse {
    let results: Vec<DeleteAclsFilterResult> = request
        .filters
        .iter()
        .take(max_filters)
        .map(|filter| {
            let removed = acls.remove(&filter_of(filter));
            DeleteAclsFilterResult::default().with_matching_acls(
                removed.iter().map(AclBinding::to_matching).collect(),
            )
        })
        .collect();
    DeleteAclsResponse::default().with_filter_results(results)
}

/// The binding one `CreateAcls` creation states.
fn binding_of(creation: &AclCreation) -> AclBinding {
    AclBinding {
        resource_type: creation.resource_type,
        resource_name: creation.resource_name.to_string(),
        pattern_type: creation.resource_pattern_type,
        principal: creation.principal.to_string(),
        host: creation.host.to_string(),
        operation: creation.operation,
        permission_type: creation.permission_type,
    }
}

/// The selection one `DeleteAcls` filter states.
fn filter_of(filter: &DeleteAclsFilter) -> AclFilter {
    AclFilter {
        resource_type: filter.resource_type_filter,
        resource_name: filter
            .resource_name_filter
            .as_ref()
            .map(ToString::to_string),
        pattern_type: filter.pattern_type_filter,
        principal: filter.principal_filter.as_ref().map(ToString::to_string),
        host: filter.host_filter.as_ref().map(ToString::to_string),
        operation: filter.operation,
        permission_type: filter.permission_type,
    }
}
