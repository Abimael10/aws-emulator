Issue-Source: trace-sweep-divergence
Issue-Trace: personalize-events-put-events-lifecycle
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: personalize-events-put-events-lifecycle
Issue-Shape-Id: com.amazonaws.personalizeevents#AmazonPersonalizeEvents
Issue-Key: personalize-events|PutEvents|4xx->4xx|$.message
Issue-Commit: 57fead3d76889f671b891b7ff2471bd21da9f263

# PutEvents reserved-properties-keyword refusal uses emulator-authored wording instead of AWS's message

> _Note: this issue was identified against the code base at commit `57fead3d76889f671b891b7ff2471bd21da9f263`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One root cause behind the one failing personalize-events wire step. PutEvents correctly REFUSES a reserved keyword in Event.properties, with the right error shape and the right HTTP status, but it emits its own sentence instead of the sentence AWS words that refusal with, so the error message a client reads over the wire diverges.

Cause 1 of 1: the reserved-properties-keyword branch of PutEvents' properties validation formats an emulator-authored message. Failing step: fixture personalize-events-put-events-lifecycle, step task#10.

AWS: Event$properties documents the keys that "can't be included as a keyword for properties (case insensitive)" — userId, sessionId, eventType, timestamp, recommendationId, impression (Smithy: com.amazonaws.personalizeevents#Event$properties documentation). A PutEvents whose properties blob is keyed with one of them is refused with InvalidInputException, the only error PutEvents models (Smithy: PutEvents errors = [InvalidInputException], httpError 400; POST /events, success code 200). The recorded response is HTTP 400 with the message "sessionid is not allowed in properties. Please refer to documentation to include sessionid". The requirement is that template, not that one string: the FIXED text is "<key> is not allowed in properties. Please refer to documentation to include <key>", and its single VARIABLE part is the offending reserved keyword taken from the request's properties object, LOWER-CASED and interpolated twice. "sessionid" in the recording is that variable filled by this request's "sessionId" key — a request keyed "eventType" must read "eventtype is not allowed in properties. Please refer to documentation to include eventtype", and so on for each reserved key.

Emulator: the refusal carries code InvalidInputException and HTTP 400 (both correct — the replay reports expected 400, actual 400), but the message body is "Event.properties must not use the reserved key sessionId; Personalize reserves it case insensitively". That is a different sentence in three ways: it names the input member ("Event.properties"), which AWS's message never mentions; it echoes the canonical camelCase spelling of the reserved key rather than the lower-cased one; and it appends a trailing clause ("; Personalize reserves it case insensitively") that AWS does not emit. The replay's only reported difference for the step is $.message.

Why: the message text of a modeled error is part of the contract a client codes against. A client that classifies or surfaces this refusal by its message — an error mapper keyed on the AWS sentence, a log or alarm assertion, or a repair path that strips the offending key by parsing it back out of the message (the AWS message is parseable because it states the lower-cased key alone, the emulator's is not) — does not recognise the emulator's sentence, so the ingestion failure is reported as an unknown error or silently mishandled. An operator comparing an emulator log against a production log cannot match the two.

Fix: the shared properties validation of the personalize-events ingestion service — the reserved-key branch of the properties parser that PutEvents (and the sibling ingestion operations that call the same parser) uses, together with the reserved-key table it matches against — must emit AWS's sentence, interpolating the lower-cased offending key twice and dropping both the member-name prefix and the trailing clause. The handler's error class (InvalidInputException) and HTTP status (400) are already correct and must not change.

Related open issue, which does NOT cover this: 20261115T185027Z-personalize-events-2-net-new-causes-put-ingestion describes the same wording defect as its Cause 2, but it is a BEHAVIORAL issue keyed to the behavioral check personalize-events.put-events-reserved-property-keyword-rejected and carries no replay key for this wire fixture step, so this wire divergence is not gated by it and a fix closing it would not be re-replayed against this corpus. It is also keyed to the "eventType" key; this step exercises "sessionId", confirming the template must be filled per-key.

Traces: personalize-events-put-events-lifecycle#task#10 (PutEvents with a properties blob keyed "sessionId": status 400 and code InvalidInputException match, $.message diverges).

## Evidence
Wire replay of fixture personalize-events-put-events-lifecycle, step task#10 (service personalize-events, operation PutEvents), verdict mismatch:
  expected_status 400, actual_status 400
  detail: $.message: value_mismatch (expected 'sessionid is not allowed in properties. Please refer to documentation to include sessionid', got 'Event.properties must not use the reserved key sessionId; Personalize reserves it case insensitively')

Expected (AWS): HTTP 400, Error Code "InvalidInputException", Error Message "<key> is not allowed in properties. Please refer to documentation to include <key>" where <key> is the offending reserved properties key from the request, lower-cased — here "sessionid is not allowed in properties. Please refer to documentation to include sessionid".
Actual (emulator): HTTP 400, Error Code "InvalidInputException", Error Message "Event.properties must not use the reserved key sessionId; Personalize reserves it case insensitively".

Fixed vs variable parts of the required message: fixed text = " is not allowed in properties. Please refer to documentation to include " plus the leading and trailing key occurrences; variable = the reserved key as sent in the request's properties object, lower-cased (sessionId -> sessionid; userId -> userid; eventType -> eventtype; timestamp, recommendationId -> recommendationid, impression likewise). No per-run value (request id, timestamp, tracking id) appears in the message.

Model basis: Smithy com.amazonaws.personalizeevents#PutEvents — input trackingId/sessionId/eventList required, errors [InvalidInputException] with httpError 400, http POST /events code 200; com.amazonaws.personalizeevents#Event$properties documentation lists userId, sessionId, eventType, timestamp, recommendationId, impression as keys that "can't be included as a keyword for properties (case insensitive)". The message wording itself is recorded behavior only (the model does not state message text).

## Steps to Reproduce
1. Boot the emulator and replay fixture personalize-events-put-events-lifecycle; the divergence is step task#10.
2. Equivalent by hand: create an ACTIVE Personalize event tracker (personalize.CreateSchema for an Interactions schema -> CreateDatasetGroup -> poll DescribeDatasetGroup to ACTIVE -> CreateDataset(datasetType="Interactions") -> poll DescribeDataset to ACTIVE -> CreateEventTracker -> poll DescribeEventTracker to ACTIVE) and take its trackingId.
3. Call personalize-events.PutEvents(trackingId=<trackingId>, sessionId=<session>, eventList=[{eventType="click", sentAt=<now>, itemId="item-1", properties=json({"sessionId": "reserved-collision"})}]).
   Expected: HTTP 400, Code "InvalidInputException", Message "sessionid is not allowed in properties. Please refer to documentation to include sessionid".
   Actual: HTTP 400, Code "InvalidInputException", Message "Event.properties must not use the reserved key sessionId; Personalize reserves it case insensitively".
4. Repeat step 3 once per reserved key (userId, eventType, timestamp, recommendationId, impression, and mixed-case spellings such as SESSIONID): each must be refused with the same fixed text filled by that key lower-cased.
5. Control: repeat step 3 with properties=json({"numberOfRatings": "12"}) -> HTTP 200 with an empty body (already correct; must stay accepted).
