Issue-Source: trace-sweep-divergence
Issue-Trace: cwp-delete-anomaly-detector-describe-empty
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cwp-delete-anomaly-detector-describe-empty cwa-range-operator-threshold-metric-id
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|PutAnomalyDetector|2xx->2xx|$.AnomalyDetectorId
Issue-Key: monitoring|DescribeAnomalyDetectors|2xx->2xx|$.AnomalyDetectors[*].AnomalyDetectorId
Issue-Commit: f0e185a528f8edd646da50d28e9f657e7fd1f0a5

# cloudwatch AnomalyDetectorId is minted as <uuid>/<epoch-millis> instead of an opaque single-token id

> _Note: this issue was identified against the code base at commit `f0e185a528f8edd646da50d28e9f657e7fd1f0a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutAnomalyDetector

## Description
This issue bundles 2 net-new root causes in the cloudwatch (monitoring, 2010-08-01) emulator, both concerning the wire FORM of the anomaly-detector identifier: the value minted by PutAnomalyDetector and the value echoed back by DescribeAnomalyDetectors. Each is numbered below with its own fixture and step. Both must be closed before submitting.

Note on what is and is not being asserted here: the replay harness does NOT assert the literal id value (the fixture expects '<shaped-not-asserted>', i.e. the per-run id is normalised away). It asserts the identifier's SHAPE CLASS, and that is what diverges — so this is a format defect, not a complaint about a generated value differing between runs.

Cause 1 of 2: PutAnomalyDetector returns an AnomalyDetectorId built from two concatenated segments, a UUID and an epoch-millisecond timestamp, where AWS returns a single opaque identifier token.
AWS: Smithy (cloudwatch 2010-08-01) models PutAnomalyDetectorOutput$AnomalyDetectorId as target AnomalyDetectorId, a string with @length min 1 max 128 and @pattern ^[A-Za-z0-9_./:%()+-]+$; the member on the AnomalyDetector shape documents it as "The unique identifier of the anomaly detector" and warns that the identifier does not restrict access to a specific detector in an IAM policy. The model constrains the character set only and says nothing about internal structure, so the concrete form is recorded behavior only: the recording's AnomalyDetectorId is a single opaque identifier token — one segment, no embedded '/' path separator and no embedded clock value — and the fixture's shape assertion is calibrated to that class. Status 200, no error involved.
Emulator: PutAnomalyDetector answers HTTP 200 (status matches) but with an AnomalyDetectorId of the form '<uuid>/<epoch-millis>' — a UUID, a literal '/', and the detector's creation time in milliseconds since the epoch (the replayed response carried a value of exactly that shape). The harness reports '$.AnomalyDetectorId: value_mismatch (expected <shaped-not-asserted>, got ...)': the two-segment, timestamp-bearing form is not the identifier class AWS returns, even though it happens to satisfy the member's permissive character pattern.
Why: the identifier is an opaque handle to a client, but its FORM leaks into client code in ways AWS's form does not. Embedding a '/' makes the id look like a path/resource segment, so a client that splits on '/', uses the id as a path component, or interpolates it into an ARN or a URL builds a different string than it would against AWS. Embedding the creation clock makes an id that AWS treats as opaque into a value whose length and content vary with time, and invites clients to parse a creation time out of it that AWS does not promise. A client that round-trips the id only through this emulator never notices; one that validates or decomposes identifiers diverges from AWS the first time it sees a real one.
Fix: PutAnomalyDetector's handler — specifically the anomaly-detector id minting in the detector state/registry (the place that composes the stored identifier), not the serializer: mint a single-segment opaque identifier within the modelled @length 1..128 / @pattern ^[A-Za-z0-9_./:%()+-]+$, with no '/' separator and no epoch-millisecond component, and store that as the detector's key so reads return the same token.
Traces: cwp-delete-anomaly-detector-describe-empty#task#1.

Cause 2 of 2: DescribeAnomalyDetectors serves the same two-segment '<uuid>/<epoch-millis>' value in AnomalyDetectors[].AnomalyDetectorId.
AWS: DescribeAnomalyDetectors is @paginated (inputToken/outputToken NextToken, items AnomalyDetectors) and returns a list of AnomalyDetector structures whose AnomalyDetectorId member has the same @length 1..128 / @pattern ^[A-Za-z0-9_./:%()+-]+$ constraints as the Put output. Recorded behavior only: each listed detector's id is the same single opaque token AWS issued at create time, so a read-back is byte-identical to the create response and belongs to the same identifier class.
Emulator: HTTP 200 (status matches) but '$.AnomalyDetectors[0].AnomalyDetectorId: value_mismatch (expected <shaped-not-asserted>, got ...)' — the listed id is of the same '<uuid>/<epoch-millis>' two-segment form as Cause 1, confirming the composite id is persisted in detector state and re-served on every read, not merely synthesised in the Put response. Any read path that returns detector ids (and any input that accepts AnomalyDetectorIds for filtering) is therefore affected by the same minting defect.
Why: the non-conforming form is not a one-response accident — it is what clients store and later feed back as DescribeAnomalyDetectors$AnomalyDetectorIds, so the divergence propagates into the client's own persistence. A client that validated or canonicalised identifiers against AWS's form on the way in would reject or rewrite ids the emulator issued.
Fix: the DescribeAnomalyDetectors read path/serializer requires no change of its own once Cause 1 is fixed at the minting site, provided it echoes the stored key verbatim; verify it does not re-compose the id (appending a timestamp or a version suffix) when projecting stored detector state onto the AnomalyDetector output shape.
Traces: cwa-range-operator-threshold-metric-id#task#7 (cluster representative; the same divergence also appears at cwp-delete-anomaly-detector-describe-empty#task#2).

## Evidence
Cause 1 — fixture cwp-delete-anomaly-detector-describe-empty, step task#1, operation PutAnomalyDetector, expected status 200 / actual status 200. Harness detail: "$.AnomalyDetectorId: value_mismatch (expected '<shaped-not-asserted>', got 'f09cc371-5ad9-4d95-80bb-16efbc873864/1795661154190')". Expected: a single opaque identifier token of the class the recording carries (no '/' segment separator, no embedded epoch-millisecond clock), satisfying AnomalyDetectorId @length 1..128 and @pattern ^[A-Za-z0-9_./:%()+-]+$. Actual: a two-segment '<uuid>/<epoch-millis>' value. The quoted UUID and millisecond value are per-run and are NOT the requirement — the requirement violated is the identifier's single-token form; the fixture normalises the value itself to '<shaped-not-asserted>' and still reports a mismatch.

Cause 2 — fixture cwa-range-operator-threshold-metric-id, step task#7, operation DescribeAnomalyDetectors, expected status 200 / actual status 200. Harness detail: "$.AnomalyDetectors[0].AnomalyDetectorId: value_mismatch (expected '<shaped-not-asserted>', got 'd0adcbed-1454-4e51-9171-15cbd2de4379/1795661264849')". Cluster affected_count 2, other member cwp-delete-anomaly-detector-describe-empty#task#2 — the same '<uuid>/<epoch-millis>' form, so the composite id is persisted and re-served, not response-local.

Neither cluster carries a stub_path or an owned_by entry in the replay report. The open issues 20261117T082319Z-cloudwatch-7-net-new-causes-putmetricalarm-accepts-threshold.md and 20261121T034732Z-cloudwatch-3-net-new-causes-the-metric-math.md touch PutAnomalyDetector / DescribeAnomalyDetectors but only their refusal messages and alarm-band behavior; neither mentions the identifier's form and neither carries these fixtures' replay keys, so both causes are net-new.

## Steps to Reproduce
Cause 1: call PutAnomalyDetector(SingleMetricAnomalyDetector={Namespace:<ns>, MetricName:<metric>, Stat:'Average'}) against the emulator's cloudwatch (monitoring) endpoint and read AnomalyDetectorId from the 200 response. Observe it is '<uuid>/<epoch-millis>': it contains a '/' and its trailing segment is the creation time in milliseconds since the epoch. AWS returns a single opaque token with neither. Replay fixture cwp-delete-anomaly-detector-describe-empty, step task#1.

Cause 2: after the Put above, call DescribeAnomalyDetectors (unfiltered, or with Namespace/MetricName matching the detector) and read AnomalyDetectors[0].AnomalyDetectorId. Observe the same two-segment '<uuid>/<epoch-millis>' value, i.e. the composite id is stored and re-served on reads. Replay fixture cwa-range-operator-threshold-metric-id, step task#7, and the sibling cwp-delete-anomaly-detector-describe-empty, step task#2.

In both cases compare only the identifier's FORM (segment count, presence of '/', presence of an epoch-millisecond component) against the modelled AnomalyDetectorId constraints — not the per-run UUID or timestamp values.
