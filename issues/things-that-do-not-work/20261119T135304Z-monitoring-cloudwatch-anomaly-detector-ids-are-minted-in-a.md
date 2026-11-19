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
Issue-Commit: 8cbd005b36aa276ee7a2abb030c4a31aa4fd1cd2

# CloudWatch anomaly detector ids are minted in a non-AWS form

> _Note: this issue was identified against the code base at commit `8cbd005b36aa276ee7a2abb030c4a31aa4fd1cd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutAnomalyDetector

## Description
This issue bundles TWO causes sharing one fault: the identifier this emulator mints for an anomaly detection model is a bare random UUIDv4, which does not have the form AWS's recorded AnomalyDetectorId has, so BOTH the write that reports the identifier and the read that reports it back diverge. There are 2 causes to close.

The single minting rule is the common root: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/anomaly_detector_reads.rs:172 `minted_anomaly_detector_id()` returns `uuid_from_random_bytes(...)`, and both the PutAnomalyDetector handler (src/ops/put_anomaly_detector.rs:226-245) and the read projection `StoredAnomalyDetector::reported()` (same file, line 103) report that value verbatim. Fixing the mint fixes both causes, but each is its own replay cluster and must be re-replayed.

Related open issue: "CloudWatch alarm history data and CreationId are emulator-authored or absent" (20261102T112036Z-monitoring-cloudwatch-alarm-history-data-and-creationid-are-emulator.md) covers the same family of emulator-authored identity values on ALARM reads (MetricAlarm.CreationId, formed as '<uuid>/<epoch-millis>'). It does NOT carry the replay keys for either anomaly-detector cluster below, so they are filed net-new.

Cause 1 of 2: PutAnomalyDetector reports an AnomalyDetectorId of the wrong form.
  AWS: Smithy: PutAnomalyDetectorOutput$AnomalyDetectorId -> AnomalyDetectorId, @length 1..128, @pattern ^[A-Za-z0-9_./:%()+-]+$, documented as "The unique identifier of the anomaly detector that you created or updated." The model fixes only the character set and length, so the form itself is recorded behavior only: the fixture records the value under a shape assertion, and a bare lowercase UUIDv4 - which DOES satisfy the modelled pattern and length - still fails that assertion, so AWS's identifier is structurally richer than a plain uuid. Hypothesis: it follows the same composite form as the other identity values CloudWatch mints in this corpus ('<uuid>/<epoch-millis>' for MetricAlarm.CreationId), i.e. a uuid plus a service-minted suffix rather than a uuid alone. Status is 200 on both sides; this is a payload-form divergence, not an error-path one.
  Emulator: returns a bare random UUIDv4 for AnomalyDetectorId (e.g. a 36-character 8-4-4-4-12 hex string with no further structure), minted by minted_anomaly_detector_id().
  Why: a client that persists the identifier a create returned, or that validates/parses it before handing it back to DescribeAnomalyDetectors(AnomalyDetectorIds) or DeleteAnomalyDetector, sees an identifier whose form does not match the one the real service issues; any client-side parsing or pattern check calibrated on AWS's form rejects or mis-splits it. The concrete uuid in any one recording is a per-run value - the requirement is the FORM of the minted identifier, not its bytes.
  Fix: the identifier mint that backs this operation - minted_anomaly_detector_id() in the service's anomaly-detector state module - and nothing in the PutAnomalyDetector handler's own logic, which correctly re-reports the stored identifier when a write updates an existing model.
  Traces: cwp-delete-anomaly-detector-describe-empty#task#1.

Cause 2 of 2: DescribeAnomalyDetectors reports the same wrongly-formed id on each AnomalyDetector.
  AWS: Smithy: DescribeAnomalyDetectorsOutput$AnomalyDetectors -> AnomalyDetector$AnomalyDetectorId, same AnomalyDetectorId shape (@length 1..128, @pattern ^[A-Za-z0-9_./:%()+-]+$), "The unique identifier of the anomaly detector." As in cause 1 this is recorded behavior only: the fixture shape-asserts the member and the recorded AWS value is not a bare uuid.
  Emulator: AnomalyDetectors[0].AnomalyDetectorId is the bare random UUIDv4 the write minted, copied through StoredAnomalyDetector::reported(). Status 200 on both sides.
  Why: the read path is what a client enumerates models with, so every identifier it hands a client to delete or re-describe a model carries the non-AWS form; a client that round-trips the listed id into DescribeAnomalyDetectors(AnomalyDetectorIds) is working with an identifier shaped unlike the real service's.
  Fix: the same mint as cause 1; the read projection should keep reporting the stored value unchanged.
  Traces: cwa-range-operator-threshold-metric-id#task#7 (plus cwp-delete-anomaly-detector-describe-empty#task#2).

## Evidence
Cause 1 - PutAnomalyDetector, fixture cwp-delete-anomaly-detector-describe-empty step task#1: expected status 200 and $.AnomalyDetectorId matching the recorded AWS shape ('<shaped-not-asserted>' in the fixture); actual status 200 with $.AnomalyDetectorId = '54d48a59-289e-44a0-9802-f1c790ce7a46' (a bare UUIDv4), reported as verdict=mismatch on key $.AnomalyDetectorId. The specific uuid is a per-run value; the divergence is that a bare uuid does not satisfy the recorded identifier shape.

Cause 2 - DescribeAnomalyDetectors, fixture cwa-range-operator-threshold-metric-id step task#7 (cluster also covers cwp-delete-anomaly-detector-describe-empty#task#2): expected status 200 and $.AnomalyDetectors[0].AnomalyDetectorId matching the recorded shape; actual status 200 with '85bd5402-eb9d-455c-afcd-f712ebcd7618'. Key: service=monitoring|DescribeAnomalyDetectors|status=200->200|keys=$.AnomalyDetectors[0].AnomalyDetectorId.

No HTTP status, error code or error message differs for either cause - both are 200/200 payload-form divergences on an identifier member. Code evidence: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/anomaly_detector_reads.rs:162-174 (minted_anomaly_detector_id -> uuid_from_random_bytes) and src/ops/put_anomaly_detector.rs:226-245 (the handler reports that minted value).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: fixture cwp-delete-anomaly-detector-describe-empty, step task#1 - call PutAnomalyDetector for a single-metric detector and inspect $.AnomalyDetectorId in the 200 response; it is a bare UUIDv4 rather than an identifier of AWS's recorded form.
3. Cause 2: fixture cwa-range-operator-threshold-metric-id, step task#7 - after a detector exists, call DescribeAnomalyDetectors and inspect $.AnomalyDetectors[0].AnomalyDetectorId; the same bare UUIDv4 form is reported back (also exercised by cwp-delete-anomaly-detector-describe-empty#task#2).
4. Re-replay both fixture steps after changing the mint: both clusters must pass, not only the PutAnomalyDetector one.
