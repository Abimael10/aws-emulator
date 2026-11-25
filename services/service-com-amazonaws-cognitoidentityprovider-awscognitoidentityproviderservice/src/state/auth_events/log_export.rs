// SPDX-License-Identifier: CC0-1.0

//! Where the events a pool LOGS are also DELIVERED: the destinations its log delivery
//! configuration names.
//!
//! A pool with threat protection switched on logs every authentication, registration,
//! password change, recovery and code resend it serves ([`super`]), and that log is not only
//! readable through this service's own API: a pool can also EXPORT it. "User pools can export
//! user notification logs and, when threat protection is active, user-activity logs"
//! (`SetLogDeliveryConfiguration`), configured per event source —
//! `LogConfigurationType.EventSource` is "to send info-level logs about threat-protection
//! user activity in user pools with the Plus feature plan, set to `userAuthEvents`"
//! ([`USER_AUTH_EVENTS_SOURCE`]). So every event the pool logs is also one RECORD written
//! into each destination its `userAuthEvents` configuration names: a configuration a client
//! reads back as on and never receives a record from is a trail that does not exist, which is
//! indistinguishable from a pool nobody signed in to.
//!
//! # What is exported, and when
//!
//! The record is the LOGGED EVENT, rendered: the same event id, event type, outcome and
//! challenge results that `AdminListUserAuthEvents` serves for the same attempt, so the
//! exported trail and the pool's own event history cannot disagree about one sign-in. An
//! attempt that is NOT over exports nothing yet — the call that answers its outstanding
//! challenge finishes the event and exports the outcome — which is what makes one terminated
//! attempt exactly one record.
//!
//! Export is gated on the two things that make the logs available at all: the pool's feature
//! plan has to include the source ([`super::super::PLUS_USER_POOL_TIER`], which
//! `LogConfigurationType.EventSource` names) and its threat protection has to be active
//! ([`super::super::UserPool::add_ons_enabled`], which is also what makes the pool log the
//! event in the first place). The configuration is read at EMISSION time, so clearing
//! `LogConfigurations` stops the export for later attempts and re-setting it resumes them,
//! with no record for the attempts made in between.
//!
//! # Where the records go
//!
//! Into the log group the destination's ARN names, through CloudWatch Logs' OWN ingest path
//! ([`::service_common::cloudwatch_logs::delivered_into_held_group`], the one vended-logs
//! write this tree makes into a group the ACCOUNT holds): the destination is a group of the
//! account's, created by whoever owns it, so a pool delivers into it and creates none. An
//! exported record is therefore an ingestion a reader of that group filters for, subject to
//! every consumer the group is configured with, rather than a second copy of the trail kept
//! here.
//!
//! Delivery is BEST EFFORT, as every vended-logs write is: the call that produced the event
//! has already been answered, so a destination the account has since deleted loses the record
//! and is reported in tracing rather than failing anything.

use ::std::time::SystemTime;

use super::{Logged, Recorded};

/// `eventSource`'s value for the user-activity logs a pool with threat protection
/// exports: "to send info-level logs about threat-protection user activity in user pools
/// with the Plus feature plan, set to `userAuthEvents`"
/// (`LogConfigurationType.EventSource`).
pub const USER_AUTH_EVENTS_SOURCE: &str = "userAuthEvents";

/// The `eventSource` an exported record carries, which is the source above as the delivered
/// record names it.
const EXPORTED_EVENT_SOURCE: &str = "USER_AUTH_EVENTS";

/// Log what one call contributed to the event history of the user it was attempted for
/// ([`super::record`]), and export that event where the pool's log delivery points it.
///
/// The two are ONE step for every call that logs an event: a pool does not hold a trail it
/// does not export, and does not export a trail it did not log. Call it with no state guard
/// of this service held — the log is taken under its own lock, which is released before the
/// delivery, which awaits.
pub async fn record_and_export<Eng: Send + Sync + 'static>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
    logged: &Logged<'_>,
    at: SystemTime,
) {
    let Some(recorded) = super::record(engine, logged, at) else {
        return;
    };
    export(ctx, engine, &recorded).await;
}

/// Write `recorded` into each `userAuthEvents` destination the pool it belongs to names.
async fn export<Eng: Send + Sync + 'static>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
    recorded: &Recorded,
) {
    // An attempt a challenge is still outstanding for is not over: the call that answers
    // that challenge finishes this very event and exports its outcome, so exporting now
    // would make one attempt two records.
    if recorded.event_response == super::EVENT_RESPONSE_IN_PROGRESS {
        return;
    }
    let destinations = destinations(engine, &recorded.user_pool_id);
    if destinations.is_empty() {
        return;
    }
    let record = exported_record(recorded).to_string();
    for destination in destinations {
        let written = ::service_common::cloudwatch_logs::delivered_into_held_group(
            ctx,
            &destination.region,
            &destination.log_group,
            // The stream is the pool whose activity the records report, so two pools
            // exporting to one group stay separable in it.
            &recorded.user_pool_id,
            ::std::slice::from_ref(&record),
        )
        .await;
        if !written {
            ::tracing::warn!(
                log_group = ?destination.log_group,
                "the log group this user pool's userAuthEvents delivery names did not take an \
                 exported authentication event"
            );
        }
    }
}

/// One CloudWatch Logs destination a pool's `userAuthEvents` configuration names.
#[derive(Debug)]
struct Destination {
    /// The Region the group is held in, which is what the write is addressed to.
    region: String,
    /// The NAME of the log group the records are written into.
    log_group: String,
}

/// The CloudWatch Logs destinations the pool `user_pool_id` exports its user-activity logs
/// to, as its STORED configuration names them.
///
/// Empty for a pool this service does not hold, for one whose feature plan does not include
/// the source or whose threat protection is not active, and for one whose configuration names
/// no CloudWatch Logs destination for the source — in each case the account has asked for no
/// such export. A destination of another kind (an Amazon Data Firehose stream, an Amazon S3
/// bucket) is not one this module writes: those are deliveries of their own.
fn destinations<Eng: Send + Sync + 'static>(engine: &Eng, user_pool_id: &str) -> Vec<Destination> {
    let Some(state) = crate::state::service_state(engine) else {
        return Vec::new();
    };
    let guard = state.lock();
    let Some(pool) = guard.pool(user_pool_id) else {
        return Vec::new();
    };
    if !crate::state::user_pool_tier_includes(
        &pool.user_pool_tier,
        crate::state::PLUS_USER_POOL_TIER,
    ) || !pool.add_ons_enabled()
    {
        return Vec::new();
    }
    pool.log_configurations
        .iter()
        .filter(|configuration| configuration.event_source == USER_AUTH_EVENTS_SOURCE)
        .filter_map(|configuration| configuration.cloud_watch_logs_configuration.as_ref())
        .filter_map(|cloud_watch_logs| cloud_watch_logs.log_group_arn.as_deref())
        .filter_map(|arn| {
            // The one reading of a log group destination's ARN this tree makes, which
            // accepts both forms of it — the group alone, and the group with everything
            // under it.
            let log_group = ::service_common::cloudwatch_logs::log_group_of_arn(arn)?;
            Some(Destination {
                // Which Region the destination is in is read where the set that ACCEPTED it
                // reads it, so the accept path and this write cannot address one group
                // differently.
                region: crate::ops::set_log_delivery_configuration::log_destinations
                    ::log_group_destination_region(arn, &pool.region)
                    .to_string(),
                log_group: log_group.to_string(),
            })
        })
        .collect()
}

/// The record one logged event is exported as.
///
/// The fields are the ones a consumer of the trail reads an exported user-activity record by:
/// the SOURCE of the records, the resource they are about, and the event itself — its id, what
/// was attempted, the outcome it reached, the user, pool and app client it ran against, and
/// the challenges that were checked, each as `<ChallengeName>:<ChallengeResponse>`. Every
/// value comes from the stored event or the pool that holds it; nothing is reported that this
/// service does not hold, which is why an event logged for a call that models no app client
/// carries no `clientId` rather than an empty one.
fn exported_record(recorded: &Recorded) -> ::serde_json::Value {
    let challenges: Vec<String> = recorded
        .challenge_responses
        .iter()
        .filter_map(|challenge| {
            let name = challenge.challenge_name.as_deref()?;
            let response = challenge.challenge_response.as_deref()?;
            Some(format!("{name}{CHALLENGE_RESULT_SEPARATOR}{response}"))
        })
        .collect();
    let mut message = ::serde_json::Map::new();
    for (member, value) in [
        ("eventId", &recorded.event_id),
        ("eventType", &recorded.event_type),
        ("eventResponse", &recorded.event_response),
        ("userName", &recorded.username),
        ("userPoolId", &recorded.user_pool_id),
    ] {
        message.insert(member.to_string(), ::serde_json::json!(value));
    }
    if let Some(client_id) = recorded.client_id.as_ref() {
        message.insert("clientId".to_string(), ::serde_json::json!(client_id));
    }
    message.insert("challenges".to_string(), ::serde_json::json!(challenges));
    ::serde_json::json!({
        "eventSource": EXPORTED_EVENT_SOURCE,
        "logSourceId": { "userPoolId": &recorded.user_pool_id },
        "message": ::serde_json::Value::Object(message),
    })
}

/// What joins a challenge's name to its result in an exported record's `challenges`.
const CHALLENGE_RESULT_SEPARATOR: &str = ":";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::ChallengeResponseType;

    /// One recorded event to render, with `challenge_responses` as the log holds them.
    fn recorded(
        event_response: &str,
        client_id: Option<&str>,
        challenge_responses: Vec<(&str, &str)>,
    ) -> Recorded {
        Recorded {
            user_pool_id: "us-east-1_exported".to_string(),
            client_id: client_id.map(str::to_string),
            username: "marker-user".to_string(),
            event_id: "11111111-2222-3333-4444-555555555555".to_string(),
            event_type: super::super::EventType::SignIn.as_str().to_string(),
            event_response: event_response.to_string(),
            challenge_responses: challenge_responses
                .into_iter()
                .map(|(name, response)| ChallengeResponseType {
                    challenge_name: Some(name.to_string()),
                    challenge_response: Some(response.to_string()),
                })
                .collect(),
        }
    }

    /// RULE: an exported record names the source of the records, the pool they are about and
    /// the logged event itself, with each checked challenge as its name and its result.
    #[test]
    fn an_exported_record_renders_the_logged_event() {
        for (case, event_response, client_id, challenge_responses, expected_challenges) in [
            (
                "a completed sign-in",
                super::super::EVENT_RESPONSE_PASS,
                Some("1h57kf5cpq17m0eml12example"),
                vec![(
                    super::super::PASSWORD_CHALLENGE,
                    super::super::CHALLENGE_SUCCESS,
                )],
                vec!["Password:Success"],
            ),
            (
                "a refused sign-in",
                super::super::EVENT_RESPONSE_FAIL,
                Some("1h57kf5cpq17m0eml12example"),
                vec![(
                    super::super::PASSWORD_CHALLENGE,
                    super::super::CHALLENGE_FAILURE,
                )],
                vec!["Password:Failure"],
            ),
            (
                "a sign-in that answered a second factor as well",
                super::super::EVENT_RESPONSE_PASS,
                Some("1h57kf5cpq17m0eml12example"),
                vec![
                    (
                        super::super::PASSWORD_CHALLENGE,
                        super::super::CHALLENGE_SUCCESS,
                    ),
                    (super::super::MFA_CHALLENGE, super::super::CHALLENGE_SUCCESS),
                ],
                vec!["Password:Success", "Mfa:Success"],
            ),
            (
                "an event of a call that models no app client",
                super::super::EVENT_RESPONSE_PASS,
                None,
                Vec::new(),
                Vec::new(),
            ),
        ] {
            let recorded = recorded(event_response, client_id, challenge_responses);
            let record = exported_record(&recorded);
            assert_eq!(
                record.get("eventSource").and_then(::serde_json::Value::as_str),
                Some(EXPORTED_EVENT_SOURCE),
                "case {case:?}: expected the exported source, got {record}"
            );
            assert_eq!(
                record
                    .pointer("/logSourceId/userPoolId")
                    .and_then(::serde_json::Value::as_str),
                Some(recorded.user_pool_id.as_str()),
                "case {case:?}: expected the pool the records are about, got {record}"
            );
            for (member, expected) in [
                ("eventId", Some(recorded.event_id.as_str())),
                ("eventType", Some(recorded.event_type.as_str())),
                ("eventResponse", Some(recorded.event_response.as_str())),
                ("userName", Some(recorded.username.as_str())),
                ("userPoolId", Some(recorded.user_pool_id.as_str())),
                ("clientId", recorded.client_id.as_deref()),
            ] {
                let found = record
                    .pointer(&format!("/message/{member}"))
                    .and_then(::serde_json::Value::as_str);
                assert_eq!(
                    found, expected,
                    "case {case:?}: expected message.{member} to be {expected:?}, got {found:?} \
                     in {record}"
                );
            }
            let found: Vec<&str> = record
                .pointer("/message/challenges")
                .and_then(::serde_json::Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(::serde_json::Value::as_str)
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                found, expected_challenges,
                "case {case:?}: expected the challenges {expected_challenges:?}, got {found:?}"
            );
        }
    }
}
