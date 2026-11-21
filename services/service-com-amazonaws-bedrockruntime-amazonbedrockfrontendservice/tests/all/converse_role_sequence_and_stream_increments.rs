// SPDX-License-Identifier: CC0-1.0

//! Two rules of the Converse API, end to end over HTTP: which conversations the
//! service will answer at all, and how a streamed turn's content reaches a
//! client.
//!
//! * the OPENING TURN of a conversation is a precondition of being served: a
//!   conversation whose first `Message.role` is `user` is answered whatever
//!   sequence the roles after it form, and one that opens on any other
//!   `ConversationRole` value — or carries no turn at all — is refused;
//! * that precondition is decided over the SUBMITTED conversation alone, before
//!   the turn is generated and before anything about the call is recorded, so the
//!   refusal carries the same sentence whatever else the request binds and the
//!   refused call leaves the service's inference log untouched — asserted over
//!   the one refusal table the rule above is asserted over;
//! * a streamed content block arrives as the INCREMENTS a client appends: several
//!   `contentBlockDelta` events, each carrying that block's own
//!   `contentBlockIndex`, which reassemble to exactly what `Converse` answers
//!   with for the same request.
//!
//! Every case drives the real router through the harness the other suites of this
//! target use, so what is asserted is the wire behaviour a client sees. Each rule
//! is asserted over a SET of inputs — the tables differ along what the rule
//! generalises over (the roles, their order, how many turns there are, the model
//! family serving the turn, how long the content is, the generation cap, and
//! whether the block carries text or a tool request) — so no row is the
//! specification on its own.

use serde_json::{json, Value};

use crate::bedrock_runtime_inference::{emulator, emulator_with_engine, post_json};
use crate::bedrock_runtime_streaming::decode_frames;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::ComAmazonawsBedrockruntimeAmazonbedrockfrontendserviceState as ServiceState;

/// The modelled `StopReason` values, which is what a served turn has to end on
/// (smithy_query(query="shape", shape="StopReason")).
const STOP_REASONS: [&str; 9] = [
    "end_turn",
    "tool_use",
    "max_tokens",
    "stop_sequence",
    "guardrail_intervened",
    "content_filtered",
    "malformed_model_output",
    "malformed_tool_use",
    "model_context_window_exceeded",
];

/// One turn of a conversation, by the role it plays.
fn turn(role: &str, text: &str) -> Value {
    json!({ "role": role, "content": [{"text": text}] })
}

/// One role sequence, and the model it is sent to.
struct SequenceRow {
    case: &'static str,
    /// A model the registry serves and the Converse API supports; the rows span
    /// several response families, which the sequence rule generalises over.
    model_id: &'static str,
    messages: Value,
}

/// The role-sequence table: every row opens on a `user` turn, as the opening-turn
/// rule requires, and the rows differ along how often a role repeats after that
/// opening turn, how many turns there are and which `ConversationRole` value a
/// later turn carries — the enum's three values are `user`, `assistant` and
/// `system`.
fn sequence_rows() -> Vec<SequenceRow> {
    vec![
        SequenceRow {
            case: "the alternating sequence a chat client usually sends",
            model_id: "amazon.nova-lite-v1:0",
            messages: json!([
                turn("user", "What is a page token?"),
                turn("assistant", "An opaque cursor."),
                turn("user", "Why opaque?"),
            ]),
        },
        SequenceRow {
            case: "two user turns before the assistant has answered",
            model_id: "moonshotai.kimi-k2.5",
            messages: json!([turn("user", "first"), turn("user", "second")]),
        },
        SequenceRow {
            case: "a pending user turn re-appended by a retry",
            model_id: "anthropic.claude-3-haiku-20240307-v1:0",
            messages: json!([
                turn("user", "Name one planet."),
                turn("assistant", "Mars."),
                turn("user", "And one moon?"),
                turn("user", "And one moon?"),
            ]),
        },
        SequenceRow {
            case: "four user turns in a row",
            model_id: "meta.llama4-scout-17b-instruct-v1:0",
            messages: json!([
                turn("user", "one"),
                turn("user", "two"),
                turn("user", "three"),
                turn("user", "four"),
            ]),
        },
        SequenceRow {
            case: "a single user turn and nothing else",
            model_id: "amazon.nova-pro-v1:0",
            messages: json!([turn("user", "Shall I continue?")]),
        },
        SequenceRow {
            case: "an assistant turn repeated",
            model_id: "mistral.mistral-7b-instruct-v0:2",
            messages: json!([
                turn("user", "List two primes."),
                turn("assistant", "2"),
                turn("assistant", "3"),
            ]),
        },
        SequenceRow {
            case: "a conversation that ends on the assistant",
            model_id: "cohere.command-r-plus-v1:0",
            messages: json!([turn("user", "hi"), turn("assistant", "hello")]),
        },
        SequenceRow {
            case: "a system-role turn inside the conversation",
            model_id: "amazon.nova-lite-v1:0",
            messages: json!([
                turn("user", "Why are cursors opaque?"),
                turn("system", "Answer in one sentence."),
            ]),
        },
    ]
}

/// RULE: once a conversation opens on a `user` turn, the ORDER of the roles that
/// follow is not a precondition of serving it — the service answers whatever
/// sequence those `Message.role` values form, repeated roles included — so what a
/// turn reports is the assistant's own message, a modelled `stopReason` and the
/// usage counted for the request.
#[tokio::test]
async fn a_conversation_is_served_whatever_order_its_roles_appear_in() {
    let router = emulator();

    for row in sequence_rows() {
        let case = row.case;
        let request = json!({ "messages": row.messages });
        let call = post_json(
            &router,
            &format!("/model/{}/converse", row.model_id),
            &request,
        )
        .await;
        assert_eq!(
            call.status,
            200,
            "case {case:?}: expected the conversation to be served, got {} / {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        let body = call.json();
        assert_eq!(
            body["output"]["message"]["role"].as_str(),
            Some("assistant"),
            "case {case:?}: expected an assistant message, got {body}"
        );
        let text = body["output"]["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the turn answered no text: {body}"));
        assert!(
            !text.is_empty(),
            "case {case:?}: expected the turn to answer content, got {body}"
        );
        let stop_reason = body["stopReason"].as_str().unwrap_or_default();
        assert!(
            STOP_REASONS.contains(&stop_reason),
            "case {case:?}: expected a modelled stopReason, got {stop_reason:?}"
        );
        // The accounting is the request's own: the prompt that was read, the
        // content that was served, and their sum.
        let input = body["usage"]["inputTokens"].as_i64().unwrap_or_default();
        let output = body["usage"]["outputTokens"].as_i64().unwrap_or_default();
        assert!(
            input > 0 && output > 0,
            "case {case:?}: expected both token counts to be counted, got {}",
            body["usage"]
        );
        assert_eq!(
            body["usage"]["totalTokens"].as_i64(),
            Some(input + output),
            "case {case:?}: expected totalTokens to close over both counts, got {}",
            body["usage"]
        );
        assert_eq!(
            output,
            i64::from(state::count_text_tokens(text)),
            "case {case:?}: expected outputTokens to be the tokens of {text:?}, got {output}"
        );

        // The streaming surface takes the same conversation: the two cannot
        // disagree about which sequences are servable.
        let stream = post_json(
            &router,
            &format!("/model/{}/converse-stream", row.model_id),
            &request,
        )
        .await;
        assert_eq!(
            stream.status,
            200,
            "case {case:?}: expected ConverseStream to serve it too, got {} / {}",
            stream.status,
            String::from_utf8_lossy(&stream.body)
        );
        assert_eq!(
            streamed_text(case, &stream.body),
            text,
            "case {case:?}: expected the stream to frame the turn Converse serves"
        );
    }
}

/// One conversation that does not open on a `user` turn, with the rest of the
/// request a client submitted it inside.
struct OpeningRefusalRow {
    case: &'static str,
    /// A model the registry serves the Converse API for; the rows span several
    /// model families, which the opening-turn rule generalises over.
    model_id: &'static str,
    request: Value,
}

/// The ONE opening-turn refusal table, which both tests of that rule below read:
/// every row submits a conversation whose FIRST turn is not a `user` turn, and
/// the rows differ along which other `ConversationRole` value opens it, whether
/// it carries a turn at all, how many turns follow, what kind of `ContentBlock`
/// that opening turn carries, which model family serves it, and how much of the
/// rest of the request is bound — a tool configuration obliging a tool, a
/// guardrail configuration, system blocks, an inference configuration — each of
/// which is a stage the request would otherwise be worked on by.
fn opening_refusal_rows() -> Vec<OpeningRefusalRow> {
    vec![
        OpeningRefusalRow {
            case: "no messages member at all",
            model_id: "amazon.nova-lite-v1:0",
            request: json!({}),
        },
        OpeningRefusalRow {
            case: "an empty conversation",
            model_id: "moonshotai.kimi-k2.5",
            request: json!({ "messages": [] }),
        },
        OpeningRefusalRow {
            case: "an empty conversation beside an inference configuration",
            model_id: "anthropic.claude-3-haiku-20240307-v1:0",
            request: json!({ "messages": [], "inferenceConfig": {"maxTokens": 32} }),
        },
        OpeningRefusalRow {
            case: "a single assistant turn and nothing else",
            model_id: "amazon.nova-pro-v1:0",
            request: json!({ "messages": [turn("assistant", "hi")] }),
        },
        OpeningRefusalRow {
            case: "a conversation that opens on the assistant",
            model_id: "meta.llama4-scout-17b-instruct-v1:0",
            request: json!({ "messages": [
                turn("assistant", "Shall I continue?"),
                turn("user", "Yes."),
            ] }),
        },
        OpeningRefusalRow {
            case: "a conversation that opens on a system turn",
            model_id: "mistral.mistral-7b-instruct-v0:2",
            request: json!({ "messages": [
                turn("system", "Answer in one sentence."),
                turn("user", "Why are cursors opaque?"),
            ] }),
        },
        OpeningRefusalRow {
            case: "an assistant turn opening a request that obliges a tool",
            model_id: "amazon.nova-lite-v1:0",
            request: json!({
                "messages": [turn("assistant", "Shall I look it up?")],
                "toolConfig": {
                    "tools": [tool_spec("get_stock_price")],
                    "toolChoice": {"any": {}},
                },
            }),
        },
        OpeningRefusalRow {
            case: "an assistant turn opening a request that names a guardrail this \
                   account does not hold",
            model_id: "anthropic.claude-3-haiku-20240307-v1:0",
            request: json!({
                "messages": [turn("assistant", "hello"), turn("user", "hi")],
                "guardrailConfig": {
                    "guardrailIdentifier": "abcd1234efgh",
                    "guardrailVersion": "DRAFT",
                    "trace": "enabled",
                },
            }),
        },
        OpeningRefusalRow {
            case: "an opening turn whose content block is a tool result nothing issued",
            model_id: "meta.llama4-scout-17b-instruct-v1:0",
            request: json!({ "messages": [{
                "role": "assistant",
                "content": [{"toolResult": {
                    "toolUseId": "tooluse_emulator000000001",
                    "content": [{"text": "41.5"}],
                }}],
            }] }),
        },
        OpeningRefusalRow {
            case: "a system turn opening a request with system blocks and an \
                   inference configuration",
            model_id: "mistral.mistral-7b-instruct-v0:2",
            request: json!({
                "system": [{"text": "You are terse."}],
                "messages": [
                    turn("system", "Answer in one sentence."),
                    turn("user", "Why are cursors opaque?"),
                ],
                "inferenceConfig": {"maxTokens": 16, "temperature": 0.2},
            }),
        },
        OpeningRefusalRow {
            case: "an opening assistant turn whose content list is empty",
            model_id: "amazon.nova-pro-v1:0",
            request: json!({ "messages": [{"role": "assistant", "content": []}] }),
        },
        OpeningRefusalRow {
            case: "a long conversation that opens on the assistant",
            model_id: "cohere.command-r-plus-v1:0",
            request: json!({ "messages": [
                turn("assistant", "Mars."),
                turn("user", "And one moon?"),
                turn("assistant", "Phobos."),
                turn("user", "And one star?"),
                turn("assistant", "Sol."),
            ] }),
        },
        OpeningRefusalRow {
            case: "no messages member at all beside a tool configuration",
            model_id: "moonshotai.kimi-k2.5",
            request: json!({ "toolConfig": {"tools": [tool_spec("get_stock_price")]} }),
        },
    ]
}

/// RULE: a conversation that does not open on a `user` turn — one with no turn at
/// all, or one whose leading `Message.role` is another `ConversationRole` value —
/// is refused `400 ValidationException` carrying the service's one fixed
/// sentence, the same on both Converse surfaces and whatever else the request
/// binds.
#[tokio::test]
async fn a_conversation_that_does_not_open_on_a_user_turn_is_refused() {
    let router = emulator();

    for row in opening_refusal_rows() {
        let case = row.case;
        for surface in ["converse", "converse-stream"] {
            let call = post_json(
                &router,
                &format!("/model/{}/{surface}", row.model_id),
                &row.request,
            )
            .await;
            assert_eq!(
                call.status,
                400,
                "case {case:?} on {surface}: expected 400, got {} / {}",
                call.status,
                String::from_utf8_lossy(&call.body)
            );
            // The wire error identity a client branches on, which restJson1
            // reports on `x-amzn-errortype`.
            assert_eq!(
                call.error_code(),
                "ValidationException",
                "case {case:?} on {surface}: expected ValidationException, got {:?}",
                call.error_code()
            );
            let body = call.json();
            let message = body["message"].as_str().unwrap_or_default();
            assert_eq!(
                message,
                state::NO_OPENING_USER_TURN,
                "case {case:?} on {surface}: expected the opening-turn refusal, got {body}"
            );
            // The sentence is FIXED: nothing in it is filled from the request, so
            // it names neither the offending turn's path nor the model the
            // request was sent to.
            assert!(
                !message.contains("messages.") && !message.contains(row.model_id),
                "case {case:?} on {surface}: the refusal names no part of the request, got \
                 {message:?}"
            );
        }
    }
}

/// RULE: that precondition is decided over the submitted conversation alone,
/// BEFORE the turn is generated and before anything about the call is recorded —
/// so none of the conversations the table above is refused for contributes to the
/// log every served inference appends to.
#[tokio::test]
async fn a_refused_conversation_is_generated_for_nothing_and_recorded_nowhere() {
    let (engine, router) = emulator_with_engine();

    for row in opening_refusal_rows() {
        let case = row.case;
        for surface in ["converse", "converse-stream"] {
            let call = post_json(
                &router,
                &format!("/model/{}/{surface}", row.model_id),
                &row.request,
            )
            .await;
            assert_eq!(
                call.status,
                400,
                "case {case:?} on {surface}: expected the conversation to be refused, got {} / {}",
                call.status,
                String::from_utf8_lossy(&call.body)
            );
        }
    }

    // Nothing was generated and nothing was recorded: a refused conversation is
    // not an inference, so it is absent from the log a served call appends to.
    let logged = engine.state::<ServiceState>().lock().invocations.clone();
    assert!(
        logged.is_empty(),
        "expected a refused conversation to be recorded nowhere, got {logged:?}"
    );

    // And that log is where a SERVED conversation lands, so the emptiness above
    // is the refusal's doing rather than a log nothing ever reaches.
    let served = post_json(
        &router,
        "/model/amazon.nova-lite-v1:0/converse",
        &json!({ "messages": [turn("user", "Name one planet.")] }),
    )
    .await;
    assert_eq!(
        served.status,
        200,
        "the same emulator serves a conversation that opens on a user turn, got {} / {}",
        served.status,
        String::from_utf8_lossy(&served.body)
    );
    let logged = engine.state::<ServiceState>().lock().invocations.clone();
    assert_eq!(
        logged.len(),
        1,
        "expected the served conversation to be the one logged call, got {logged:?}"
    );
}

/// One streamed turn: the request, and where the content block it delivers is
/// read from the unary answer.
struct IncrementRow {
    case: &'static str,
    model_id: &'static str,
    request: Value,
    /// `true` for a turn that requests a tool, whose block carries the tool's
    /// input document instead of text.
    tool_use: bool,
}

/// One tool definition, as `ToolSpecification` models it.
fn tool_spec(name: &str) -> Value {
    json!({"toolSpec": {
        "name": name,
        "description": format!("Look up {name}."),
        "inputSchema": {"json": {
            "type": "object",
            "properties": {"symbol": {"type": "string"}},
            "required": ["symbol"],
        }},
    }})
}

/// The increment table: the rows differ along the LENGTH of the content a block
/// carries (a generous budget, a modest one, a budget that leaves a single
/// increment), the model family framing it, and whether the block is text or a
/// tool request.
fn increment_rows() -> Vec<IncrementRow> {
    let ask = |text: &str| json!([turn("user", text)]);
    vec![
        IncrementRow {
            case: "a turn under a generous budget",
            model_id: "amazon.nova-lite-v1:0",
            request: json!({ "messages": ask("Explain idempotency.") }),
            tool_use: false,
        },
        IncrementRow {
            case: "a turn whose prompt is long",
            model_id: "anthropic.claude-3-haiku-20240307-v1:0",
            request: json!({ "messages": ask(&"word ".repeat(200)) }),
            tool_use: false,
        },
        IncrementRow {
            case: "a turn under a modest budget",
            model_id: "meta.llama4-scout-17b-instruct-v1:0",
            request: json!({
                "messages": ask("Name one planet."),
                "inferenceConfig": {"maxTokens": 12},
            }),
            tool_use: false,
        },
        IncrementRow {
            case: "a turn whose budget leaves a single increment",
            model_id: "amazon.nova-lite-v1:0",
            request: json!({
                "messages": ask("Name one planet."),
                "inferenceConfig": {"maxTokens": 3},
            }),
            tool_use: false,
        },
        IncrementRow {
            case: "a turn halted at a stop sequence",
            model_id: "moonshotai.kimi-k2.5",
            request: json!({
                "messages": ask("Name one planet."),
                "inferenceConfig": {"stopSequences": ["request"]},
            }),
            tool_use: false,
        },
        IncrementRow {
            case: "a turn obliged to request a tool",
            model_id: "amazon.nova-lite-v1:0",
            request: json!({
                "messages": ask("What is the price of the symbol in my note?"),
                "toolConfig": {
                    "tools": [tool_spec("get_stock_price")],
                    "toolChoice": {"any": {}},
                },
            }),
            tool_use: true,
        },
    ]
}

/// RULE: a streamed content block arrives as the INCREMENTS a client appends —
/// each `contentBlockDelta` carries a piece of the block and the block's own
/// `contentBlockIndex`, the pieces reassemble to exactly what `Converse` answers
/// with for the same request, and a block longer than one increment is delivered
/// in several of them rather than whole in the first.
#[tokio::test]
async fn a_streamed_block_arrives_as_the_increments_that_reassemble_to_it() {
    let router = emulator();

    for row in increment_rows() {
        let case = row.case;
        let converse = post_json(
            &router,
            &format!("/model/{}/converse", row.model_id),
            &row.request,
        )
        .await;
        assert_eq!(
            converse.status,
            200,
            "case {case:?}: expected Converse to answer 200, got {} / {}",
            converse.status,
            String::from_utf8_lossy(&converse.body)
        );
        let answer = converse.json();
        // What the one block of this turn carries: its text, or the JSON document
        // a tool request hands the tool.
        let whole = if row.tool_use {
            let tool_use = answer["output"]["message"]["content"][0]["toolUse"].clone();
            assert!(
                tool_use.is_object(),
                "case {case:?}: expected a toolUse block, got {answer}"
            );
            tool_use["input"].to_string()
        } else {
            answer["output"]["message"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: the turn answered no text: {answer}"))
                .to_string()
        };

        let stream = post_json(
            &router,
            &format!("/model/{}/converse-stream", row.model_id),
            &row.request,
        )
        .await;
        assert_eq!(
            stream.status,
            200,
            "case {case:?}: expected ConverseStream to answer 200, got {} / {}",
            stream.status,
            String::from_utf8_lossy(&stream.body)
        );
        let frames = decode_frames(case, &stream.body);
        let member = if row.tool_use { "toolUse" } else { "text" };
        let pieces: Vec<String> = frames
            .iter()
            .filter(|(name, _)| name == "contentBlockDelta")
            .map(|(_, payload)| {
                let piece = if row.tool_use {
                    payload["delta"]["toolUse"]["input"].as_str()
                } else {
                    payload["delta"]["text"].as_str()
                };
                piece.unwrap_or_else(|| {
                    panic!("case {case:?}: a delta carried no {member} piece: {payload}")
                })
                .to_string()
            })
            .collect();

        assert_eq!(
            pieces.concat(),
            whole,
            "case {case:?}: expected the deltas to reassemble to {whole:?}, got {pieces:?}"
        );
        for piece in &pieces {
            assert!(
                !piece.is_empty(),
                "case {case:?}: an increment carries content, got {pieces:?}"
            );
        }
        for (_, payload) in frames
            .iter()
            .filter(|(name, _)| name == "contentBlockDelta")
        {
            assert_eq!(
                payload["contentBlockIndex"].as_i64(),
                Some(0),
                "case {case:?}: every delta carries its own block's index, got {payload}"
            );
        }
        // A block of more than one increment's worth of tokens is delivered in
        // several deltas; one that fits inside a single increment, in one.
        let tokens = usize::try_from(state::count_text_tokens(&whole)).unwrap_or(usize::MAX);
        let expected_several = tokens > state::stream_framing::TOKENS_PER_DELTA;
        assert_eq!(
            pieces.len() > 1,
            expected_several,
            "case {case:?}: a block of {tokens} token(s) must arrive as {} delta(s), got \
             {} ({pieces:?})",
            if expected_several { "several" } else { "one" },
            pieces.len()
        );
        assert!(
            pieces.len() <= state::stream_framing::MAX_DELTAS,
            "case {case:?}: one block cannot be framed without limit, got {} deltas",
            pieces.len()
        );
    }
}

/// The text a `ConverseStream` body delivers: its `contentBlockDelta` pieces, in
/// order, reassembled.
fn streamed_text(case: &str, body: &[u8]) -> String {
    decode_frames(case, body)
        .iter()
        .filter(|(name, _)| name == "contentBlockDelta")
        .map(|(_, payload)| {
            payload["delta"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}
