// SPDX-License-Identifier: CC0-1.0

//! The security plugin's INTERNAL USER DATABASE, and the account route that tells one of
//! its users who it is.
//!
//! A domain created with `AdvancedSecurityOptions$InternalUserDatabaseEnabled` keeps users
//! of its OWN beside the IAM principals that reach it: the master user
//! `MasterUserOptions$MasterUserName`/`MasterUserPassword` named, which the control plane
//! wrote, and every user this API has added since. A request presenting HTTP Basic
//! credentials is authenticated against them — the one decision
//! [`crate::state::fgac::authentication`] owns — and what the user it authenticated as may
//! then DO is the role mappings' and the roles' own narrowing, exactly as it is for an AWS
//! principal.
//!
//! Two routes, both of the plugin's `api` surface:
//!
//! * `/_plugins/_security/api/internalusers` and `/{name}` — the database itself, which
//!   only the MASTER user may read or change, like every other record of the security
//!   configuration;
//! * `/_plugins/_security/api/account` — who the AUTHENTICATED caller is, answered for
//!   whatever permissions that caller holds, which is what a client checks its own
//!   credential with before it asks for anything.
//!
//! A user's password is held for one comparison alone: it is never reported back — the
//! answers below carry no hash and no password — and nothing logs it.

use ::serde_json::{json, Map, Value};
use aws_smithy_runtime_api::http::Response as RtResponse;

use crate::state::{fgac, Domain};

use super::security_records::{over_security_quota, security_api_guard, written};
use super::{error, held_roles, ok, rendered, wrong_method, Request, PARSE_EXCEPTION,
    SECURITY_RECORD_QUOTA};

/// The member a write names the user's password in.
const PASSWORD: &str = "password";

/// The member a write may name a pre-hashed password in instead — a credential this cluster
/// cannot compare a presented password against ([`written_user`]).
const HASH: &str = "hash";

/// The member naming the external identities the user is also known by.
const BACKEND_ROLES: &str = "backend_roles";

/// The member naming the security roles the user holds directly, without a role mapping.
const SECURITY_ROLES: &str = "opendistro_security_roles";

/// The member carrying the user's own attributes, which the API reports back as given.
const ATTRIBUTES: &str = "attributes";

/// `GET`, `PUT`, `PATCH` or `DELETE` on the internal user database — the whole of it where
/// `name` is `None`, and one user of it otherwise.
pub(super) fn users_request(
    domain: &mut Domain,
    request: &Request<'_>,
    name: Option<&str>,
) -> RtResponse {
    if let Some(refusal) = security_api_guard(domain, request) {
        return refusal;
    }
    match (request.method, name) {
        ("GET", None) => ok(&Value::Object(every_user(domain))),
        ("GET", Some(name)) => match user_view(domain, name) {
            Some(view) => ok(&json!({ name: view })),
            None => not_found(name),
        },
        ("PUT" | "PATCH", Some(name)) => written_user(domain, request, name),
        ("DELETE", Some(name)) => {
            if is_reserved(domain, name) {
                return read_only(name);
            }
            match domain.internal_users.remove(name) {
                Some(_) => ok(&json!({ "status": "OK", "message": format!("'{name}' deleted.") })),
                None => not_found(name),
            }
        }
        ("PUT" | "PATCH" | "DELETE", None) => wrong_method(request, "GET"),
        _ => wrong_method(request, "GET, PUT, PATCH, DELETE"),
    }
}

/// `GET /_plugins/_security/api/account` — who the authenticated caller is.
///
/// NOT narrowed by the caller's permissions, for the reason the plugin's `authinfo` is not:
/// the plugin answers it for whoever is authenticated, which is what makes it the route a
/// client confirms its credential with. A domain without fine-grained access control runs
/// no security plugin at all, so the path has no handler there.
pub(super) fn account_request(domain: &Domain, request: &Request<'_>) -> RtResponse {
    if !fgac::enforced(domain) {
        return super::no_handler(request);
    }
    if request.method != "GET" {
        return wrong_method(request, "GET");
    }
    let name = request.principal;
    let held = domain.internal_users.get(name);
    // The tenant a user always has is its own, and this request asked for none.
    let mut tenants = Map::new();
    tenants.insert(name.to_string(), Value::Bool(true));
    ok(&json!({
        "user_name": name,
        "is_reserved": is_reserved(domain, name),
        "is_hidden": false,
        "is_internal_user": is_internal_user(domain, name),
        "user_requested_tenant": Value::Null,
        "backend_roles": match held {
            Some(user) => user.backend_roles.clone(),
            None => fgac::backend_roles(name),
        },
        "custom_attribute_names": Vec::<String>::new(),
        "tenants": Value::Object(tenants),
        "roles": held_roles(domain, request),
    }))
}

/// Every user the database holds, keyed by name — the master user the control plane wrote
/// among them, since the master IS a user of the database on a domain that keeps one.
fn every_user(domain: &Domain) -> Map<String, Value> {
    let mut users = Map::new();
    if let Some(master) = master_name(domain) {
        if let Some(view) = user_view(domain, master) {
            users.insert(master.to_string(), view);
        }
    }
    for (name, user) in &domain.internal_users {
        users.insert(name.clone(), reported(user, false));
    }
    users
}

/// One user of the database as the API reports it, or `None` for a name it holds none for.
fn user_view(domain: &Domain, name: &str) -> Option<Value> {
    if let Some(user) = domain.internal_users.get(name) {
        return Some(reported(user, false));
    }
    // The MASTER user of a domain that keeps an internal database: its record is the
    // domain's own, written by the create, and it holds the all-permitting role because
    // that is the mapping the control plane itself made for it.
    (master_name(domain) == Some(name)).then(|| {
        reported(
            &fgac::InternalUser {
                name: name.to_string(),
                security_roles: vec![fgac::ALL_ACCESS_ROLE.to_string()],
                ..fgac::InternalUser::default()
            },
            true,
        )
    })
}

/// One user as the API reports it: everything the write named BUT the password, which is
/// held for authentication alone and reported by nothing.
fn reported(user: &fgac::InternalUser, reserved: bool) -> Value {
    let attributes: Map<String, Value> = user
        .attributes
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    json!({
        BACKEND_ROLES: user.backend_roles,
        SECURITY_ROLES: user.security_roles,
        ATTRIBUTES: Value::Object(attributes),
        "reserved": reserved,
        "hidden": false,
        "static": false,
    })
}

/// The name of the master user of a domain that keeps an internal user database, and `None`
/// for a domain whose master is an IAM principal or that keeps no database.
fn master_name(domain: &Domain) -> Option<&str> {
    fgac::internal_user_database(domain)
        .then_some(domain.master_user_name.as_deref())
        .flatten()
}

/// Whether `name` is a user of the internal database — one this API wrote, or the master
/// user the create named for a domain that keeps one.
fn is_internal_user(domain: &Domain, name: &str) -> bool {
    domain.internal_users.contains_key(name) || master_name(domain) == Some(name)
}

/// Whether `name` is a RESERVED user, which this API may neither change nor remove: the
/// master user the control plane wrote, whose credentials are the domain's configuration
/// and are changed through `UpdateDomainConfig` rather than here.
fn is_reserved(domain: &Domain, name: &str) -> bool {
    master_name(domain) == Some(name)
}

/// Add or replace one user of the database.
fn written_user(domain: &mut Domain, request: &Request<'_>, name: &str) -> RtResponse {
    if is_reserved(domain, name) {
        return read_only(name);
    }
    let body = match request.json() {
        Ok(Some(Value::Object(members))) => members,
        Ok(_) => {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected the body to be a JSON object naming the user",
            )
        }
        Err(refusal) => return *refusal,
    };
    let held = domain.internal_users.get(name);
    let password = body.get(PASSWORD).and_then(Value::as_str);
    // "Specify either 'hash' or 'password' when creating a new internal user": a user the
    // database does not hold yet needs a credential, or it would be a user nothing could
    // ever authenticate as.
    if held.is_none() && password.is_none() && body.get(HASH).is_none() {
        return rendered(
            400,
            &json!({
                "status": "error",
                "reason": "Specify either 'hash' or 'password' when creating a new internal \
                           user.",
            }),
        );
    }
    // A PRE-HASHED credential is one this cluster cannot compare a presented password
    // against: it holds a password to compare, and nothing here computes the hash a
    // `hash` names. Storing the record anyway would answer `created` for a user no request
    // could ever authenticate as, so the write is refused and the member it cannot take is
    // named — the emulator's own bound, stated in the envelope the plugin's own refusals
    // carry.
    if body.get(HASH).is_some() {
        return rendered(
            400,
            &json!({
                "status": "error",
                "reason": "This cluster cannot authenticate a user against a pre-hashed \
                           credential; specify 'password'.",
            }),
        );
    }
    if !domain.internal_users.contains_key(name)
        && domain.internal_users.len() >= SECURITY_RECORD_QUOTA
    {
        return over_security_quota(domain, "internalusers");
    }
    let listed = |member: &str| -> Vec<String> {
        body.get(member)
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
    let attributes = body
        .get(ATTRIBUTES)
        .and_then(Value::as_object)
        .map(|members| {
            members
                .iter()
                .map(|(key, value)| {
                    let text = match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    (key.clone(), text)
                })
                .collect()
        })
        .unwrap_or_default();
    // A write naming no password on an EXISTING user leaves its credential as it was,
    // which is what makes changing a user's roles alone possible. One naming none on a user
    // the database does not hold is refused above, so no record is ever stored with a
    // credential nothing can authenticate against.
    let password = match (password, held) {
        (Some(password), _) => fgac::Secret::new(password),
        (None, Some(held)) => held.password.clone(),
        (None, None) => fgac::Secret::default(),
    };
    let existed = domain
        .internal_users
        .insert(
            name.to_string(),
            fgac::InternalUser {
                name: name.to_string(),
                password,
                backend_roles: listed(BACKEND_ROLES),
                security_roles: listed(SECURITY_ROLES),
                attributes,
            },
        )
        .is_some();
    written(name, existed)
}

/// What the API answers for a name the database holds no user for.
fn not_found(name: &str) -> RtResponse {
    rendered(
        404,
        &json!({ "status": "NOT_FOUND", "message": format!("Resource '{name}' not found.") }),
    )
}

/// What it answers a write or a delete of a RESERVED user ([`is_reserved`]).
fn read_only(name: &str) -> RtResponse {
    rendered(
        403,
        &json!({ "status": "FORBIDDEN", "message": format!("Resource '{name}' is read-only.") }),
    )
}
