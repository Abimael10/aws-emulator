Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connectparticipant.complete-attachment-upload-twice-conflict-rejected
Issue-Shape-Id: com.amazonaws.connectparticipant#AmazonConnectParticipantServiceLambda
Issue-Key: connectparticipant.complete-attachment-upload-twice-conflict-rejected|_transcript_count#14|Timeout|time.sleep(3)
Issue-Commit: b0a5c373bacd7e6d8fa6ef741d33eb3219cdfdb1

# GetTranscript reports NextToken as the empty string on the terminal page instead of omitting the member

> _Note: this issue was identified against the code base at commit `b0a5c373bacd7e6d8fa6ef741d33eb3219cdfdb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTranscript

## Description
One net-new root cause, found while replaying the behavioral check connectparticipant.complete-attachment-upload-twice-conflict-rejected, whose transcript walk pages GetTranscript to exhaustion.

Cause 1 of 1: GetTranscript always reports the NextToken member, and on the page that ends a scan it reports it as the EMPTY STRING rather than leaving it out.

AWS: Smithy (services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/smithy-model/com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda.json) binds GetTranscript as @paginated{inputToken: NextToken, outputToken: NextToken, pageSize: MaxResults}, http POST /participant/transcript -> 200, and GetTranscriptResponse$NextToken targets NextToken with @length(min 1, max 1000). A member constrained to a minimum length of 1 cannot carry the empty string, so the only wire-legal way for a paginated AWS response to say "no further page" is to OMIT NextToken — which is exactly the AWS pagination contract the input side of the same token states (GetTranscriptRequest$NextToken is also @length(min 1, max 1000), so the terminal value is not even re-feedable). Recorded behavior only for the terminal page's exact bytes; the model is what states the constraint.

Emulator: the handler builds `next_token = Some(...)` unconditionally and substitutes a TERMINAL_NEXT_TOKEN constant defined as the empty string whenever the scan is exhausted (or MaxResults is 0), so every GetTranscript 200 body contains "NextToken": "" on its last page instead of containing no NextToken member. The crate documents this as deliberate ("The member is reported on EVERY page ... rather than dropping it"), so it is a design divergence, not a missed branch. Feeding that value back is then refused 400 ValidationException (the modelled min-length-1 boundary, or the handler's own "NextToken is not a transcript pagination token this service issued").

Why: a client that paginates by the AWS rule — keep calling while the output token is PRESENT — never terminates on its own terms, and the generated SDK paginators do exactly that. botocore's paginator stops only when the output token is absent/None; an empty string is a present token, so boto3's get_paginator("get_transcript").paginate(...) feeds "" back and the emulator answers 400 ValidationException, where against AWS the same loop completes cleanly. Hand-written loops that test truthiness happen to survive, so the breakage is silent and client-dependent. The response also violates a modelled output constraint, so strictly-validating clients and conformance tooling see an illegal body.

Fix: GetTranscript's handler/serializer — the terminal branch that assembles GetTranscriptOutput.next_token must leave the member absent (None) when the scan is exhausted, instead of emitting the empty-string sentinel; the TERMINAL_NEXT_TOKEN sentinel and the "reported on EVERY page" rule should go with it. No other operation's token logic is implicated.

Traces: connectparticipant.complete-attachment-upload-twice-conflict-rejected (the only failing check in this sweep; its transcript walk pages GetTranscript with MaxResults=100 until the token is falsy). Not covered by any open connectparticipant issue: 20261109T180950Z-connectparticipant-error-paths-miss-accessdenied-and-aws-message.md, 20261114T011434Z-connectparticipant-describeview-403-accessdeniedexception-body-omits-the-required-message.md and 20261116T222001Z-connectparticipant-startattachmentupload-fails-open-on-an-invalid-connection-token.md are all error-path/message findings and none carries a pagination key.

## Evidence
Failing check: connectparticipant.complete-attachment-upload-twice-conflict-rejected — "Check timed out after 60.0s" (the check's own transcript walk pages GetTranscript to exhaustion twice).

Expected (AWS, per the Smithy model): the last page of a GetTranscript scan is HTTP 200 with a body carrying InitialContactId and Transcript and NO NextToken member, because GetTranscriptResponse$NextToken is @length(min 1, max 1000) and GetTranscript is @paginated with outputToken=NextToken.

Actual (emulator): the last page is HTTP 200 with "NextToken": "" — the member is present and empty. Source: services/service-com-amazonaws-connectparticipant-amazonconnectparticipantservicelambda/src/ops/get_transcript.rs — `let next_token = Some(if max_results > 0 && page_end < scan.len() { cursor.at(page_end).encode() } else { TERMINAL_NEXT_TOKEN.to_string() });` (around lines 311-325) with `const TERMINAL_NEXT_TOKEN: &str = "";` (around line 354) and the module doc at lines 14-15 ("The member is reported on EVERY page: the last page of a scan carries the terminal token ... rather than dropping it").

Follow-on refusal: re-submitting that value (what an SDK paginator does, since "" is a present token) is answered 400 ValidationException — either by the modelled NextToken min-length-1 boundary or by the handler's own message "NextToken is not a transcript pagination token this service issued" (lines 216-219) — so a paginated read of a chat transcript fails against the emulator and succeeds against AWS.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a connectparticipant client at its data listener.
2. Stand up a chat session: connect:CreateInstance + CreateContactFlow + StartChatContact, then connectparticipant:CreateParticipantConnection(Type=["CONNECTION_CREDENTIALS"]) to get a ConnectionToken.
3. Put at least one item in the transcript (SendMessage, or StartAttachmentUpload + CompleteAttachmentUpload).
4. Call GetTranscript(ConnectionToken=<token>, SortOrder="ASCENDING", MaxResults=100) and inspect the RAW 200 body: it contains "NextToken": "" even though every item fits on this one page. AWS omits the member on the page that ends a scan.
5. Paginate the AWS way: boto3 `list(cp.get_paginator("get_transcript").paginate(ConnectionToken=<token>, SortOrder="ASCENDING", MaxResults=100))`. The paginator treats the empty string as a present token, re-submits NextToken="" and the call is refused 400 ValidationException; against AWS the same paginator completes after one page.
6. Re-replay connectparticipant.complete-attachment-upload-twice-conflict-rejected to confirm the fix.
