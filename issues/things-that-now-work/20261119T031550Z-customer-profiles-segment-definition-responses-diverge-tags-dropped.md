Issue-Source: trace-sweep-divergence
Issue-Trace: customer-profiles-create-segment-definition-roundtrip
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: customer-profiles-create-segment-definition-roundtrip
Issue-Shape-Id: com.amazonaws.customerprofiles#CustomerProfiles_20200815
Issue-Key: customer-profiles|CreateSegmentDefinition|2xx->2xx|$.Tags
Issue-Key: customer-profiles|GetSegmentDefinition|2xx->2xx|$.Tags
Issue-Key: customer-profiles|CreateSegmentDefinition|4xx->4xx|$.Message
Issue-Commit: 9b159f9cbfb35899ba4a4563d05074a20e675bbc

# customer-profiles segment-definition responses diverge: Tags dropped from create/get projections and the missing-segment-source message is rewritten (3 causes)

> _Note: this issue was identified against the code base at commit `9b159f9cbfb35899ba4a4563d05074a20e675bbc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSegmentDefinition

## Description
Three net-new wire causes in the customer-profiles segment-definition surface: the create and get projections both drop the Tags member, and the BadRequestException raised when neither segment source is supplied carries the emulator's own wording instead of AWS's.

Cause 1 of 3: CreateSegmentDefinition omits Tags from its response instead of returning it present and empty.
AWS: Smithy: CreateSegmentDefinitionResponse$Tags targets TagMap (@length 1..50, jsonName "Tags"); the operation is POST /domains/{DomainName}/segment-definitions/{SegmentDefinitionName}, success code 200. Recorded behavior only for the empty case (the model marks Tags optional and says nothing about absence): a segment definition created with no Tags returns 200 with Tags PRESENT and empty ({}); created WITH Tags it echoes the submitted map. This is the same convention the domain projections follow.
Emulator: 200 whose body has no Tags key at all.
Why: an SDK caller that reads response['Tags'] straight off the create response - safe against AWS - raises KeyError, and code that distinguishes 'this segment definition carries no tags' from 'tags were not reported' loses that distinction before it can even issue a read.
Fix: the CreateSegmentDefinition response projection (the operation's serializer) - always serialize Tags, empty map when none were supplied.
Traces: customer-profiles-create-segment-definition-roundtrip#task#2.

Cause 2 of 3: GetSegmentDefinition omits Tags the same way.
AWS: Smithy: GetSegmentDefinitionResponse$Tags targets TagMap (@length 1..50, jsonName "Tags"); GET /domains/{DomainName}/segment-definitions/{SegmentDefinitionName}, success code 200. Recorded behavior only: reading back a segment definition created with no Tags returns 200 with Tags present and equal to {}.
Why: the read side of the tagging round-trip cannot be used to confirm or deny tags on a segment definition; tag-driven selection and cleanup logic keyed off GetSegmentDefinition sees a response shape AWS never returns.
Emulator: 200 with no Tags key.
Fix: the GetSegmentDefinition response projection - serialize the definition's current tag map, empty when it carries none.
Traces: customer-profiles-create-segment-definition-roundtrip#task#3.

Cause 3 of 3: the 400 for a CreateSegmentDefinition that supplies neither SegmentGroups nor SegmentSqlQuery uses the emulator's own sentence, not AWS's.
AWS: Smithy: SegmentGroups (SegmentGroup) and SegmentSqlQuery (sensitiveString1To50000) are both OPTIONAL members, so the 'exactly one source' rule is enforced at runtime and surfaces as the modeled BadRequestException (httpError 400). Recorded response: HTTP 400 with Message "Must specify either SegmentSqlQuery or SegmentGroups. Domain: <DomainName>, SegmentDefinitionName: <SegmentDefinitionName>". Fixed text: "Must specify either SegmentSqlQuery or SegmentGroups. Domain: " and ", SegmentDefinitionName: "; the variable parts are the DomainName and SegmentDefinitionName taken verbatim from the request's http labels (in the recording the probe's generated domain and segment names - those values are that run's, not part of the requirement).
Emulator: HTTP 400 (status correct) with Message "Either SegmentGroups or SegmentSqlQuery must be provided" - different wording and, critically, no domain/segment-definition qualification at all.
Why: the status and error class are right but the message is not AWS's: callers and test suites that assert on the message text, and operators grepping logs for the AWS sentence, do not match; the message also drops the two identifiers AWS includes, so a client batching many creates cannot tell WHICH request was rejected.
Fix: the CreateSegmentDefinition handler's request validation - raise BadRequestException with AWS's sentence, interpolating the request's DomainName and SegmentDefinitionName.
Traces: customer-profiles-create-segment-definition-roundtrip#task#4.

Related but not covering: open issue 20261103T055557Z-customer-profiles-tagging-surface-unusable-tag-untag-listtags.md files the same Tags-omission fault for the DOMAIN projections (CreateDomain/GetDomain) and the tagging operations' ARN gate; it carries no replay key for either segment-definition projection, so Causes 1 and 2 are not covered by it. Open issue 20261103T055557Z-customer-profiles-answers-200-where-aws-refuses-domain.md covers absent domain-scope/request gates (and owns the GetDomain/SearchProfiles/CreateCalculatedAttributeDefinition clusters in this same replay), not the message wording of Cause 3, which already returns the right status.

## Evidence
Replay of simparity-wire-aws-v1 (emulator_up=true, bootstrap_signal=false, score_pct=84.2). All three clusters are verdict=mismatch, affected_count=1, with empty stub_path and empty owned_by (i.e. net-new, not already filed).

Cause 1 - fixture customer-profiles-create-segment-definition-roundtrip step task#2, CreateSegmentDefinition, status 200 -> 200:
  $.Tags: missing (expected {}, got None)

Cause 2 - same fixture step task#3, GetSegmentDefinition, status 200 -> 200:
  $.Tags: missing (expected {}, got None)

Cause 3 - same fixture step task#4, CreateSegmentDefinition, status 400 -> 400:
  $.Message: value_mismatch (expected 'Must specify either SegmentSqlQuery or SegmentGroups. Domain: <DomainName>, SegmentDefinitionName: <SegmentDefinitionName>', got 'Either SegmentGroups or SegmentSqlQuery must be provided')

The domain and segment-definition names in the recording are that run's generated values; the requirement is the shape above - Tags always present (empty map when none) on both segment-definition projections, and the AWS message template with the request's own DomainName/SegmentDefinitionName interpolated.

## Steps to Reproduce
Let D be an existing Customer Profiles domain and S a segment-definition name matching ^[a-zA-Z0-9_-]{1,64}$.

Cause 1: CreateSegmentDefinition(DomainName=D, SegmentDefinitionName=S, DisplayName='probe', SegmentGroups={...}) with NO Tags -> expect 200 whose body carries Tags == {}; observe no Tags key.
Cause 2: GetSegmentDefinition(DomainName=D, SegmentDefinitionName=S) -> expect 200 whose body carries Tags == {}; observe no Tags key.
Cause 3: CreateSegmentDefinition(DomainName=D, SegmentDefinitionName=S2, DisplayName='probe') supplying NEITHER SegmentGroups NOR SegmentSqlQuery -> expect 400 BadRequestException with Message 'Must specify either SegmentSqlQuery or SegmentGroups. Domain: D, SegmentDefinitionName: S2'; observe 400 with Message 'Either SegmentGroups or SegmentSqlQuery must be provided'.

Re-replay fixture customer-profiles-create-segment-definition-roundtrip (steps task#2, task#3, task#4) to confirm all three.
