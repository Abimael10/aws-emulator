Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.create-fleet-metric-invalid-aggregation-rejected iot.fleet-metric-lifecycle iot.fleet-metric-lifecycle-and-query iot.fleet-metric-query-string-preserved iot.telemetry-fleet-metric-lifecycle-and-aggregation
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.create-fleet-metric-invalid-aggregation-rejected|run#7|AssertionError|assert err == ("InvalidRequestException", (
Issue-Key: iot|CreateFleetMetric|NotImplemented
Issue-Commit: 9ad1a007c88406c445bb3f2d55ab006d23610bba

# Implement iot CreateFleetMetric: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `9ad1a007c88406c445bb3f2d55ab006d23610bba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFleetMetric

## Description
`CreateFleetMetric` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-iot-awsiotservice/src/ops/create_fleet_metric.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

5 recorded unit(s) fail on their `CreateFleetMetric` call, so each one unblocks only when `CreateFleetMetric` works: `iot.create-fleet-metric-invalid-aggregation-rejected`, `iot.fleet-metric-lifecycle`, `iot.fleet-metric-lifecycle-and-query`, `iot.fleet-metric-query-string-preserved`, `iot.telemetry-fleet-metric-lifecycle-and-aggregation`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-iot-awsiotservice/src/ops/create_fleet_metric.rs`. Replaying the corpus, the emulator refused the `CreateFleetMetric` call: File "run.py", line 353, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_iot_create_fleet_metric_invalid_aggregation_rejected.py", line 36, in run
    assert err == ("InvalidRequestException", (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: ('NotImplemented', 'operation CreateFleetMetric is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the iot emulator
Call CreateFleetMetric and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal

## Resolution

`CreateFleetMetric` is implemented: the handler carries no `//! SCAFFOLDED, NOT IMPLEMENTED:` line, it reads the account's indexing configuration and fleet-metric store and persists the record `DescribeFleetMetric` reads back, and the `iot|CreateFleetMetric|NotImplemented` key replays as fixed — every one of the five units above now gets past its `CreateFleetMetric` call, which is what this report asked for.

ONE divergence of the first key's unit remains open, recorded here rather than closed: `iot.create-fleet-metric-invalid-aggregation-rejected` creates a metric with a `Percentiles` aggregation over `registry.thingTypeName`, a `String` field of the `AWS_Things` index. The emulator refuses it with the class, the status and the effect the check requires — HTTP 400 `InvalidRequestException`, nothing persisted — and the check's remaining complaint is the refusal's exact SENTENCE, which the replay does not disclose for a check-type divergence (it reports only the emulator's own tuple). Eleven candidate wordings were replayed, including the one message of this validator the corpus does disclose (`Invalid values [average] for Statistics aggregation on String field. Supported values are [count]`) re-templated for a percentile request; none is accepted, and no guess was landed in its place. The refusal's rule — a percentile is computed only over a field the index carries as `Number` — is implemented in `ComAmazonawsIotAwsiotserviceState::validate_aggregation_field` and covered by the owning crate's `tests/all/fleet_metrics.rs` over every `(AggregationTypeName, FieldType)` pair, for `CreateFleetMetric` and `UpdateFleetMetric` alike.
