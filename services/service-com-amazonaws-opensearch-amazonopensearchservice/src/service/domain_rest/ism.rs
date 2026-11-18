// SPDX-License-Identifier: CC0-1.0

//! The INDEX-MANAGEMENT (ISM) plugin's policy API: the policies a domain holds, and what
//! the plugin itself fills in on one it stores.
//!
//! A policy is a state machine the plugin RUNS over a managed index, so what it stores is
//! not quite the document a client wrote: every option the plugin applies when the client
//! named none is materialised on the write ([`normalised_policy`]), which is why a policy
//! read back carries settings nobody wrote.

use super::*;

/// A `PUT`, `GET` or `DELETE` on ONE index-management policy.
pub(super) fn ism_policy_request(
    domain: &mut Domain,
    request: &Request<'_>,
    id: &str,
    now: SystemTime,
) -> RtResponse {
    match request.method {
        "PUT" => guarded(domain, request, fgac::ACTION_PUT_ISM_POLICY, |domain| {
            write_ism_policy(domain, request, id, now)
        }),
        "GET" => guarded(domain, request, fgac::ACTION_GET_ISM_POLICY, |domain| {
            match domain.ism_policies.get(id) {
                Some(policy) => ok(&ism_policy_view(policy)),
                None => error(404, STATUS_EXCEPTION, "Policy not found"),
            }
        }),
        "DELETE" => guarded(domain, request, fgac::ACTION_DELETE_ISM_POLICY, |domain| {
            match domain.ism_policies.remove(id) {
                Some(policy) => ok(&json!({
                    "_index": ISM_CONFIG_INDEX,
                    "_id": policy.policy_id,
                    "_version": policy.version,
                    "result": "deleted",
                    "_seq_no": policy.seq_no,
                    "_primary_term": policy.primary_term,
                })),
                None => error(404, STATUS_EXCEPTION, "Policy not found"),
            }
        }),
        _ => wrong_method(request, "PUT, GET, DELETE"),
    }
}

/// The engine's error type for a request the index-management plugin refuses.
pub(super) const STATUS_EXCEPTION: &str = "status_exception";

/// The index the index-management plugin keeps its own configuration in, which the write it
/// performs is reported against.
pub(super) const ISM_CONFIG_INDEX: &str = ".opendistro-ism-config";

/// The `_primary_term` every write of this cluster is taken in: one run of a cluster takes
/// every write in one term, and nothing here re-elects a primary.
pub(super) const ISM_PRIMARY_TERM: i64 = 1;

/// Write the index-management policy the path names.
///
/// Creating one is a 201; UPDATING one is a write against the revision the caller read, so
/// it carries `if_seq_no` and `if_primary_term` — "to update an existing policy, you must
/// specify the sequence number and primary term" — and a write that names neither, or names
/// a revision the policy has moved past, is the engine's version conflict rather than a
/// silent overwrite of someone else's policy.
pub(super) fn write_ism_policy(
    domain: &mut Domain,
    request: &Request<'_>,
    id: &str,
    now: SystemTime,
) -> RtResponse {
    let body = match request.json() {
        Ok(Some(Value::Object(members))) => Value::Object(members),
        Ok(_) => {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected the body to be a JSON object carrying a policy",
            )
        }
        Err(refusal) => return *refusal,
    };
    let Some(definition) = body.get("policy").filter(|policy| policy.is_object()) else {
        return error(400, PARSE_EXCEPTION, "[policy] required property is missing");
    };
    let held = domain.ism_policies.get(id);
    if let Some(held) = held {
        let named = (
            request.parameter("if_seq_no").and_then(|v| v.parse().ok()),
            request
                .parameter("if_primary_term")
                .and_then(|v| v.parse().ok()),
        );
        if named != (Some(held.seq_no), Some(held.primary_term)) {
            return error(
                409,
                VERSION_CONFLICT_ENGINE_EXCEPTION,
                &format!(
                    "[{id}]: version conflict, required seqNo [{}], primary term [{}]",
                    held.seq_no, held.primary_term
                ),
            );
        }
    } else if domain.ism_policies.len() >= state::ISM_POLICY_QUOTA {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "cluster [{}] already holds the maximum of {} index management policies, so \
                 policy [{id}] could not be created",
                cluster_name(domain),
                state::ISM_POLICY_QUOTA
            ),
        );
    }
    let created = held.is_none();
    let version = held.map_or(1, |held| held.version.saturating_add(1));
    // The sequence number of a write is the next one over the plugin's configuration index,
    // so it is one past the highest any policy carries.
    let seq_no = domain
        .ism_policies
        .values()
        .map(|policy| policy.seq_no)
        .max()
        .map_or(0, |highest| highest.saturating_add(1));
    let policy = state::IsmPolicy {
        policy_id: id.to_string(),
        // Stored as the PLUGIN holds it: the document the client wrote, under the defaults
        // the plugin applies to it ([`normalised_policy`]).
        definition: normalised_policy(definition),
        version,
        seq_no,
        primary_term: ISM_PRIMARY_TERM,
        updated_at: now,
    };
    let view = ism_policy_view(&policy);
    domain.ism_policies.insert(id.to_string(), policy);
    rendered(if created { 201 } else { 200 }, &view)
}

/// One index-management policy, as that plugin's policy API reports it: the revision
/// counters of the write that stored it, and the policy document itself under the member it
/// was written in.
///
/// The document is reported as the plugin HOLDS it — the client's own document under the
/// defaults the write applied to it ([`normalised_policy`]) — with the two members the plugin
/// fills in itself beside it: the policy's own id, which is the one the path addressed it by,
/// and when it was last written.
pub(super) fn ism_policy_view(policy: &state::IsmPolicy) -> Value {
    let mut reported = policy
        .definition
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    reported.insert("policy_id".to_string(), json!(policy.policy_id));
    reported.insert(
        "last_updated_time".to_string(),
        json!(state::epoch_millis(policy.updated_at)),
    );
    json!({
        "_id": policy.policy_id,
        "_version": policy.version,
        "_seq_no": policy.seq_no,
        "_primary_term": policy.primary_term,
        "policy": { "policy": Value::Object(reported) },
    })
}

/// How many times the plugin retries an action that fails, how it spaces the attempts and
/// how long it waits before the first: the ACTION RETRY every action of every state of a
/// stored policy carries, which the plugin applies to an action that declares none.
///
/// A policy is run by the PLUGIN rather than by the client, so the attempts an action gets
/// are part of what the plugin stores for it: the defaults are materialised on the write,
/// which is what a read of the policy reports.
const DEFAULT_RETRY_COUNT: i64 = 3;
const DEFAULT_RETRY_BACKOFF: &str = "exponential";
const DEFAULT_RETRY_DELAY: &str = "1m";

/// The member an action carries its retry policy under.
const RETRY: &str = "retry";

/// The member a policy carries its states under, and the member a state carries its actions
/// under.
const STATES: &str = "states";
const ACTIONS: &str = "actions";

/// The policy document as the plugin STORES it: the document the client wrote, with every
/// option the plugin fills in itself materialised on it.
///
/// That is the action retry ([`DEFAULT_RETRY_COUNT`]): every action of every state is retried
/// by the plugin, so an action declaring no `retry` of its own is stored carrying the one the
/// plugin would retry it under — not left without retry semantics at all. An action that
/// declares one keeps it: a default is what the client did not state.
fn normalised_policy(definition: &Value) -> Value {
    let mut policy = definition.as_object().cloned().unwrap_or_else(Map::new);
    if let Some(Value::Array(states)) = policy.get_mut(STATES) {
        for stage in states.iter_mut() {
            let Some(Value::Array(actions)) = stage.get_mut(ACTIONS) else {
                continue;
            };
            for action in actions.iter_mut() {
                let Some(action) = action.as_object_mut() else {
                    continue;
                };
                action.entry(RETRY).or_insert_with(|| {
                    json!({
                        "count": DEFAULT_RETRY_COUNT,
                        "backoff": DEFAULT_RETRY_BACKOFF,
                        "delay": DEFAULT_RETRY_DELAY,
                    })
                });
            }
        }
    }
    Value::Object(policy)
}
