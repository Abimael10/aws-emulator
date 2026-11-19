// SPDX-License-Identifier: CC0-1.0

//! The security plugin's AUDIT-CONFIGURATION resource: `/_plugins/_security/api/audit` and
//! the `config` beneath it.
//!
//! WHICH requests a domain audits is the plugin's own configuration
//! ([`crate::state::audit`]), and this resource is the only way a client reads or changes
//! it — which is what makes an `AUDIT_LOGS` log group fill at all. Three routes, the
//! plugin's own:
//!
//! * `GET /_plugins/_security/api/audit` — the configuration in force, beside the paths
//!   this API will not write;
//! * `PATCH /_plugins/_security/api/audit` — an RFC 6902 JSON Patch over that document,
//!   which is how a client turns one flag or clears one list without restating the rest;
//! * `PUT /_plugins/_security/api/audit/config` — the whole document restated.
//!
//! Each is reachable under the plugin's legacy `_opendistro` prefix as well, like every
//! other route of its API ([`super::serve_resolved`]).
//!
//! It is the MASTER user's alone, like every other record of the security configuration
//! ([`super::security_records::security_api_guard`]) — a caller that could switch auditing
//! off is a caller that could act unrecorded — and the read is a read: it reports the
//! document and writes nothing.

use ::serde_json::json;
use aws_smithy_runtime_api::http::Response as RtResponse;

use crate::state::{audit, Domain};

use super::security_records::{security_api_guard, written};
use super::{ok, rendered, wrong_method, Request};

/// The resource name the plugin knows this configuration by, which its answer to a write
/// that states the whole document names.
const RESOURCE: &str = "config";

/// `GET` or `PATCH` on the audit-configuration resource itself.
pub(super) fn audit_request(domain: &mut Domain, request: &Request<'_>) -> RtResponse {
    if let Some(refusal) = security_api_guard(domain, request) {
        return refusal;
    }
    match request.method {
        "GET" => ok(&domain.audit_config.reported()),
        "PATCH" => {
            let operations = match request.json() {
                Ok(Some(operations)) => operations,
                // A patch of no operations changes nothing, and a body that is not there is
                // not one: the plugin cannot read a write out of it.
                Ok(None) => return refused(audit::Refusal::Unreadable),
                Err(refusal) => return *refusal,
            };
            match domain.audit_config.patched(&operations) {
                Ok(()) => patched(),
                Err(refusal) => refused(refusal),
            }
        }
        _ => wrong_method(request, "GET, PATCH"),
    }
}

/// `PUT` on `/audit/config` — the configuration stated whole rather than patched.
pub(super) fn audit_config_request(domain: &mut Domain, request: &Request<'_>) -> RtResponse {
    if let Some(refusal) = security_api_guard(domain, request) {
        return refusal;
    }
    match request.method {
        "PUT" => {
            let stated = match request.json() {
                Ok(Some(stated)) => stated,
                Ok(None) => return refused(audit::Refusal::Unreadable),
                Err(refusal) => return *refusal,
            };
            match domain.audit_config.replaced(&stated) {
                // The resource was there before this call and is there after it, so the
                // write UPDATED it: there is no create of a configuration every domain
                // already holds.
                Ok(()) => written(RESOURCE, true),
                Err(refusal) => refused(refusal),
            }
        }
        _ => wrong_method(request, "PUT"),
    }
}

/// What a taken PATCH of this resource answers.
///
/// The path names the resource as a whole rather than one record of a collection, so the
/// plugin states the resource it updated rather than a name it was given.
fn patched() -> RtResponse {
    ok(&json!({ "status": "OK", "message": "Resource updated." }))
}

/// What a write this resource does not take answers: the plugin's own envelope, carrying
/// the status it refused with and why ([`audit::Refusal`]).
fn refused(refusal: audit::Refusal) -> RtResponse {
    rendered(
        400,
        &json!({
            "status": "BAD_REQUEST",
            "message": refusal.message(),
        }),
    )
}
