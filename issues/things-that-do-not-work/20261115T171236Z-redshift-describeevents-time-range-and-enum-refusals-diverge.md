Issue-Source: trace-sweep-divergence
Issue-Trace: redshift-rshealth-describe-events-scoping
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: redshift-rshealth-describe-events-scoping redshift-snapshot-schedule-lifecycle
Issue-Shape-Id: com.amazonaws.redshift#RedshiftServiceVersion20121201
Issue-Key: redshift|DescribeEvents|4xx->2xx|$.ErrorResponse
Issue-Key: redshift|DescribeEvents|4xx->2xx|status
Issue-Key: redshift|DescribeEvents|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: redshift|ModifySnapshotSchedule|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 860eb666e989875e1daa85fefb4b7f7b73b30cd3

# Redshift DescribeEvents time-range and enum refusals diverge and ModifySnapshotSchedule not-found wording is emulator-authored

> _Note: this issue was identified against the code base at commit `860eb666e989875e1daa85fefb4b7f7b73b30cd3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
This bundle carries THREE net-new root causes in the Redshift Query-protocol refusal surface: one request-validation check that does not run at all, and two refusal messages the emulator authors itself instead of emitting AWS's wording. All three must be closed before this issue is submitted.

Cause 1 of 3: DescribeEvents accepts an EndTime that precedes StartTime and answers HTTP 200 instead of refusing the combination (fixture redshift-rshealth-describe-events-scoping, step task#4).
AWS: DescribeEvents scopes an event read by StartTime/EndTime - Smithy: DescribeEvents input DescribeEventsMessage{SourceIdentifier, SourceType (enum SourceType), StartTime (TStamp), EndTime (TStamp), Duration, MaxRecords, Marker}, output EventsMessage, modeled errors: NONE (the operation declares no operation-specific faults), so the refusal is one of the Query-protocol service-common refusals and its text is recorded behavior only. The recorded response for StartTime > EndTime is HTTP 400 with ErrorResponse.Error = {Type: 'Sender', Code: 'InvalidParameterCombination', Message: 'End time must not precede start time'}. That message is ENTIRELY fixed text - it interpolates no request value, no timestamp and no member name - so it must be reproduced verbatim, and the RequestId element is a per-run value that is not asserted.
Emulator: the call is SERVED - HTTP 200 with an events payload and no ErrorResponse element at all (replay: "status: expected 400, got 200"; "$.ErrorResponse: missing (expected {'Error': {'Type': 'Sender', 'Code': 'InvalidParameterCombination', 'Message': 'End time must not precede start time'}, ...}, got None)"). The StartTime/EndTime ordering is never checked.
Why: a client that relies on AWS rejecting an inverted window gets a false positive - a monitoring or audit tool that swaps its window bounds (a common off-by-one in time arithmetic) silently receives an empty/unscoped event list from the emulator and a hard InvalidParameterCombination from AWS, so the bug is invisible in testing and fatal in production, and tests that assert the refusal pass against the emulator only.
Fix: the DescribeEvents handler's input validation (the time-window check that must run before the event store is queried), raising the Query-protocol Sender/InvalidParameterCombination refusal with the fixed message and serializing nothing else.

Cause 2 of 3: the DescribeEvents SourceType enum refusal lists the permitted values in the emulator's own (alphabetical) order instead of AWS's order (fixture redshift-rshealth-describe-events-scoping, step task#2).
AWS: SourceType is a closed enum - Smithy: DescribeEventsMessage$SourceType targets com.amazonaws.redshift#SourceType, whose values are {cluster, cluster-parameter-group, cluster-security-group, cluster-snapshot, scheduled-action}; an out-of-set value is refused with the Query-protocol constraint-violation refusal, HTTP 400, Error.Type 'Sender', Error.Code 'ValidationError'. The message template is recorded behavior only: "<n> validation error detected: Value '<supplied value>' at '<member name>' failed to satisfy constraint: Member must satisfy enum value set: [cluster-security-group, cluster-parameter-group, scheduled-action, cluster-snapshot, cluster]". FIXED text: " validation error detected: Value '", "' at '", "' failed to satisfy constraint: Member must satisfy enum value set: [", "]" AND the bracketed value list INCLUDING ITS ORDER - AWS emits the set in its own server-side order (cluster-security-group, cluster-parameter-group, scheduled-action, cluster-snapshot, cluster), which is neither alphabetical nor the model's declaration order. The only variable parts are the error count, the rejected value echoed from the request and the member name ('sourceType'); the recording's 'redshift-cluster' is a request value, not part of the template.
Emulator: the status, Code and the whole fixed prefix match, but the value list is re-ordered alphabetically (replay: expected "...enum value set: [cluster-security-group, cluster-parameter-group, scheduled-action, cluster-snapshot, cluster]", got "...enum value set: [cluster, cluster-parameter-group, cluster-security-group, cluster-snapshot, scheduled-action]"), i.e. the list is rendered from a sorted/model-ordered collection rather than AWS's fixed order.
Why: the enum list is part of the refusal's fixed text, so any client that compares or snapshot-tests the validation message (CLI output assertions, log scrapers, golden-file tests of error text) sees a sentence no AWS region emits, and a human reading the emulator's message is told a different canonical ordering than AWS documents in its own errors.
Fix: the shared Query-protocol enum-constraint validator that renders the value set for the ValidationError message (and the SourceType enum ordering it reads) - the list must be emitted in AWS's recorded order for this enum, not sorted and not in Smithy declaration order.

Cause 3 of 3: ModifySnapshotSchedule's not-found refusal carries the model's documentation sentence instead of AWS's message (fixture redshift-snapshot-schedule-lifecycle, step task#7).
AWS: modifying a schedule whose ScheduleIdentifier does not exist is refused with the modeled fault - Smithy: ModifySnapshotSchedule input ScheduleIdentifier (required) + ScheduleDefinitions (required), errors {InvalidScheduleFault, SnapshotScheduleNotFoundFault, SnapshotScheduleUpdateInProgressFault}; SnapshotScheduleNotFoundFault is errorKind client, httpError 400, awsQueryError code 'SnapshotScheduleNotFound', documented "We could not find the specified snapshot schedule." The wire message is recorded behavior only: HTTP 400 with Error.Message "No snapshot schedule found for schedule identifier <ScheduleIdentifier>". FIXED text: "No snapshot schedule found for schedule identifier " with NO trailing period; the single variable part is the ScheduleIdentifier echoed verbatim from the request (the recording's 'sim2real-probe-ss-missing' is a request value, not part of the template).
Emulator: the status and error code match, but the message is the Smithy fault's documentation sentence with the identifier spliced in and quoted: "We could not find the specified snapshot schedule '<ScheduleIdentifier>'." - different fixed text, quoted identifier, trailing period (replay: expected 'No snapshot schedule found for schedule identifier sim2real-probe-ss-missing', got "We could not find the specified snapshot schedule 'sim2real-probe-ss-missing'.").
Why: clients and operators that match or display refusal text (idempotency wrappers that recognise the not-found sentence, log scrapers, CLI snapshot tests) see a message AWS never emits; worse, the pattern of emitting the model's doc string as the wire message generalises to every fault built this way, so error text is systematically wrong rather than wrong in one place.
Fix: the ModifySnapshotSchedule handler's not-found refusal message (the SnapshotScheduleNotFoundFault mapping must carry AWS's per-operation wording, not the shape's documentation trait). Open issue 20261108T233629Z-redshift-snapshot-schedule-lifecycle-diverges-missing-invalidschedule-floor.md covers other snapshot-schedule causes (sub-hourly rate floor, already-exists wording, modify/delete not reaching state) but NOT this not-found message and it carries no replay key for this step, so this cause is net-new and must be re-replayed here.

Traces: redshift-rshealth-describe-events-scoping#task#4 (Cause 1, representative), redshift-rshealth-describe-events-scoping#task#2 (Cause 2), redshift-snapshot-schedule-lifecycle#task#7 (Cause 3).

## Evidence
Cause 1 - DescribeEvents with EndTime before StartTime (fixture redshift-rshealth-describe-events-scoping, step task#4):
  Expected (recorded AWS): HTTP 400; $.ErrorResponse.Error = {Type: 'Sender', Code: 'InvalidParameterCombination', Message: 'End time must not precede start time'} (RequestId shaped, not asserted).
  Actual (emulator): HTTP 200 and no ErrorResponse element - replay detail: "status: expected 400, got 200" / "$.ErrorResponse: missing (expected {'Error': {'Type': 'Sender', 'Code': 'InvalidParameterCombination', 'Message': 'End time must not precede start time'}, 'RequestId': '<shaped-not-asserted>'}, got None)". Divergence key: service=redshift|DescribeEvents|status=400->200|keys=$.ErrorResponse,status.

Cause 2 - DescribeEvents with an out-of-set SourceType (step task#2; recording supplies 'redshift-cluster'):
  Expected (recorded AWS): HTTP 400, Error.Code ValidationError, Error.Message "1 validation error detected: Value 'redshift-cluster' at 'sourceType' failed to satisfy constraint: Member must satisfy enum value set: [cluster-security-group, cluster-parameter-group, scheduled-action, cluster-snapshot, cluster]".
  Actual (emulator): HTTP 400 with the same prefix but the set re-ordered: "... Member must satisfy enum value set: [cluster, cluster-parameter-group, cluster-security-group, cluster-snapshot, scheduled-action]". Only $.ErrorResponse.Error.Message diverges. Divergence key: service=redshift|DescribeEvents|status=400->400|keys=$.ErrorResponse.Error.Message.

Cause 3 - ModifySnapshotSchedule on an unknown ScheduleIdentifier (fixture redshift-snapshot-schedule-lifecycle, step task#7):
  Expected (recorded AWS): HTTP 400, Error.Code SnapshotScheduleNotFound, Error.Message 'No snapshot schedule found for schedule identifier <ScheduleIdentifier>' (recording: '... sim2real-probe-ss-missing', no trailing period).
  Actual (emulator): HTTP 400 with Error.Message "We could not find the specified snapshot schedule '<ScheduleIdentifier>'." - the Smithy fault's documentation sentence. Only $.ErrorResponse.Error.Message diverges. Divergence key: service=redshift|ModifySnapshotSchedule|status=400->400|keys=$.ErrorResponse.Error.Message.

Model basis: Smithy DescribeEvents input {SourceIdentifier, SourceType->SourceType enum, StartTime, EndTime, Duration, MaxRecords, Marker}, output EventsMessage, errors [] (service-common refusals only); com.amazonaws.redshift#SourceType values {cluster, cluster-parameter-group, cluster-security-group, cluster-snapshot, scheduled-action}; ModifySnapshotSchedule errors {InvalidScheduleFault 400/InvalidSchedule, SnapshotScheduleNotFoundFault 400/SnapshotScheduleNotFound (doc "We could not find the specified snapshot schedule."), SnapshotScheduleUpdateInProgressFault 400}.

## Steps to Reproduce
1. Boot the emulator and point a redshift (Query protocol, API version 2012-12-01) client at its data listener.
2. Cause 1: call DescribeEvents with StartTime later than EndTime (any inverted window, e.g. StartTime = now, EndTime = now - 1h). Observe HTTP 200 with an events payload; AWS answers HTTP 400 with Error.Type Sender, Error.Code InvalidParameterCombination, Error.Message 'End time must not precede start time'.
3. Cause 2: call DescribeEvents with SourceType set to a value outside the enum (e.g. 'redshift-cluster'). Observe HTTP 400 whose Message ends "enum value set: [cluster, cluster-parameter-group, cluster-security-group, cluster-snapshot, scheduled-action]"; AWS ends it "enum value set: [cluster-security-group, cluster-parameter-group, scheduled-action, cluster-snapshot, cluster]". Compare the full message string, not just the status/code.
4. Cause 3: call ModifySnapshotSchedule with a ScheduleIdentifier that does not exist (e.g. 'sim2real-probe-ss-missing') and any ScheduleDefinitions. Observe HTTP 400 / SnapshotScheduleNotFound with Message "We could not find the specified snapshot schedule '<id>'."; AWS answers 'No snapshot schedule found for schedule identifier <id>' with no quoting and no trailing period.
5. Replay the bundled fixture steps: redshift-rshealth-describe-events-scoping#task#4, redshift-rshealth-describe-events-scoping#task#2, redshift-snapshot-schedule-lifecycle#task#7.
