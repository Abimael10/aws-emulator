// SPDX-License-Identifier: CC0-1.0

//! What one request to the API server ADDRESSES, read off its method and its path.
//!
//! The ONE reading for the authorizer and for the audit record, so the decision and
//! the record of it cannot describe a request differently. It is total: slice patterns
//! and `first`/`get` over the path's segments, no indexing of request-derived text, so
//! a path of any shape is read rather than refused by a panic.

use super::*;

/// What one request to the API server addresses, as the authorizer reads it off the
/// method and the path — the resource request's group, resource, subresource,
/// namespace and name, or the path of a request that addresses no resource at all.
#[derive(Debug, Clone, Default)]
pub struct RequestAttributes {
    pub verb: String,
    pub api_group: String,
    pub resource: String,
    pub subresource: String,
    pub namespace: String,
    pub name: String,
    /// The request's path, which is what a non-resource rule names.
    pub path: String,
}

impl RequestAttributes {
    /// Whether the request addresses a resource rather than a path of the server's
    /// own (`/version`, `/healthz`, the discovery documents).
    #[must_use]
    pub fn is_resource_request(&self) -> bool {
        !self.resource.is_empty()
    }

    /// The `Status` the API server refuses a request RBAC does not admit with, in
    /// the API server's own wording.
    #[must_use]
    pub fn forbidden(&self, username: &str) -> ApiRefusal {
        if !self.is_resource_request() {
            return ApiRefusal::forbidden(
                format!(
                    "forbidden: User \"{username}\" cannot {} path \"{}\"",
                    self.verb, self.path
                ),
                None,
            );
        }
        let resource = if self.subresource.is_empty() {
            self.resource.clone()
        } else {
            format!("{}/{}", self.resource, self.subresource)
        };
        let qualified = if self.api_group.is_empty() {
            self.resource.clone()
        } else {
            format!("{}.{}", self.resource, self.api_group)
        };
        let scope = if self.namespace.is_empty() {
            "at the cluster scope".to_string()
        } else {
            format!("in the namespace \"{}\"", self.namespace)
        };
        // The addressed object's name is part of the sentence exactly when the
        // request names one: a request addressing a collection has no name to
        // attribute the denial to, and gets the shorter form.
        let addressed = if self.name.is_empty() {
            qualified
        } else {
            format!("{qualified} \"{}\"", self.name)
        };
        ApiRefusal::forbidden(
            format!(
                "{addressed} is forbidden: User \"{username}\" cannot {} resource \
                 \"{resource}\" in API group \"{}\" {scope}",
                self.verb, self.api_group
            ),
            Some((self.resource.clone(), self.name.clone())),
        )
    }
}

/// The path every Kubernetes core-API request is addressed under.
pub(super) const CORE_API_SEGMENTS: [&str; 2] = ["api", "v1"];

/// The segment a namespaced path carries before the namespace it addresses.
pub(super) const NAMESPACES_SEGMENT: &str = "namespaces";

/// What `method` on `path` addresses.
///
/// The ONE reading of a request for the authorizer and for the audit record, so the
/// decision and the record of it cannot describe a request differently.
#[must_use]
pub fn request_attributes(method: &str, path: &str) -> RequestAttributes {
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let (api_group, rest) = if let Some(rest) = segments.strip_prefix(&CORE_API_SEGMENTS[..]) {
        (String::new(), rest)
    } else if let ["apis", group, _version, rest @ ..] = segments.as_slice() {
        ((*group).to_string(), rest)
    } else {
        return RequestAttributes {
            verb: non_resource_verb(method),
            path: path.to_string(),
            ..RequestAttributes::default()
        };
    };
    if rest.is_empty() {
        return RequestAttributes {
            verb: non_resource_verb(method),
            api_group,
            path: path.to_string(),
            ..RequestAttributes::default()
        };
    }
    let (namespace, addressed) = match rest {
        [first, namespace, addressed @ ..]
            if *first == NAMESPACES_SEGMENT && !addressed.is_empty() =>
        {
            ((*namespace).to_string(), addressed)
        }
        // `/api/v1/namespaces` and `/api/v1/namespaces/{name}` address the
        // `namespaces` resource itself, which is the namespace the request is in.
        [first, namespace] if *first == NAMESPACES_SEGMENT => {
            return RequestAttributes {
                verb: resource_verb(method, true),
                api_group,
                resource: NAMESPACES_SEGMENT.to_string(),
                namespace: (*namespace).to_string(),
                name: (*namespace).to_string(),
                path: path.to_string(),
                ..RequestAttributes::default()
            };
        }
        _ => (String::new(), rest),
    };
    let resource = addressed.first().copied().unwrap_or_default().to_string();
    let name = addressed.get(1).copied().unwrap_or_default().to_string();
    let subresource = addressed.get(2).copied().unwrap_or_default().to_string();
    RequestAttributes {
        verb: resource_verb(method, !name.is_empty()),
        api_group,
        resource,
        subresource,
        namespace,
        name,
        path: path.to_string(),
    }
}

/// The verb a method is on a resource: a read of one object is `get` and a read of a
/// collection is `list`, a delete of a collection is `deletecollection`, and the
/// write methods are the verbs named after them.
pub(super) fn resource_verb(method: &str, has_name: bool) -> String {
    match method {
        "GET" | "HEAD" => {
            if has_name {
                "get"
            } else {
                "list"
            }
        }
        "POST" => "create",
        "PUT" => "update",
        "PATCH" => "patch",
        "DELETE" => {
            if has_name {
                "delete"
            } else {
                "deletecollection"
            }
        }
        _ => return method.to_ascii_lowercase(),
    }
    .to_string()
}
