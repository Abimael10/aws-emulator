// SPDX-License-Identifier: CC0-1.0

//! The one shape every "that resource is not here" refusal in this crate takes.
//!
//! `ResourceNotFoundException` carries three members — `message` (`@required`),
//! `resourceId` ("The identifier of the resource that was not found") and `resourceType`
//! ("The type of the resource that was not found") — and every operation that resolves an
//! ARN fills them the same way, through [`resource_not_found`], so two readers of the same
//! missing resource cannot answer differently.

use crate::ops::types::ResourceNotFoundException;

use super::SYSTEM_RESOURCE;

/// `resourceType` for a resilience policy — the `Policy` shape's own name, which is what
/// the type member reports.
pub const POLICY_RESOURCE_TYPE: &str = "Policy";

/// `resourceType` for a service, as [`POLICY_RESOURCE_TYPE`] is for a policy.
pub const SERVICE_RESOURCE_TYPE: &str = "Service";

/// `resourceType` for a system, as [`POLICY_RESOURCE_TYPE`] is for a policy.
pub const SYSTEM_RESOURCE_TYPE: &str = "System";

/// `resourceType` for a service function, as [`POLICY_RESOURCE_TYPE`] is for a policy.
///
/// A function is addressed by its `serviceFunctionId` rather than by an ARN of its own, so
/// a refusal naming one reports that identifier as its `resourceId` — see
/// [`service_function_not_found`].
pub const SERVICE_FUNCTION_RESOURCE_TYPE: &str = "ServiceFunction";

/// `resourceType` for an input source, as [`POLICY_RESOURCE_TYPE`] is for a policy.
///
/// An input source is addressed by its `inputSourceId` within its service rather than by
/// an ARN of its own, so a refusal naming one reports that identifier as its `resourceId`
/// — see [`input_source_not_found`].
pub const INPUT_SOURCE_RESOURCE_TYPE: &str = "InputSource";

/// How a refusal's MESSAGE names a service function: the resource as the service words it
/// in prose, which is not always its shape name — see [`service_function_not_found`].
const SERVICE_FUNCTION_LABEL: &str = "Service function";

/// How a refusal's MESSAGE names an input source, as [`SERVICE_FUNCTION_LABEL`] does a
/// service function.
const INPUT_SOURCE_LABEL: &str = "Input source";

/// The refusal for a request whose `arn` names no stored resource of `resource_type`.
///
/// `resourceId` is the IDENTIFIER of the resource, taken from the ARN the request supplied
/// through [`refusal_resource_id`] — never the whole ARN, whose account, Region and
/// partition segments say where the resource would live rather than which resource it is.
/// `message` is worded by [`not_found_message`] around that same identifier.
pub fn resource_not_found(resource_type: &str, arn: &str) -> ResourceNotFoundException {
    let resource_id = refusal_resource_id(resource_type, arn);
    ResourceNotFoundException {
        message: not_found_message(resource_type, resource_id),
        resource_id: Some(resource_id.to_string()),
        resource_type: Some(resource_type.to_string()),
    }
}

/// The sentence a refusal naming a missing resource of `resource_type` is worded with: the
/// resource type, fixed text, and the `identifier` of the resource that was not found.
///
/// Shared with the refusals that name a missing resource WITHOUT reporting it as the
/// not-found resource of the request — a request whose own subject exists but whose body
/// REFERENCES a resource that does not — so one missing resource is worded one way however
/// it is reached.
pub fn not_found_message(resource_type: &str, identifier: &str) -> String {
    format!("{resource_type} not found: {identifier}")
}

/// The identifier a refusal for `arn` reports: the identifier the resource itself reports.
///
/// A system is the one resource of this store that carries an identifier of its OWN beside
/// its ARN — `System$systemId`, typed `SystemId` ("System ID for cross-account use without
/// exposing account structure") and reported by every reader of a system — and that id is
/// the ARN's resource segment WITHOUT the `system/` qualifier, so a refusal naming a system
/// names that same value. `Service` and `Policy` carry no id member at all, so the
/// identifier of one of those is the whole resource segment the ARN carries.
pub fn refusal_resource_id<'a>(resource_type: &str, arn: &'a str) -> &'a str {
    let resource_id = arn_resource_id(arn);
    if resource_type != SYSTEM_RESOURCE_TYPE {
        return resource_id;
    }
    resource_id
        .strip_prefix(SYSTEM_RESOURCE)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(resource_id)
}

/// The refusal for a `serviceFunctionId` that names no stored service function.
///
/// A function is identified by that id within its service rather than by an ARN, so the
/// id is both the `resourceId` and what the message names. The message names the resource
/// the way the service words it in prose ([`SERVICE_FUNCTION_LABEL`]), while
/// `resourceType` carries the model's own shape name for it.
pub fn service_function_not_found(service_function_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: not_found_message(SERVICE_FUNCTION_LABEL, service_function_id),
        resource_id: Some(service_function_id.to_string()),
        resource_type: Some(SERVICE_FUNCTION_RESOURCE_TYPE.to_string()),
    }
}

/// The refusal for an `inputSourceId` that names no stored input source.
///
/// An input source is identified by that id within its service rather than by an ARN, so
/// the id is both the `resourceId` and what the message names, the way a service
/// function's is.
pub fn input_source_not_found(input_source_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: not_found_message(INPUT_SOURCE_LABEL, input_source_id),
        resource_id: Some(input_source_id.to_string()),
        resource_type: Some(INPUT_SOURCE_RESOURCE_TYPE.to_string()),
    }
}

/// The resource segment of `arn`: the sixth and last field of
/// `arn:<partition>:<service>:<region>:<account>:<resource>`.
///
/// The segment itself may contain further `:` separators (`Arn`'s modelled pattern admits
/// them), so everything after the fifth one belongs to it. A value carrying too few
/// fields to have a resource segment is its own identifier — the caller sent it, and no
/// part of it can be dropped without inventing one.
pub fn arn_resource_id(arn: &str) -> &str {
    arn.splitn(6, ':').nth(5).unwrap_or(arn)
}
