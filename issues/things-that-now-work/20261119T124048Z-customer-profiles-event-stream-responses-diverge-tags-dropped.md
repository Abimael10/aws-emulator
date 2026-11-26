Issue-Source: trace-sweep-divergence
Issue-Trace: customer-profiles-create-event-stream-kinesis-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: customer-profiles-create-event-stream-kinesis-lifecycle
Issue-Shape-Id: com.amazonaws.customerprofiles#CustomerProfiles_20200815
Issue-Key: customer-profiles|CreateEventStream|2xx->2xx|$.Tags
Issue-Key: customer-profiles|GetEventStream|2xx->2xx|$.Tags
Issue-Key: customer-profiles|CreateEventStream|4xx->4xx|$.Message
Issue-Commit: 9c3f3fd7f5eb99b4aa92991d885e52b5ab74e4e0

# customer-profiles event-stream responses diverge: Tags dropped from create/get projections and the missing-domain message is rewritten

> _Note: this issue was identified against the code base at commit `9c3f3fd7f5eb99b4aa92991d885e52b5ab74e4e0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventStream

## Description
Three net-new wire causes in the customer-profiles event-stream surface, all in one fixture: the CreateEventStream and GetEventStream response projections both drop the Tags member, and the ResourceNotFoundException raised when CreateEventStream names a domain that does not exist carries the emulator's own wording instead of AWS's sentence.

Cause 1 of 3: CreateEventStream omits Tags from its response instead of returning it present and empty.
AWS: Smithy: CreateEventStreamResponse members are EventStreamArn (string1To255, @required) and Tags (TagMap, @length 1..50); POST /domains/{DomainName}/event-streams/{EventStreamName}, success code 200. Recorded behavior only for the empty case (the model marks Tags optional and says nothing about absence): an event stream created with no Tags returns 200 with Tags PRESENT and empty ({}); created WITH Tags it echoes the submitted map. This is the same convention the domain and segment-definition projections follow.
Emulator: 200 whose body carries EventStreamArn but no Tags key at all.
Why: an SDK caller that reads response['Tags'] straight off the create response - safe against AWS - raises KeyError, and code that distinguishes 'this event stream carries no tags' from 'tags were not reported' loses that distinction before it can issue a read.
Fix: the CreateEventStream response projection (the operation's serializer) - always serialize Tags, an empty map when none were supplied.
Traces: customer-profiles-create-event-stream-kinesis-lifecycle#task#4.

Cause 2 of 3: GetEventStream omits Tags the same way.
AWS: Smithy: GetEventStreamResponse carries DomainName, EventStreamArn, CreatedAt, State, DestinationDetails (all @required), StoppedSince and Tags (TagMap, @length 1..50); GET /domains/{DomainName}/event-streams/{EventStreamName}, success code 200. Recorded behavior only: reading back an event stream created with no Tags returns 200 with Tags present and equal to {}.
Emulator: 200 with the rest of the projection correct but no Tags key.
Why: the read side of the tagging round-trip cannot be used to confirm or deny tags on an event stream; tag-driven selection and cleanup logic keyed off GetEventStream sees a response shape AWS never returns.
Fix: the GetEventStream response projection - serialize the stream's current tag map, empty when it carries none.
Traces: customer-profiles-create-event-stream-kinesis-lifecycle#task#5.

Cause 3 of 3: the 404 for a CreateEventStream against a non-existent domain uses the emulator's generic domain-not-found sentence, not the event-stream one AWS returns.
AWS: Smithy: CreateEventStream models ResourceNotFoundException (errorKind client, httpError 404) among its errors; DomainName is an @required httpLabel (@length 1..64, pattern ^[a-zA-Z0-9_-]+$), so a syntactically valid but unknown domain is rejected at runtime as that modeled error. Recorded response: HTTP 404 with Message "Domain with name <DomainName> not found". Fixed text: "Domain with name " and " not found"; the single variable part is the DomainName taken verbatim from the request's http label (the recording's generated probe-domain name is that run's value, not part of the requirement).
Emulator: HTTP 404 (status and error class correct) with Message "Domain <DomainName> is not found" - different fixed text ('Domain ' / ' is not found' instead of 'Domain with name ' / ' not found').
Why: the status is right but the message is not AWS's, so callers and test suites that assert on the message text, and operators grepping logs for the AWS sentence, do not match. Note the emulator uses this ONE sentence for several unrelated not-found conditions, whereas AWS words the event-stream case differently from the GetDomain case, so the emulator also erases the distinction between them.
Fix: the CreateEventStream handler's domain-existence check - raise ResourceNotFoundException with AWS's event-stream wording, interpolating the request's DomainName, rather than delegating to the shared generic domain-not-found message.
Traces: customer-profiles-create-event-stream-kinesis-lifecycle#task#6.

Related but not covering: open issue 20261119T031550Z-customer-profiles-segment-definition-responses-diverge-tags-dropped.md files the same two fault classes for the SEGMENT-DEFINITION projections and its own BadRequestException wording, and 20261103T055557Z-customer-profiles-tagging-surface-unusable-tag-untag-listtags.md files them for the DOMAIN projections; neither carries a replay key for any event-stream step, so none of these three causes is covered. Open issue 20261103T055557Z-customer-profiles-answers-200-where-aws-refuses-domain.md owns the GetDomain/SearchProfiles/CreateCalculatedAttributeDefinition clusters in this same replay (absent domain-scope gates), not the message wording of Cause 3, which already returns the right status.

## Evidence
Replay of simparity-wire-aws-v1 (emulator_up=true, bootstrap_signal=false, score_pct=84.8, steps_total=165, steps_passed=140). All three clusters are verdict=mismatch, affected_count=1, with empty stub_path and empty owned_by (i.e. net-new, not already filed).

Cause 1 - fixture customer-profiles-create-event-stream-kinesis-lifecycle step task#4, CreateEventStream, status 200 -> 200:
  $.Tags: missing (expected {}, got None)

Cause 2 - same fixture step task#5, GetEventStream, status 200 -> 200:
  $.Tags: missing (expected {}, got None)

Cause 3 - same fixture step task#6, CreateEventStream, status 404 -> 404:
  $.Message: value_mismatch (expected 'Domain with name <DomainName> not found', got 'Domain <DomainName> is not found')

The domain and event-stream names in the recording are that run's generated values; the requirement is the shape above - Tags always present (empty map when none) on both event-stream projections, and AWS's message template with the request's own DomainName interpolated.

## Steps to Reproduce
Let D be an existing Customer Profiles domain, E an event-stream name matching ^[a-zA-Z0-9_-]{1,64}$, and U a Kinesis stream URI.

Cause 1: CreateEventStream(DomainName=D, EventStreamName=E, Uri=U) with NO Tags -> expect 200 whose body carries EventStreamArn and Tags == {}; observe 200 with no Tags key.
Cause 2: GetEventStream(DomainName=D, EventStreamName=E) -> expect 200 whose body carries Tags == {} alongside DomainName/EventStreamArn/CreatedAt/State/DestinationDetails; observe no Tags key.
Cause 3: CreateEventStream(DomainName=X, EventStreamName=E, Uri=U) where X is a syntactically valid domain name that does not exist -> expect 404 ResourceNotFoundException with Message 'Domain with name X not found'; observe 404 with Message 'Domain X is not found'.

Re-replay fixture customer-profiles-create-event-stream-kinesis-lifecycle (steps task#4, task#5, task#6) to confirm all three.
