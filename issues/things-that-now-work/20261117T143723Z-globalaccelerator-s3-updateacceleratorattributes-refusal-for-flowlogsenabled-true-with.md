Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: globalaccelerator__s3.flow-logs-bucket-required-to-enable-but-mutable-while-disabled
Issue-Key: globalaccelerator__s3.flow-logs-bucket-required-to-enable-but-mutable-while-disabled|run#7|AssertionError|assert message == "Flow logs s3 bucket should not be empty.", (
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# globalaccelerator__s3: UpdateAcceleratorAttributes' refusal for FlowLogsEnabled=true with no bucket carries an emulator-authored message instead of AWS's

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateAcceleratorAttributes

## Description
Cause 1 of 1: UpdateAcceleratorAttributes(FlowLogsEnabled=true) with no FlowLogsS3Bucket is correctly refused, but the refusal's message is rewritten.

AWS: Smithy (globalaccelerator) models UpdateAcceleratorAttributes with FlowLogsEnabled, FlowLogsS3Bucket and FlowLogsS3Prefix; enabling flow logs without a bucket is an input-validation refusal whose message is the fixed sentence 'Flow logs s3 bucket should not be empty.' — it has no variable parts (no member name, id or value is interpolated). The recorded expectation is exactly that sentence.

Emulator: the message is a different sentence of the emulator's own wording: 'FlowLogsS3Bucket must be specified when FlowLogsEnabled is true.'

Why: the error code may match, but a client that matches on, logs, or surfaces the message (and any test asserting AWS's text) sees wording AWS never emits, so a parity-sensitive caller treats a correct refusal as an unknown one.

Fix: the UpdateAcceleratorAttributes handler's flow-logs input validation — the refusal must carry AWS's fixed message text.

Traces: globalaccelerator__s3.flow-logs-bucket-required-to-enable-but-mutable-while-disabled.

## Evidence
Expected message: 'Flow logs s3 bucket should not be empty.' (fixed text, no variable parts).
Actual message: 'FlowLogsS3Bucket must be specified when FlowLogsEnabled is true.'
Assertion: 'AssertionError: UpdateAcceleratorAttributes(FlowLogsEnabled=True) with no bucket returned message 'FlowLogsS3Bucket must be specified when FlowLogsEnabled is true.''

## Steps to Reproduce
1. CreateAccelerator.
2. Call UpdateAcceleratorAttributes(AcceleratorArn=<arn>, FlowLogsEnabled=True) with no FlowLogsS3Bucket.
3. Read the error's message.
Observed: 'FlowLogsS3Bucket must be specified when FlowLogsEnabled is true.' instead of 'Flow logs s3 bucket should not be empty.'
