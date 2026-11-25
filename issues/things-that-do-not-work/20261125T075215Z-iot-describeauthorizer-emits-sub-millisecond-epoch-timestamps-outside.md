Issue-Source: trace-sweep-divergence
Issue-Trace: iot-authorizer-refusal-unresolvable-and-two-key-boundary
Issue-Step: task#6
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-authorizer-refusal-unresolvable-and-two-key-boundary
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeAuthorizer|2xx->2xx|$.authorizerDescription.creationDate
Issue-Key: iot|DescribeAuthorizer|2xx->2xx|$.authorizerDescription.lastModifiedDate
Issue-Commit: a229f129d1ff315b559df25acb8faf2dc81c8c64

# iot DescribeAuthorizer emits sub-millisecond epoch timestamps outside AWS's shape

> _Note: this issue was identified against the code base at commit `a229f129d1ff315b559df25acb8faf2dc81c8c64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAuthorizer

## Description
One net-new cause on the iot authorizer read path. The fixture step does NOT assert the recorded timestamp VALUE (the corpus records it as '<shaped-not-asserted>', i.e. a per-run clock value), so a clock difference alone passes; it asserts the SHAPE of the serialized timestamp, and the emulator's rendering fails that assertion.

Cause 1 of 1: AuthorizerDescription.creationDate and .lastModifiedDate are serialized with sub-millisecond precision.
AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json): AWSIotService version 2015-05-28, protocol restJson1; DescribeAuthorizer -> GET /authorizer/{authorizerName}, success 200, output member authorizerDescription targets AuthorizerDescription, whose members creationDate and lastModifiedDate both target DateType, a plain `timestamp` with no @timestampFormat trait — so the restJson1 default applies and both are serialized as a JSON number of epoch seconds. Their docs are "The UNIX timestamp of when the authorizer was created" and "The UNIX timestamp of when the authorizer was last updated". The precision itself is recorded behavior only: IoT emits epoch seconds with at most millisecond resolution (at most three fractional digits, e.g. 1763489234.567), which is the shape the fixture asserts in place of the unasserted value. Hypothesis: the assertion fails specifically on the extra sub-millisecond digits, since that is the only non-per-run property of the emitted number that can differ.
Emulator: answers 200 with both members carried as a float of full `SystemTime` resolution — creationDate and lastModifiedDate each rendered with seven fractional digits (the replay observed values of the form <epoch-seconds>.<7 digits>). The shape assertion on both members therefore fails while the status/body around them matches.
Why: a client that round-trips these timestamps against AWS's own precision sees a value AWS would never emit. Anything that compares, hashes, formats or re-serializes the timestamp (cache keys, golden-file assertions, a fixed-precision decimal column, a test that pins the response body) matches on AWS and diverges on the emulator, and a strict client-side parser that only accepts epoch-millisecond timestamps rejects the response outright. Because the same conversion is shared by every IoT record that stores a SystemTime, the divergence is systematic rather than specific to this one read.
Fix: the AuthorizerDescription projection in the iot service's state — the `RegisteredAuthorizer::description()` conversion that maps the stored `SystemTime` creation/last-modified instants through `aws_smithy_types::DateTime::from` — or the shared timestamp capture that feeds it: truncate the instant to millisecond resolution when it is recorded (or when it is projected) so the serialized epoch-seconds number carries no sub-millisecond digits. No other member of this response diverges.
Traces: iot-authorizer-refusal-unresolvable-and-two-key-boundary#task#6 (cluster size 1).

Relation to an already-open issue: 'iot authorizer status is not persisted, and ListThingPrincipals and UntagResource responses diverge (4 causes)' (20261118T162135Z-iot-authorizer-status-is-not-persisted-and-listthingprincipals.md) carries the SAME fixture and step as its representative binding, but it does NOT cover this cause: its replay key for this operation is `iot|DescribeAuthorizer|2xx->2xx|$.authorizerDescription.status`, and its own text explicitly excludes creationDate/lastModifiedDate from the finding, judging them per-run clock values. With the status cause now matching, what this cluster still reports is the two date members alone, and the replay attributes it to no open issue (owned_by empty).

## Evidence
Replay of trace set simparity-wire-aws-v1 (169 fixtures, 1120 steps, score 81.8%). Cluster: service=iot, operation=DescribeAuthorizer, verdict=mismatch, fixture_id=iot-authorizer-refusal-unresolvable-and-two-key-boundary, step_key=task#6, affected_count=1, stub_path empty, owned_by empty.

expected status 200 / actual status 200 — the divergence is entirely in the body:
  $.authorizerDescription.creationDate: value_mismatch (expected '<shaped-not-asserted>', got 1795592658.6697628)
  $.authorizerDescription.lastModifiedDate: value_mismatch (expected '<shaped-not-asserted>', got 1795592658.6697628)

'<shaped-not-asserted>' is the corpus's shape-only assertion: the recorded value is withheld because it is a per-run clock reading, so only the serialized form is checked. The two observed numbers carry seven fractional digits; an epoch-seconds timestamp as IoT emits it carries at most three. The integral part (a plausible current epoch second) and the fact that creationDate equals lastModifiedDate on a freshly created authorizer are both correct and are NOT part of this finding — neither is the specific recorded number, which is a per-run value.

Model evidence: AuthorizerDescription.creationDate -> DateType (timestamp, no @timestampFormat), doc "The UNIX timestamp of when the authorizer was created"; AuthorizerDescription.lastModifiedDate -> DateType, doc "The UNIX timestamp of when the authorizer was last updated"; service protocol restJson1 (default epoch-seconds for an untraited timestamp).

Code evidence: services/service-com-amazonaws-iot-awsiotservice/src/ops/describe_authorizer.rs:177-179 returns `authorizer.description()` unmodified; services/service-com-amazonaws-iot-awsiotservice/src/state.rs:2409-2412 stores `creation_date`/`last_modified_date` as `SystemTime`, and :2474-2475 projects them with `aws_smithy_types::DateTime::from(self.creation_date)` / `(self.last_modified_date)`, carrying the full sub-millisecond resolution of the captured instant into the response.

## Steps to Reproduce
Boot the emulator from this checkout and replay the corpus (replay_wire_corpus), or drive the step directly:

1. POST /authorizer/<authorizerName> (CreateAuthorizer) with authorizerFunctionArn, tokenKeyName and one or two tokenSigningPublicKeys.
2. GET /authorizer/<authorizerName> (DescribeAuthorizer) and read authorizerDescription.creationDate and authorizerDescription.lastModifiedDate from the 200 body.
3. Inspect the serialized numbers, not their values: AWS emits epoch seconds with at most three fractional digits; the emulator emits seven (full SystemTime resolution).

Replay key: iot-authorizer-refusal-unresolvable-and-two-key-boundary#task#6.
