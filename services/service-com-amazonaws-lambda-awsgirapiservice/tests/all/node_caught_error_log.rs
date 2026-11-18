// SPDX-License-Identifier: CC0-1.0

//! What a node handler that CAUGHT its own error leaves in the function's log group.
//!
//! RULE: a managed `nodejs` runtime words an error class ONE way — the wording the JavaScript
//! engine such a runtime embeds states for it, which
//! [`crate::node_handler_error_wording`] owns — and EVERY reading of one error renders that
//! same wording: the error document an invocation that did not catch it answers, the line its
//! trace opens with, and the `stack` and the text form of the error a handler that DID catch
//! it reads. Catching an error is not a function error, so such an invocation answers the
//! handler's own value with no `X-Amz-Function-Error` at all, and whatever the handler wrote
//! to the console is delivered to the function's log group — so a client searching that group
//! for the property that was read, or for the frame that raised, finds the run that caught it.
//!
//! That is the only record of a caught failure there is: the response carries the handler's
//! own answer and says nothing about what went wrong inside it, so a client debugging one
//! reads the group — and finds the run only if the text there is worded the way the runtime
//! words that class everywhere else. What the handler reported in ONE call is one event of
//! that group, the line breaks of a stack included, so the wording and the frame that raised
//! are read together.
//!
//! The rows differ along what the rule generalises over: the OPERATION that raised (a property
//! read and a property write), the VALUE it was performed on (`undefined` and `null`), the
//! PROPERTY NAME (a plain one and one reached through a computed key), the CLASS of the error
//! (one the engine raised and one the handler threw), and HOW the handler rendered what it
//! caught (the error itself, its `stack`, its text form, and that text inside a sentence of
//! the handler's own).
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine, and reads the delivery back through
//! CloudWatch Logs' own group-wide search (`FilterLogEvents`), so nothing that did not reach
//! that store can pass.
//!
//! Every row RUNS a handler, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};

use crate::invocation_log_delivery::{
    create_function, emulator, invocation_request, searched, Created, FUNCTION_ERROR_HEADER,
    NODE_RUNTIME,
};
use crate::node_handler_error_wording::ROLE;

/// What every handler below answers once it has caught its own error: its own envelope, which
/// is the value the invocation has to report as the payload.
fn handler_answer() -> Value {
    json!({ "statusCode": 500, "body": "Internal server error" })
}

/// One row: a handler that raises inside a call of its own, catches it and writes it to the
/// console, and the text the function's log group then has to hold.
struct Row {
    case: &'static str,
    /// The function's name, which is also the default log group's last segment.
    name: &'static str,
    /// The handler's source: a module whose `handler` catches what one of its own functions
    /// raised, writes it, and answers [`handler_answer`].
    source: &'static str,
    /// The message every reader of that error reports.
    message: &'static str,
    /// The function that raised, as the frames of the error name it — `None` for a row whose
    /// rendering carries no frames at all.
    frame: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a property read on undefined, written as the error itself",
        name: "node-caught-reads-undefined",
        source: "function readsTheDomain(event) {\n\
                 \x20 return event.requestContext.domainName.replace('wss://', 'https://');\n\
                 }\n\
                 exports.handler = async (event) => {\n\
                 \x20 try {\n\
                 \x20   return { statusCode: 200, body: readsTheDomain(event) };\n\
                 \x20 } catch (error) {\n\
                 \x20   console.error('the handler could not answer', error);\n\
                 \x20   return { statusCode: 500, body: 'Internal server error' };\n\
                 \x20 }\n\
                 };\n",
        message: "Cannot read properties of undefined (reading 'domainName')",
        frame: Some("readsTheDomain"),
    },
    Row {
        case: "a property read on null, written as the error's stack",
        name: "node-caught-reads-null",
        source: "function readsTheName(held) {\n\
                 \x20 return held.replace('a', 'b');\n\
                 }\n\
                 exports.handler = async () => {\n\
                 \x20 try {\n\
                 \x20   return { statusCode: 200, body: readsTheName(null) };\n\
                 \x20 } catch (error) {\n\
                 \x20   console.error(error.stack);\n\
                 \x20   return { statusCode: 500, body: 'Internal server error' };\n\
                 \x20 }\n\
                 };\n",
        message: "Cannot read properties of null (reading 'replace')",
        frame: Some("readsTheName"),
    },
    Row {
        case: "a property read through a computed key",
        name: "node-caught-reads-computed",
        source: "function readsTheRoute(event) {\n\
                 \x20 const key = 'routeKey';\n\
                 \x20 return event.requestContext[key].trim();\n\
                 }\n\
                 exports.handler = async (event) => {\n\
                 \x20 try {\n\
                 \x20   return { statusCode: 200, body: readsTheRoute(event) };\n\
                 \x20 } catch (error) {\n\
                 \x20   console.error(error);\n\
                 \x20   return { statusCode: 500, body: 'Internal server error' };\n\
                 \x20 }\n\
                 };\n",
        message: "Cannot read properties of undefined (reading 'routeKey')",
        frame: Some("readsTheRoute"),
    },
    Row {
        case: "a property write on undefined, written as the error's text form",
        name: "node-caught-writes-undefined",
        source: "function stampsTheStage(event) {\n\
                 \x20 event.requestContext.stage = 'prod';\n\
                 \x20 return 'stamped';\n\
                 }\n\
                 exports.handler = async (event) => {\n\
                 \x20 try {\n\
                 \x20   return { statusCode: 200, body: stampsTheStage(event) };\n\
                 \x20 } catch (error) {\n\
                 \x20   console.error('refused: ' + String(error));\n\
                 \x20   return { statusCode: 500, body: 'Internal server error' };\n\
                 \x20 }\n\
                 };\n",
        message: "Cannot set properties of undefined (setting 'stage')",
        frame: None,
    },
    Row {
        case: "an error the handler threw itself, written inside a sentence of its own",
        name: "node-caught-throws-its-own",
        source: "function checksTheAction(event) {\n\
                 \x20 throw new Error('unsupported action ' + String(event.action));\n\
                 }\n\
                 exports.handler = async (event) => {\n\
                 \x20 try {\n\
                 \x20   return { statusCode: 200, body: checksTheAction(event) };\n\
                 \x20 } catch (error) {\n\
                 \x20   console.error(`the handler gave up: ${error}`);\n\
                 \x20   return { statusCode: 500, body: 'Internal server error' };\n\
                 \x20 }\n\
                 };\n",
        message: "Error: unsupported action undefined",
        frame: None,
    },
];

/// A caught error is a successful invocation whose log group holds the runtime's own wording
/// of what was caught, beside the frame that raised it.
#[tokio::test]
async fn a_caught_error_is_no_function_error_and_its_text_reaches_the_log_group() {
    for row in ROWS {
        let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
        let case = row.case;
        let router = emulator();
        create_function(
            &router,
            &Created {
                case,
                name: row.name,
                source: row.source,
                runtime: NODE_RUNTIME,
                role: ROLE,
                log_group: None,
            },
        )
        .await;

        // The event carries none of the members the handlers above read, which is what makes
        // each of them raise inside its own call.
        let response = router
            .dispatch(invocation_request(row.name, &[]))
            .await;
        let (parts, body) = response.into_parts();
        let payload: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        assert_eq!(
            parts.status.as_u16(),
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {payload}",
            parts.status
        );
        assert!(
            !parts.headers.contains_key(FUNCTION_ERROR_HEADER),
            "case {case:?}: a handler that caught its own error reports no function error, got \
             {:?}",
            parts.headers.get(FUNCTION_ERROR_HEADER)
        );
        assert_eq!(
            payload,
            handler_answer(),
            "case {case:?}: expected the handler's own envelope as the payload, got {payload}"
        );

        let group = format!("/aws/lambda/{}", row.name);
        let held = searched(&router, case, &group).await;
        assert!(
            held.iter().any(|message| message.contains(row.message)),
            "case {case:?}: expected an event of {group:?} carrying {expected:?}, got {held:?}",
            expected = row.message
        );
        // Where the handler wrote the error's FRAMES, ONE event carries both the wording and
        // the function that raised: what the handler reported in one call is one record of the
        // log, line breaks of its own included, which is the event a client debugging the run
        // reads.
        if let Some(frame) = row.frame {
            assert!(
                held.iter()
                    .any(|message| message.contains(row.message) && message.contains(frame)),
                "case {case:?}: expected ONE event of {group:?} carrying {expected:?} and the \
                 frame {frame:?} that raised, got {held:?}",
                expected = row.message
            );
        }
    }
}
