Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__kinesis.deploy-cfn-kinesis-stream-desired-shard-level-metrics-invalid-value-rejected cloudformation__kinesis.deploy-cfn-kinesis-stream-shard-level-metrics-updated-in-place cloudformation__kinesis.deploy-cfn-kinesis-stream-encryption-disabled-in-place
Issue-Key: cloudformation__kinesis.deploy-cfn-kinesis-stream-desired-shard-level-metrics-invalid-value-rejected|run#11|AssertionError|assert found == want, f"validation errors of the create: expected {want!r}, got {found!r}"
Issue-Key: cloudformation__kinesis.deploy-cfn-kinesis-stream-shard-level-metrics-updated-in-place|_require#8|AssertionError|raise AssertionError(f"stack {stack_id} ended {stack['StackStatus']!r}, expected {want!r}")
Issue-Key: cloudformation__kinesis.deploy-cfn-kinesis-stream-encryption-disabled-in-place|_require#13|AssertionError|raise AssertionError(f"stack {stack_id} ended {stack['StackStatus']!r}, expected {want!r}")
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 3 net-new causes on AWS::Kinesis::Stream in CloudFormation: shard-level metrics are not validated and not provisionable, and disabling stream encryption in place rolls back

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three distinct defects in the CloudFormation AWS::Kinesis::Stream resource handler. Each cause has its own failing check ids.

Cause 1 of 3: an off-enum DesiredShardLevelMetrics value passes pre-provisioning template validation.
AWS: recorded behavior only (the value set is the Kinesis shard-level metric names). The create is refused before anything is provisioned, with one validation error naming the resource type, the offending value and the property path: resource type `AWS::Kinesis::Stream`, message `<value> is not a valid enum value. Supported values: [IncomingBytes, IncomingRecords, OutgoingBytes, OutgoingRecords, WriteProvisionedThroughputExceeded, ReadProvisionedThroughputExceeded, IteratorAgeMilliseconds, ALL]`, property path `/Resources/<LogicalId>/Properties/DesiredShardLevelMetrics/<index>`. Fixed text: the ` is not a valid enum value. Supported values: [...]` sentence and the member list; variable parts: the rejected value, the logical id and the list index, all from the template.
Emulator: no validation errors are reported at all — the list of validation rows is empty.
Why: a template AWS refuses before touching any resource is accepted here, so the error surfaces later (or not at all) and a client never learns which property was wrong.
Fix: the AWS::Kinesis::Stream pre-provisioning property validation (enum constraint on DesiredShardLevelMetrics).
Traces: cloudformation__kinesis.deploy-cfn-kinesis-stream-desired-shard-level-metrics-invalid-value-rejected

Cause 2 of 3: a stack declaring an AWS::Kinesis::Stream with a valid DesiredShardLevelMetrics list cannot be created.
AWS: recorded behavior only. The stack reaches CREATE_COMPLETE and the stream's enhanced monitoring reflects the declared metric list.
Emulator: the stack settles at ROLLBACK_COMPLETE, so the stream is never provisioned.
Why: a client cannot deploy a stream with enhanced shard-level monitoring at all.
Fix: the AWS::Kinesis::Stream resource handler's Create for the DesiredShardLevelMetrics property (it must drive EnableEnhancedMonitoring).
Traces: cloudformation__kinesis.deploy-cfn-kinesis-stream-shard-level-metrics-updated-in-place

Cause 3 of 3: a stack update that removes StreamEncryption from an AWS::Kinesis::Stream is rolled back.
AWS: recorded behavior only. Dropping StreamEncryption is an in-place update: the stack reaches UPDATE_COMPLETE and the stream's encryption type becomes NONE.
Emulator: the update ends UPDATE_ROLLBACK_COMPLETE, so encryption is never disabled and the stack is reverted.
Why: a client cannot turn server-side encryption off through the stack that owns the stream; the update appears to fail and silently reverts.
Fix: the AWS::Kinesis::Stream resource handler's Update path for StreamEncryption removal (StopStreamEncryption).
Traces: cloudformation__kinesis.deploy-cfn-kinesis-stream-encryption-disabled-in-place

## Evidence
Cause 1: AssertionError: validation errors of the create: expected [('AWS::Kinesis::Stream', 'BOGUS is not a valid enum value. Supported values: [IncomingBytes, IncomingRecords, OutgoingBytes, OutgoingRecords, WriteProvisionedThroughputExceeded, ReadProvisionedThroughputExceeded, IteratorAgeMilliseconds, ALL]', '/Resources/Stream/Properties/DesiredShardLevelMetrics/0')], got [].
Cause 2: AssertionError: stack arn:aws:cloudformation:...:stack/<name>/<uuid> ended 'ROLLBACK_COMPLETE', expected 'CREATE_COMPLETE'.
Cause 3: AssertionError: stack arn:aws:cloudformation:...:stack/<name>/<uuid> ended 'UPDATE_ROLLBACK_COMPLETE', expected 'UPDATE_COMPLETE'.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. cloudformation__kinesis.deploy-cfn-kinesis-stream-desired-shard-level-metrics-invalid-value-rejected
2. cloudformation__kinesis.deploy-cfn-kinesis-stream-shard-level-metrics-updated-in-place
3. cloudformation__kinesis.deploy-cfn-kinesis-stream-encryption-disabled-in-place
