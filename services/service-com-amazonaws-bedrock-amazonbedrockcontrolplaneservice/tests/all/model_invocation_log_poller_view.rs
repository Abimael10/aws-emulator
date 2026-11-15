// SPDX-License-Identifier: CC0-1.0

//! MODEL INVOCATION LOGGING read the way an audit consumer reads it: the destination a
//! logging configuration names is LISTED under the prefix that configuration carried, and
//! each record found there belongs to one invocation that actually ran.
//!
//! The rule asserted here: while a configuration naming an object destination stands, the
//! invocations served under it each deliver their own record below the `keyPrefix` the
//! configuration named — so a consumer that lists the destination by that prefix alone
//! finds them — and a record states the call it is of, which is the one whose request body
//! it carries, whose operation and model it names and whose token counts it repeats.
//!
//! Why that is a rule of its own rather than a second reading of
//! [`crate::model_invocation_log_destination_delivery`]: the configuration is an
//! account-and-region singleton, so a consumer looking for one invocation's record reads a
//! destination that other invocations are delivering into at the same time. "A record was
//! delivered" is therefore weaker than what such a consumer needs, which is that the
//! record it finds for its own call is attributable to that call and no other. The rows
//! differ along the surface that served the invocation, along the prompt each one carried
//! (so their bodies and their accounting differ), along how many records share the
//! destination, and along the form of the prefix the destination was configured with.
//!
//! The three services a record travels through are mounted on one shared engine by the
//! harness this suite borrows from the file above, exactly as `core/router` assembles them.

use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;

use crate::model_invocation_log_destination_delivery::{
    account_id, delivered_keys, emulator, given_bucket, records_of, Emulator, BUCKET, LOGGING_URI,
    MODEL, REGION, RUNTIME_AUTHORIZATION,
};
use crate::support::Reply;

/// Put a configuration delivering into the destination bucket under `key_prefix`, with the
/// prompt and completion bodies travelling — the configuration an operator enables to audit
/// what was prompted and what was generated.
async fn given_logging(emu: &Emulator, case: &str, key_prefix: Option<&str>) {
    let mut s3_config = json!({ "bucketName": BUCKET });
    if let Some(prefix) = key_prefix {
        s3_config["keyPrefix"] = json!(prefix);
    }
    let put = emu
        .bedrock(
            "PUT",
            LOGGING_URI,
            Some(json!({
                "loggingConfig": {
                    "s3Config": s3_config,
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
}

/// One invocation served under the shared configuration.
struct Served {
    case: &'static str,
    /// The path under `/model/<modelId>/` that decides the surface serving it.
    path: &'static str,
    /// The `operation` the record of this call has to name.
    operation: &'static str,
    /// Whether the request body is the Converse-API shape rather than the model family's
    /// own invoke envelope.
    converse_shaped: bool,
    /// Whether the answer reports its accounting in a JSON response body, which is what a
    /// record's token counts can be held against here. The streaming surface reports its
    /// own in a frame of the event stream it answers with instead, so what is required of
    /// its record here is that it carries the accounting of the turn it served.
    accounting_in_body: bool,
    /// The text only this call carries, which is how its record is told apart from the
    /// records of the other calls sharing the destination.
    prompt: &'static str,
}

/// The invocations one destination is shared by: both Converse surfaces and the invoke
/// surface, with prompts of differing content and length so no two calls report the same
/// body or the same accounting by construction.
fn served() -> Vec<Served> {
    vec![
        Served {
            case: "Converse, a one-word prompt",
            path: "converse",
            operation: logging::OPERATION_CONVERSE,
            converse_shaped: true,
            accounting_in_body: true,
            prompt: "greet",
        },
        Served {
            case: "Converse, a long prompt",
            path: "converse",
            operation: logging::OPERATION_CONVERSE,
            converse_shaped: true,
            accounting_in_body: true,
            prompt: "name one primary colour and then say nothing further about painting",
        },
        Served {
            case: "ConverseStream, the streaming turn of the same API",
            path: "converse-stream",
            operation: logging::OPERATION_CONVERSE_STREAM,
            converse_shaped: true,
            accounting_in_body: false,
            prompt: "describe a kettle in a single short sentence",
        },
        Served {
            case: "InvokeModel, the model family's own envelope",
            path: "invoke",
            operation: logging::OPERATION_INVOKE_MODEL,
            converse_shaped: false,
            accounting_in_body: true,
            prompt: "list two citrus fruits",
        },
    ]
}

/// Serve one invocation on its own surface, carrying `prompt`.
async fn serve(emu: &Emulator, served: &Served, prompt: &str) -> Reply {
    let body = if served.converse_shaped {
        json!({ "messages": [{ "role": "user", "content": [{ "text": prompt }] }] })
    } else {
        json!({
            "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
            "inferenceConfig": { "maxTokens": 64 },
        })
    };
    let reply = emu
        .request(
            RUNTIME_AUTHORIZATION,
            "bedrock-runtime",
            "POST",
            &format!("/model/{MODEL}/{}", served.path),
            Some(body),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {:?}: the inference must be served, got {} {:?}",
        served.case, reply.status, reply.body
    );
    reply
}

/// Every record the destination holds under `prefix`, with the key each was delivered
/// under: the whole of what a consumer listing that prefix and fetching what it finds
/// reads.
async fn records_under(emu: &Emulator, case: &str, prefix: &str) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    for key in delivered_keys(emu, case, prefix).await {
        for record in records_of(emu, case, &key).await {
            found.push((key.clone(), record));
        }
    }
    found
}

/// The marker only the invocation of row `index` carries, appended to its prompt so the
/// record OF that call is identifiable by construction rather than by the rows happening to
/// word their prompts differently.
fn marker(index: usize) -> String {
    format!("[request-{index}-of-this-destination]")
}

/// RULE: a consumer that lists a logging destination by the `keyPrefix` the configuration
/// named finds one record per invocation served under it, and the record carrying a call's
/// request body is the one naming that call's operation and model and repeating the token
/// counts that same call reported inline.
#[tokio::test]
async fn each_served_invocation_has_its_own_record_under_the_configured_prefix() {
    /// The prefix this destination is configured with, which is all a consumer of it knows.
    const KEY_PREFIX: &str = "audit/invocations";

    let calls = served();
    let shared = "four invocations sharing one logging configuration";
    let emu = emulator();
    given_bucket(&emu, shared).await;
    given_logging(&emu, shared, Some(KEY_PREFIX)).await;

    // Serve them all against the one configuration, keeping the prompt each call carried and
    // what it reported inline.
    let mut served_prompts: Vec<String> = Vec::new();
    let mut inline: Vec<Option<(i64, i64)>> = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        let prompt = format!("{} {}", call.prompt, marker(index));
        let answered = serve(&emu, call, &prompt).await;
        served_prompts.push(prompt);
        if !call.accounting_in_body {
            inline.push(None);
            continue;
        }
        let counts = (
            answered.body["usage"]["inputTokens"].as_i64(),
            answered.body["usage"]["outputTokens"].as_i64(),
        );
        let (Some(input_tokens), Some(output_tokens)) = counts else {
            panic!(
                "case {:?}: the call must report its accounting inline, got {:?}",
                call.case, answered.body
            );
        };
        inline.push(Some((input_tokens, output_tokens)));
    }

    let found = records_under(&emu, shared, KEY_PREFIX).await;
    assert_eq!(
        found.len(),
        calls.len(),
        "case {shared:?}: expected one record per served invocation, got {:?}",
        found.iter().map(|(key, _)| key).collect::<Vec<_>>()
    );
    let layout = logging::record_key_prefix(Some(KEY_PREFIX), &account_id(), REGION);
    for (key, _) in &found {
        assert!(
            key.starts_with(&layout) && key.ends_with(logging::LOG_FILE_SUFFIX),
            "case {shared:?}: expected the record under {layout:?} as a {} object, got {key:?}",
            logging::LOG_FILE_SUFFIX
        );
    }

    for ((call, counts), prompt) in calls.iter().zip(inline).zip(&served_prompts) {
        let case = call.case;
        // The record OF this call: the one whose logged request carries the marker only this
        // call's prompt had.
        let mine: Vec<&Value> = found
            .iter()
            .map(|(_, record)| record)
            .filter(|record| record["input"]["inputBodyJson"].to_string().contains(prompt))
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "case {case:?}: expected exactly one record carrying this call's request, got \
             {mine:?}"
        );
        let record = mine[0];
        assert_eq!(
            (
                record["operation"].as_str(),
                record["modelId"].as_str(),
                record["region"].as_str(),
                record["accountId"].as_str(),
            ),
            (
                Some(call.operation),
                Some(MODEL),
                Some(REGION),
                Some(account_id().as_str()),
            ),
            "case {case:?}: the record must state the call it is of, got {record}"
        );
        let logged = (
            record["input"]["inputTokenCount"].as_i64(),
            record["output"]["outputTokenCount"].as_i64(),
        );
        match counts {
            Some(reported) => assert_eq!(
                logged,
                (Some(reported.0), Some(reported.1)),
                "case {case:?}: the record must repeat the accounting that call reported \
                 inline ({reported:?}), got {record}"
            ),
            None => assert!(
                logged.0.unwrap_or_default() > 0 && logged.1.unwrap_or_default() > 0,
                "case {case:?}: the record must carry the accounting of the turn it served, \
                 got {record}"
            ),
        }
        assert!(
            !record["output"]["outputBodyJson"].is_null(),
            "case {case:?}: the record must carry the answer the call served, got {record}"
        );
    }
}

/// RULE: the whole layout lands below the `keyPrefix` the configuration named, in the form
/// it named it — so listing the destination by exactly that prefix reaches the records, and
/// a configuration that named no prefix delivers from the bucket's root.
///
/// What a destination's prefix forms make of the KEY is the sibling suite's table
/// ([`crate::model_invocation_log_destination_delivery`], which lists the whole bucket and
/// holds the key against the layout); what is asserted here is that the prefix the CALLER
/// configured is a prefix it can list by, which is how a consumer of a destination reaches
/// records at all, and includes a prefix it wrote with a separator around it.
#[tokio::test]
async fn a_record_is_reachable_by_listing_the_prefix_the_configuration_named() {
    // (case, the configured keyPrefix, the prefix a consumer lists the destination by)
    let rows: [(&str, Option<&str>, &str); 4] = [
        ("a one-segment prefix", Some("audit"), "audit"),
        (
            "a nested prefix",
            Some("audit/bedrock/invocations"),
            "audit/bedrock/invocations",
        ),
        (
            "a prefix written with a trailing separator",
            Some("audit/"),
            "audit/",
        ),
        (
            "no prefix at all, so the consumer lists the bucket itself",
            None,
            "",
        ),
    ];
    for (case, configured, listed) in rows {
        let emu = emulator();
        given_bucket(&emu, case).await;
        given_logging(&emu, case, configured).await;

        // A prompt whose text is this case's own, so a record carrying it cannot be one
        // delivered for another call.
        let prompt = format!("a prompt served under {case}");
        emu.converse(case, &prompt).await;

        let found = records_under(&emu, case, listed).await;
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: expected the record reachable under {listed:?}, got {:?}",
            found.iter().map(|(key, _)| key).collect::<Vec<_>>()
        );
        let (key, record) = &found[0];
        let layout = logging::record_key_prefix(configured, &account_id(), REGION);
        assert!(
            key.starts_with(&layout),
            "case {case:?}: expected the record under {layout:?}, got {key:?}"
        );
        assert!(
            record["input"]["inputBodyJson"]
                .to_string()
                .contains(&prompt),
            "case {case:?}: expected this call's prompt in the record it is of, got {record}"
        );
    }
}
