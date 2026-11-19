Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: customer-profiles.event-stream-kinesis-lifecycle
Issue-Shape-Id: com.amazonaws.customerprofiles#CustomerProfiles_20200815
Issue-Key: customer-profiles.event-stream-kinesis-lifecycle|run#22|AssertionError|assert message == (
Issue-Commit: 0da99206c40b9a0852cdbaa2ba3793f12a97497b

# customer-profiles: GetEventStream's ResourceNotFoundException message for an absent event stream uses the emulator's own sentence, not AWS's domain-qualified wording

> _Note: this issue was identified against the code base at commit `0da99206c40b9a0852cdbaa2ba3793f12a97497b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetEventStream

## Description
One net-new root cause in the customer-profiles event-stream read surface.

Cause 1 of 1: after DeleteEventStream, GetEventStream correctly refuses with HTTP 404 ResourceNotFoundException, but the Message is worded by the emulator's shared event-stream-not-found helper instead of AWS's event-stream sentence, and it drops the domain scope entirely.

AWS: Smithy: GetEventStream is GET /domains/{DomainName}/event-streams/{EventStreamName}, success code 200; both DomainName and EventStreamName are @required httpLabels (name, @length 1..64, pattern ^[a-zA-Z0-9_-]+$); ResourceNotFoundException is modeled (errorKind client, httpError 404, documented "The requested resource does not exist, or access was denied."). The model does not fix the message text, so the wording is recorded behavior only: reading an event stream that was just deleted (or was never created) in an EXISTING domain answers 404 ResourceNotFoundException with Message "The EventStream with name <EventStreamName> not found for domain <DomainName>". Fixed text: "The EventStream with name ", " not found for domain ". Variable parts: the EventStreamName and the DomainName, both taken verbatim from the request's http labels. The generated names in any one recording are that run's values, not part of the requirement.

Emulator: HTTP 404 with the right error class (ResourceNotFoundException) but Message "EventStream <EventStreamName> is not found" — different fixed text ("EventStream " / " is not found" instead of "The EventStream with name " / " not found for domain "), and the DomainName variable part is absent altogether, so the message does not say which domain was searched.

Why: the status and code are right, so control flow survives, but any client, test suite, or operator log-grep that asserts on AWS's sentence does not match; and because the emulator's helper is name-only, a caller cannot tell from the message whether the event stream was missing in the domain it asked about or whether the domain scope itself was wrong — a distinction AWS's message makes explicit. It also means the not-found text for an event stream is indistinguishable in shape from other not-found texts this emulator emits.

Fix: the GetEventStream handler's event-stream lookup error, and the shared helper it delegates to, must emit AWS's event-stream wording with BOTH the request's EventStreamName and DomainName interpolated, rather than a name-only generic sentence.

Traces: failing check customer-profiles.event-stream-kinesis-lifecycle (the CreateEventStream -> RUNNING -> ListEventStreams -> DeleteEventStream -> GetEventStream sequence; the assertion that fails is the GetEventStream message after delete). File and line citations: services/service-com-amazonaws-customerprofiles-customerprofiles-20200815/src/ops/get_event_stream.rs lines 167-171 (the not_found(crate::state::event_stream_not_found(&input.event_stream_name)) call, which is passed no domain) and the crate::state::event_stream_not_found helper it uses.

Not covered by an existing issue: open issue 20261119T124048Z-customer-profiles-event-stream-responses-diverge-tags-dropped.md carries replay keys for CreateEventStream/GetEventStream $.Tags and for CreateEventStream's MISSING-DOMAIN 404 message ("Domain with name <D> not found"); it says nothing about the GetEventStream event-stream-not-found message exercised here, and the suite marks this check neither ALREADY FILED nor OWNED.

## Evidence
Behavioral suite replay: 40/47 passed; check customer-profiles.event-stream-kinesis-lifecycle verdict=mismatch.

Failure, raised in the check body after a successful DeleteEventStream:
  AssertionError: unexpected GetEventStream message after delete: 'EventStream sim2real-cp-es-149be8fa10f2 is not found'

The check first asserts the error code and that assertion PASSES (code == "ResourceNotFoundException"), then asserts the message:
  expected: "The EventStream with name <EventStreamName> not found for domain <DomainName>"
  actual:   "EventStream <EventStreamName> is not found"

So: HTTP 404 and error code ResourceNotFoundException are correct; only the Message diverges. Fixed text expected "The EventStream with name " + <EventStreamName> + " not found for domain " + <DomainName>; the emulator emits "EventStream " + <EventStreamName> + " is not found" and never mentions the domain. The names in the quoted line (sim2real-cp-es-..., sim2real-cp-dom-...) are that run's generated values and fill the template; they are not part of the requirement.

Everything earlier in the lifecycle passes: CreateEventStream, GetEventStream reaching State RUNNING with an EventStreamArn ending in the event-stream name, and ListEventStreams showing the stream while running.

## Steps to Reproduce
Let D be a Customer Profiles domain that exists, E an event-stream name matching ^[a-zA-Z0-9_-]{1,64}$, and U the ARN of an existing Kinesis stream.

1. CreateDomain(DomainName=D, DefaultExpirationDays=30).
2. CreateEventStream(DomainName=D, EventStreamName=E, Uri=U) and poll GetEventStream until State == RUNNING.
3. DeleteEventStream(DomainName=D, EventStreamName=E).
4. GetEventStream(DomainName=D, EventStreamName=E).

Expect: HTTP 404, Error.Code == "ResourceNotFoundException", Error.Message == "The EventStream with name E not found for domain D".
Observe: HTTP 404, Error.Code == "ResourceNotFoundException", Error.Message == "EventStream E is not found" (AWS's fixed text replaced and the domain omitted).

The same divergence reproduces without any delete, by calling GetEventStream(DomainName=D, EventStreamName=E) for an E that was never created in the existing domain D.

Re-replay behavioral check customer-profiles.event-stream-kinesis-lifecycle to confirm.
