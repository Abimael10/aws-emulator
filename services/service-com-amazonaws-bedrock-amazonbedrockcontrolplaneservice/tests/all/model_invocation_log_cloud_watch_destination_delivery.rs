// SPDX-License-Identifier: CC0-1.0

//! MODEL INVOCATION LOGGING into a CloudWatch Logs destination, end to end across the
//! three planes: the configuration this control plane admits is the destination the
//! inference plane PUTS each served invocation's log event in, in the log group CloudWatch
//! Logs holds.
//!
//! Storing a configuration and delivering to it are two different observable states, and
//! this suite is about the second one for the `cloudWatchConfig` destination. An admitted
//! `PutModelInvocationLoggingConfiguration` naming a log group has to leave every
//! subsequent inference putting one `ModelInvocationLog` event in that group — so an
//! operator who sees the configuration echoed back by
//! `GetModelInvocationLoggingConfiguration` is right to conclude an audit trail is being
//! written, and a client polling `FilterLogEvents` for its own invocation's record finds
//! it — and the `DeleteModelInvocationLoggingConfiguration` that withdraws it has to stop
//! them, leaving what was already delivered where it is.
//!
//! The rows differ along the form of the log group name the destination is configured with
//! and along the per-datatype delivery switch the configuration carries, which is what
//! decides how much of a record travels. The record's own layout and contents are asserted
//! over the whole set of inference surfaces by the inference plane's own suite
//! (`model_invocation_log_cloud_watch_delivery.rs` of that crate); what is asserted here is
//! the WIRING: this control plane's put and delete decide whether anything is put in the
//! group at all.
//!
//! The four services a record travels through are mounted on ONE shared engine, exactly as
//! `core/router` assembles them: this control plane, the Bedrock Runtime data plane that
//! serves the inference, the CloudWatch Logs whose group the event lands in — read back
//! here through its own `FilterLogEvents` — and the IAM that holds the delivery role the
//! destination names, because a destination is only admitted once both halves of the pair
//! it describes resolve.

use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;

use crate::control_plane_authorization::{given_role, trust};
use crate::support::{emulator_with_inference_logs_and_iam, Emulator, ACCOUNT, REGION};

/// The URI the logging configuration is addressed by.
const LOGGING_URI: &str = "/logging/modelinvocations";

/// The model every inference below is served on.
const MODEL: &str = "amazon.nova-lite-v1:0";

/// The role every destination below is configured with, held by this account and trusted to
/// the service, so the put is admitted and the delivery is the thing under test.
const DELIVERY_ROLE: &str = "BedrockInvocationLogDelivery";

/// How many `FilterLogEvents` pages one case's reader walks: a case serves at most a
/// couple of invocations, and a page that carried events is followed by at most one that
/// carries none.
const MAX_PAGES: usize = 8;

/// The account the emulator serves, which every delivered record names.
fn account_id() -> String {
    ::engine::emulator_account_id().to_string()
}

/// A log group CloudWatch Logs holds under `name`, as the account's owner creates it before
/// pointing a logging configuration at it.
async fn given_log_group(emu: &Emulator, case: &str, name: &str) {
    let created = emu
        .call_awsjson(
            "logs",
            "Logs_20140328.CreateLogGroup",
            json!({ "logGroupName": name }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the log group {name:?} must be created, got {} {:?}",
        created.status, created.body
    );
}

/// The records the log group `name` holds: one per log event `FilterLogEvents` reports, its
/// `message` parsed as the JSON document a model invocation log event carries.
async fn delivered_records(emu: &Emulator, case: &str, name: &str) -> Vec<Value> {
    let mut records: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = json!({ "logGroupName": name });
        if let Some(token) = next.as_ref() {
            request["nextToken"] = json!(token);
        }
        let page = emu
            .call_awsjson("logs", "Logs_20140328.FilterLogEvents", request)
            .await;
        assert_eq!(
            page.status, 200,
            "case {case:?}: the log group {name:?} must be filterable, got {} {:?}",
            page.status, page.body
        );
        let events = page.body["events"].as_array().cloned().unwrap_or_default();
        let carried = events.len();
        for event in events {
            assert_eq!(
                event["logStreamName"].as_str(),
                Some(logging::LOG_STREAM_NAME),
                "case {case:?}: expected the event in the invocation-log stream, got {event}"
            );
            let message = event["message"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: a delivered log event carries no message: {event}")
            });
            records.push(serde_json::from_str(message).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered record is not JSON ({error}): {message:?}")
            }));
        }
        next = page.body["nextToken"].as_str().map(str::to_string);
        if carried == 0 || next.is_none() {
            break;
        }
    }
    records
}

/// One Converse call on the data plane mounted beside this control plane, which shares this
/// service's signing name and is told apart by its own path space.
async fn converse(emu: &Emulator, case: &str, prompt: &str) -> Value {
    let reply = emu
        .call(
            "POST",
            &format!("/model/{MODEL}/converse"),
            Some(json!({
                "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
            })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the inference must be served, got {} {:?}",
        reply.status, reply.body
    );
    reply.body
}

/// One configuration to install, and what it makes of the events delivered for the
/// invocations served while it stands.
struct Case {
    case: &'static str,
    /// `cloudWatchConfig.logGroupName`, which is also the group the case creates.
    log_group: &'static str,
    /// `textDataDeliveryEnabled`, or `None` for a configuration that names it not at all.
    text_data_delivery_enabled: Option<bool>,
    /// Whether the prompt and the completion travel in the record.
    bodies_travel: bool,
}

/// The configuration table: the rows differ along the FORM of the configured log group name
/// (a path-like name, a flat one, one carrying the punctuation CloudWatch Logs' published
/// name rule admits) and along the text-delivery switch — named on, named off, and not
/// named at all, which withholds nothing.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "a path-like group with text delivery on",
            log_group: "/aws/bedrock/invocation-logs",
            text_data_delivery_enabled: Some(true),
            bodies_travel: true,
        },
        Case {
            case: "a flat group name with text delivery off",
            log_group: "bedrock-invocation-logs",
            text_data_delivery_enabled: Some(false),
            bodies_travel: false,
        },
        Case {
            case: "a group carrying the punctuation the name rule admits, switch unnamed",
            log_group: "Audit.Bedrock_Logs/2026#1",
            text_data_delivery_enabled: None,
            bodies_travel: true,
        },
    ]
}

/// The `loggingConfig` one case puts.
fn logging_config(case: &Case) -> Value {
    let mut config = json!({
        "cloudWatchConfig": {
            "logGroupName": case.log_group,
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/{DELIVERY_ROLE}"),
        },
    });
    if let Some(enabled) = case.text_data_delivery_enabled {
        config["textDataDeliveryEnabled"] = json!(enabled);
    }
    config
}

/// RULE: an admitted logging configuration naming a log group destination is the
/// destination every invocation served after it puts its own record in, as one log event of
/// that group — and the configuration's per-datatype switches decide only how much of each
/// record travels, never whether one arrives.
#[tokio::test]
async fn an_installed_cloud_watch_destination_is_delivered_to_by_every_invocation() {
    for case in cases() {
        let name = case.case;
        let emu = emulator_with_inference_logs_and_iam();
        given_log_group(&emu, name, case.log_group).await;
        given_role(&emu.engine, DELIVERY_ROLE, Some(&trust(None)));

        let put = emu
            .call(
                "PUT",
                LOGGING_URI,
                Some(json!({ "loggingConfig": logging_config(&case) })),
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case {name:?}: the configuration must be admitted, got {} {:?}",
            put.status, put.body
        );
        // The configuration an operator reads back, which is what makes the delivery below
        // the thing that configuration promised.
        let echoed = emu.call("GET", LOGGING_URI, None).await;
        assert_eq!(
            (
                echoed.status,
                echoed.body["loggingConfig"]["cloudWatchConfig"]["logGroupName"].as_str(),
            ),
            (200, Some(case.log_group)),
            "case {name:?}: the stored configuration must be echoed back, got {} {:?}",
            echoed.status,
            echoed.body
        );

        // A prompt whose text is this case's own, so a record carrying it cannot be one
        // delivered for another call.
        let prompt = format!("a prompt served under {name}");
        let answered = converse(&emu, name, &prompt).await;

        let records = delivered_records(&emu, name, case.log_group).await;
        assert_eq!(
            records.len(),
            1,
            "case {name:?}: expected one delivered log event, got {records:?}"
        );
        let record = &records[0];
        assert_eq!(
            (
                record["schemaType"].as_str(),
                record["operation"].as_str(),
                record["modelId"].as_str(),
                record["region"].as_str(),
                record["accountId"].as_str(),
                record["input"]["inputTokenCount"].as_i64(),
                record["output"]["outputTokenCount"].as_i64(),
            ),
            (
                Some(logging::SCHEMA_TYPE),
                Some(logging::OPERATION_CONVERSE),
                Some(MODEL),
                Some(REGION),
                Some(account_id().as_str()),
                answered["usage"]["inputTokens"].as_i64(),
                answered["usage"]["outputTokens"].as_i64(),
            ),
            "case {name:?}: the record must state the call it is of and the accounting that \
             call reported inline, got {record}"
        );
        let carries_the_prompt = record.to_string().contains(&prompt);
        assert_eq!(
            carries_the_prompt, case.bodies_travel,
            "case {name:?}: expected the prompt in the record={}, got {record}",
            case.bodies_travel
        );
    }
}

/// RULE: the delivery stops with the configuration. Once
/// `DeleteModelInvocationLoggingConfiguration` has withdrawn it, an invocation served
/// afterwards puts no event in the group — and what was already delivered stays where it
/// is.
#[tokio::test]
async fn a_withdrawn_cloud_watch_destination_stops_the_events_and_leaves_the_delivered_ones() {
    let case = "the configuration is withdrawn between two identical calls";
    let group = "/aws/bedrock/withdrawn-destination";
    let emu = emulator_with_inference_logs_and_iam();
    given_log_group(&emu, case, group).await;
    given_role(&emu.engine, DELIVERY_ROLE, Some(&trust(None)));

    let put = emu
        .call(
            "PUT",
            LOGGING_URI,
            Some(json!({
                "loggingConfig": {
                    "cloudWatchConfig": {
                        "logGroupName": group,
                        "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/{DELIVERY_ROLE}"),
                    },
                    "textDataDeliveryEnabled": true,
                },
            })),
        )
        .await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: the configuration must be admitted, got {} {:?}",
        put.status, put.body
    );

    converse(&emu, case, "the call before the delete").await;
    let before = delivered_records(&emu, case, group).await;
    assert_eq!(
        before.len(),
        1,
        "case {case:?}: expected the positive control's event, got {before:?}"
    );

    let deleted = emu.call("DELETE", LOGGING_URI, None).await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: the configuration must be removable, got {} {:?}",
        deleted.status, deleted.body
    );

    converse(&emu, case, "the byte-identical call after the delete").await;
    let after = delivered_records(&emu, case, group).await;
    assert_eq!(
        after, before,
        "case {case:?}: expected no further event and the delivered one untouched, got \
         {after:?}"
    );
}

/// RULE: an account that configured no destination has nothing delivered for it, so a log
/// group a caller holds for the purpose stays empty until a configuration points at it.
#[tokio::test]
async fn an_account_with_no_configuration_has_nothing_put_in_its_log_group() {
    let case = "no logging configuration was ever put";
    let group = "/aws/bedrock/unconfigured";
    let emu = emulator_with_inference_logs_and_iam();
    given_log_group(&emu, case, group).await;

    let echoed = emu.call("GET", LOGGING_URI, None).await;
    assert_eq!(
        echoed.status, 200,
        "case {case:?}: the read must be served, got {} {:?}",
        echoed.status, echoed.body
    );

    converse(&emu, case, "an unlogged call").await;
    let records = delivered_records(&emu, case, group).await;
    assert!(
        records.is_empty(),
        "case {case:?}: expected nothing delivered, got {records:?}"
    );
}
