Issue-Source: trace-sweep-divergence
Issue-Trace: neptunedata-query-languages-live-cluster-round-trip
Issue-Step: task#23
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: neptunedata-query-languages-live-cluster-round-trip
Issue-Shape-Id: com.amazonaws.neptunedata#AmazonNeptuneDataplane
Issue-Key: neptunedata|ExecuteOpenCypherQuery|4xx->4xx|$.code
Issue-Key: neptunedata|ExecuteOpenCypherQuery|4xx->4xx|$.detailedMessage
Issue-Key: neptunedata|ExecuteOpenCypherQuery|4xx->4xx|$.message
Issue-Key: neptunedata|ExecuteOpenCypherQuery|4xx->4xx|$.requestId
Issue-Commit: 237516c3db13377f1407f4f915feb2879a07798e

# neptunedata ExecuteOpenCypherQuery malformed-query refusal body diverges: code carries the HTTP status, no message member, non-Neptune parser text

> _Note: this issue was identified against the code base at commit `237516c3db13377f1407f4f915feb2879a07798e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteOpenCypherQuery

## Description
ONE root cause, observed on the implemented ExecuteOpenCypherQuery refusal path: the emulator refuses a syntactically invalid openCypher query with the right HTTP status (400) but serves a modelled-error BODY that no Neptune client can read — the `code` member carries the numeric HTTP status instead of the exception class, the client-visible `message` member is absent altogether, and the `detailedMessage` text is the emulator's own parser wording rather than the openCypher parser message template Neptune returns. This was reached against a RUNNING, serving emulator (31 of 43 steps of the same fixture passed and this very step answered 400), so it is not the open unreachability item 20261109T113928Z-neptunedata-is-still-entirely-unreachable-the-neptune-db.md.

Cause 1 of 1: a MalformedQueryException body from ExecuteOpenCypherQuery is classified and worded unlike Neptune's.
AWS: Smithy (com.amazonaws.neptunedata) models ExecuteOpenCypherQuery as POST /opencypher (success 200) with MalformedQueryException among its errors (errorKind client, httpError 400, "Raised when a query is submitted that is syntactically incorrect or does not pass additional validation"); the shape's required members are detailedMessage ("A detailed message describing the problem"), requestId and code. Recorded behavior shows how Neptune fills that body for this error class: `code` is the EXCEPTION CLASS NAME — the fixed string "MalformedQueryException" — not the numeric status (despite the member's prose "The HTTP status code returned with the exception"); a `message` member is ALSO present on the wire, carrying the SAME text as detailedMessage, and that is the member a client (botocore's Error.Message) surfaces; and that text is the openCypher parser's own diagnostic, whose template is "Invalid input '<the offending character>': expected '<the alternatives the parser expected>' (line <line>, column <column> (offset: <0-based offset>))" — the fixed parts being "Invalid input '", "': expected '", "' (line ", ", column ", " (offset: ", "))", and every variable part derived from the submitted query text. The recording's "Invalid input ' ': expected 'e/E' (line 1, column 6 (offset: 5))" is ONE filling of that template for one query, not the requirement. requestId is a per-run value and is not asserted.
Emulator: for the same request it answers 400 with code "400", NO message member at all, and detailedMessage "Failed to interpret openCypher query: expected a clause, found identifier at 0..5" — i.e. the emulator's own parse-failure wording with a byte-range ("at 0..5") in place of Neptune's line/column/offset triple and the expected-alternatives list. The classification itself (MalformedQueryException, 400) and the x-amzn-errortype header are correct; only the body is wrong. In the checkout, every modelled-error payload builder in services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/state.rs hard-codes `code: "400".to_string()` (see invalid_query_id:693-699, invalid_parameter_range:707-721, missing_parameter:726-732, invalid_fast_reset_token:740-747, unsupported_action:751-761) and none of them populate a message member, so this is a shared serialization/payload defect reached through this operation's refusal() mapping (src/ops/execute_open_cypher_query.rs:359-378, which routes QueryFailure::Malformed/Refused to crate::state::malformed_open_cypher_query).
Why: a client that branches on the error class as Neptune reports it in the body sees the string "400" and cannot tell a malformed query from any other 400-class refusal, and every client-side diagnostic, log line and message-matching branch that reads Error.Message gets an empty string. A developer fixing a bad query also loses the line/column/offset and the expected-token list, which is the only actionable part of Neptune's refusal.
Fix: the neptunedata modelled-error payload builders plus the error serializer of this crate (state.rs — `code` must carry the exception class name and the serialized body must also emit the `message` member AWS populates alongside detailedMessage), and the openCypher parse-failure message construction behind crate::state::malformed_open_cypher_query, which must render Neptune's "Invalid input '<char>': expected '<alternatives>' (line <l>, column <c> (offset: <o>))" template from the submitted query. Because the same builders back the InvalidParameterException / MissingParameterException / IllegalArgumentException refusals of the sibling operations, the code/message half of this fix belongs in that shared layer, not in this one handler.
Related but NOT covering this: 20261115T144528Z-neptunedata-refusal-paths-diverge-ml-opencypher-status-cancel.md (its Cause 5 describes ExecuteOpenCypherQuery answering 501 NotImplemented as a scaffold stub, which the checkout no longer does — the handler is implemented and now diverges in the body it serves) and 20261109T174758Z-neptunedata-data-plane-responses-diverge-from-neptune-engine.md (engine-status payload, content-type and fast-reset refusal). The replay marked neither as owning this step.
Traces: neptunedata-query-languages-live-cluster-round-trip#task#23.

## Evidence
Fixture neptunedata-query-languages-live-cluster-round-trip, step task#23, ExecuteOpenCypherQuery, verdict mismatch, expected_status 400 / actual_status 400.

Per-field divergence as replayed:
- $.code: expected 'MalformedQueryException' (the exception class name, fixed text), got '400'.
- $.message: expected present and equal to the parser diagnostic "Invalid input ' ': expected 'e/E' (line 1, column 6 (offset: 5))" — fixed text "Invalid input '" / "': expected '" / "' (line " / ", column " / " (offset: " / "))" with the offending character, expected alternatives, line, column and offset taken from the submitted query — got None (member absent from the body).
- $.detailedMessage: expected the same parser diagnostic, got 'Failed to interpret openCypher query: expected a clause, found identifier at 0..5'.
- $.requestId: shaped-not-asserted (per-run value, NOT part of this finding).

Status 400 and the x-amzn-errortype classification matched; only the body members above diverge.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture 'neptunedata-query-languages-live-cluster-round-trip' (replay_wire_corpus), or reproduce by hand:
2. Point a neptunedata client at the emulator's data listener.
3. POST /opencypher with a syntactically invalid openCypher query (the fixture submits a misspelled leading clause, e.g. execute_open_cypher_query(openCypherQuery="CREAT (n:`<Label>`) SET n.x = ")).
4. Inspect the raw 400 response body: today `code` is "400", there is no `message` member, and `detailedMessage` reads 'Failed to interpret openCypher query: ... at 0..5'. Neptune returns code "MalformedQueryException" plus message == detailedMessage == "Invalid input '<char>': expected '<alternatives>' (line <l>, column <c> (offset: <o>))" computed from the submitted query.
5. Confirm the fix by re-replaying fixture neptunedata-query-languages-live-cluster-round-trip step task#23.
