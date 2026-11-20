Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__logs__sqs.esm-scaling-config-floor-and-overflow-then-clear
Issue-Key: lambda|UpdateEventSourceMapping|ResourceInUseException
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# Lambda UpdateEventSourceMapping refuses a settled SQS mapping with ResourceInUseException, so ScalingConfig can never be changed or cleared

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateEventSourceMapping

## Description
Cause 1 of 1: an update of an existing event source mapping is rejected as in use.
AWS: recorded behavior only for the timing — Lambda's contract is that UpdateEventSourceMapping on an ENABLED mapping is accepted (the mapping moves to Updating and settles back to Enabled with the new configuration); ResourceConflictException/ResourceInUseException is AWS's answer only while a mapping is genuinely mid-transition, not for a settled one. The scenario needs ScalingConfig.MaximumConcurrency to be set to its floor, raised past the overflow bound (refused on the VALUE, not on the resource's state) and then cleared.
Emulator: UpdateEventSourceMapping answers ResourceInUseException 'Cannot update the event source mapping because it is in use.' for the settled mapping, so no ScalingConfig change — valid, invalid or clearing — can be made at all.
Why: a client can create an ESM but never reconfigure it; every tuning/rollback path (concurrency, batch size, enable/disable) is permanently refused, and value-level validation is unreachable behind the state-level refusal.
Fix: the UpdateEventSourceMapping handler's state gate — the in-use refusal must apply only to a mapping actually in a transitional state, and the mapping's lifecycle must settle out of that state after create/update.
Failing check: iam__lambda__logs__sqs.esm-scaling-config-floor-and-overflow-then-clear

## Evidence
[error] iam__lambda__logs__sqs.esm-scaling-config-floor-and-overflow-then-clear — botocore.errorfactory.ResourceInUseException: An error occurred (ResourceInUseException) when calling the UpdateEventSourceMapping operation: Cannot update the event source mapping because it is in use. Expected: the update accepted (and, for the out-of-range MaximumConcurrency, a value-level refusal) with the mapping settling back to Enabled.

## Steps to Reproduce
1. Create a function and an SQS event source mapping and wait for it to settle (State Enabled).
2. Call UpdateEventSourceMapping with ScalingConfig={'MaximumConcurrency': 2} (the floor), then with a value past the overflow bound, then with ScalingConfig={} to clear it.
