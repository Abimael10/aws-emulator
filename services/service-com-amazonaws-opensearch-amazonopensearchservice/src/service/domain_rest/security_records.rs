// SPDX-License-Identifier: CC0-1.0

//! The security plugin's own ROLE and ROLE-MAPPING records: `/_plugins/_security/api/roles`
//! and `/_plugins/_security/api/rolesmapping`.
//!
//! A role says what holding it is worth — the cluster and index actions it permits, and the
//! documents and fields of a named index it narrows a read to, which
//! [`crate::state::fgac_permissions`] reads off the document stored here. A role MAPPING
//! says which principals hold it. Both are the MASTER user's alone to read or change
//! ([`security_api_guard`]), which is what keeps a principal from granting itself anything.
//!
//! The quota and the written/created wording are shared with the plugin's other records
//! ([`super::internal_users`]), so no two of them can answer one condition differently.

use super::*;

/// A request on the security plugin's role API.
pub(super) fn security_role(domain: &mut Domain, request: &Request<'_>, name: &str) -> RtResponse {
    if let Some(refusal) = security_api_guard(domain, request) {
        return refusal;
    }
    match request.method {
        "PUT" | "PATCH" => {
            let definition = match request.json() {
                Ok(Some(Value::Object(members))) => Value::Object(members),
                Ok(_) => {
                    return error(
                        400,
                        PARSE_EXCEPTION,
                        "failed to parse: expected the body to be a JSON object",
                    )
                }
                Err(refusal) => return *refusal,
            };
            if !domain.security_roles.contains_key(name)
                && domain.security_roles.len() >= SECURITY_RECORD_QUOTA
            {
                return over_security_quota(domain, "roles");
            }
            let existed = domain
                .security_roles
                .insert(
                    name.to_string(),
                    fgac::SecurityRole {
                        name: name.to_string(),
                        definition,
                    },
                )
                .is_some();
            written(name, existed)
        }
        "GET" => match domain.security_roles.get(name) {
            Some(role) => ok(&json!({ name: role.definition })),
            None => rendered(
                404,
                &json!({ "status": "NOT_FOUND", "message": format!("role {name} not found.") }),
            ),
        },
        "DELETE" => match domain.security_roles.remove(name) {
            Some(_) => ok(
                &json!({ "status": "OK", "message": format!("'{name}' deleted.") }),
            ),
            None => rendered(
                404,
                &json!({ "status": "NOT_FOUND", "message": format!("role {name} not found.") }),
            ),
        },
        _ => wrong_method(request, "PUT, PATCH, GET, DELETE"),
    }
}

/// A request on the security plugin's role-mapping API — the records that decide which
/// principals hold a role, and so what any principal but the master user may do.
pub(super) fn security_role_mapping(domain: &mut Domain, request: &Request<'_>, name: &str) -> RtResponse {
    if let Some(refusal) = security_api_guard(domain, request) {
        return refusal;
    }
    match request.method {
        "PUT" | "PATCH" => {
            let definition = match request.json() {
                Ok(Some(Value::Object(members))) => Value::Object(members),
                Ok(_) => {
                    return error(
                        400,
                        PARSE_EXCEPTION,
                        "failed to parse: expected the body to be a JSON object",
                    )
                }
                Err(refusal) => return *refusal,
            };
            let listed = |member: &str| -> Vec<String> {
                definition
                    .get(member)
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            if !domain.security_role_mappings.contains_key(name)
                && domain.security_role_mappings.len() >= SECURITY_RECORD_QUOTA
            {
                return over_security_quota(domain, "rolesmapping");
            }
            let existed = domain
                .security_role_mappings
                .insert(
                    name.to_string(),
                    fgac::SecurityRoleMapping {
                        role: name.to_string(),
                        backend_roles: listed("backend_roles"),
                        users: listed("users"),
                        hosts: listed("hosts"),
                    },
                )
                .is_some();
            written(name, existed)
        }
        "GET" => match domain.security_role_mappings.get(name) {
            Some(mapping) => ok(&json!({
                name: {
                    "hosts": mapping.hosts,
                    "users": mapping.users,
                    "backend_roles": mapping.backend_roles,
                }
            })),
            None => rendered(
                404,
                &json!({
                    "status": "NOT_FOUND",
                    "message": format!("rolesmapping {name} not found."),
                }),
            ),
        },
        "DELETE" => match domain.security_role_mappings.remove(name) {
            Some(_) => ok(
                &json!({ "status": "OK", "message": format!("'{name}' deleted.") }),
            ),
            None => rendered(
                404,
                &json!({
                    "status": "NOT_FOUND",
                    "message": format!("rolesmapping {name} not found."),
                }),
            ),
        },
        _ => wrong_method(request, "PUT, PATCH, GET, DELETE"),
    }
}

/// What the security API answers a write past [`SECURITY_RECORD_QUOTA`]: the record is not
/// stored, so the configuration this emulator holds stays bounded.
pub(super) fn over_security_quota(domain: &Domain, kind: &str) -> RtResponse {
    error(
        400,
        ingest::ILLEGAL_ARGUMENT_EXCEPTION,
        &format!(
            "domain [{}] already holds the maximum of {SECURITY_RECORD_QUOTA} {kind} records",
            domain.domain_name
        ),
    )
}

/// What the security API answers a request on a domain it is not reachable on: the plugin
/// is part of fine-grained access control, so a domain without it has no such handler at
/// all, and only the MASTER user may change the security configuration of a domain that
/// has it.
pub(super) fn security_api_guard(domain: &Domain, request: &Request<'_>) -> Option<RtResponse> {
    if !fgac::enforced(domain) {
        return Some(no_handler(request));
    }
    fgac::security_api_refusal(domain, request.principal)
        .map(|reason| error(403, ingest::SECURITY_EXCEPTION, &reason))
}

/// What the security API answers a write: the record was created, or the one that was
/// there was replaced.
pub(super) fn written(name: &str, existed: bool) -> RtResponse {
    if existed {
        return ok(&json!({ "status": "OK", "message": format!("'{name}' updated.") }));
    }
    rendered(
        201,
        &json!({ "status": "CREATED", "message": format!("'{name}' created.") }),
    )
}
