Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.get-resource-metrics-configuration-malformed-and-unsupported-arn-rejected
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch|GetResourceMetricsConfiguration|NotImplemented
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# Implement cloudwatch GetResourceMetricsConfiguration: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourceMetricsConfiguration

## Description
`GetResourceMetricsConfiguration` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/get_resource_metrics_configuration.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `GetResourceMetricsConfiguration` call, so each one unblocks only when `GetResourceMetricsConfiguration` works: `cloudwatch.get-resource-metrics-configuration-malformed-and-unsupported-arn-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/get_resource_metrics_configuration.rs`. Replaying the corpus, the emulator refused the `GetResourceMetricsConfiguration` call: File "check_cloudwatch_get_resource_metrics_configuration_malformed_and_unsupported_arn_rejected.py", line 48, in run
    expect_error(
  File "check_cloudwatch_get_resource_metrics_configuration_malformed_and_unsupported_arn_rejected.py", line 22, in expect_error
    assert got == (code, message), f"expected {(code, message)!r}, got {got!r}"
           ^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected ('ValidationError', 'Unsupported resource type'), got ('NotImplemented', 'operation GetResourceMetricsConfiguration is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the cloudwatch emulator
Call GetResourceMetricsConfiguration and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
