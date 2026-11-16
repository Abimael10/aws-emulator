Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis__pipes__sqs.kinesis-source-dead-letter-config-requires-sqs-or-sns
Issue-Key: kinesis__pipes__sqs.kinesis-source-dead-letter-config-requires-sqs-or-sns|run#26|AssertionError|assert terminal["StateReason"] == (
Issue-Commit: 29b39fea7962a85a6fc2f1b2b973f9d82128d2e7

# pipes: the CREATE_FAILED StateReason for a Kinesis-source DeadLetterConfig.Arn that is neither SQS nor SNS is emulator-authored instead of AWS's fixed text

> _Note: this issue was identified against the code base at commit `29b39fea7962a85a6fc2f1b2b973f9d82128d2e7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribePipe

## Description
Cause 1 of 1: a pipe whose Kinesis-stream source carries a DeadLetterConfig.Arn that names something other than an SQS queue or an SNS topic does reach CREATE_FAILED, but the StateReason DescribePipe serves is the emulator's own sentence rather than AWS's.

AWS: recorded behavior only — the Pipes Smithy model types `Pipe$StateReason`/`DescribePipeResponse$StateReason` as a free-form string (PipeStateReason) and documents no text, and CreatePipe accepts the request synchronously (HTTP 200, no ValidationException): the destination-type check happens when the pipe starts. The recording shows the pipe settling into CurrentState `CREATE_FAILED` with StateReason exactly:
  "Input parameter is invalid from the request due to : The service specified by the TargetArn isn't supported as a destination."
That string is entirely fixed text — it has no variable part, and in particular it does not name the offending ARN, the member, or the source type.

Emulator: DescribePipe reaches CREATE_FAILED correctly (that assertion passed) but serves a different StateReason, composed by the emulator and interpolating the request's ARN:
  "DeadLetterConfig.Arn <arn> is not valid. For Amazon Kinesis stream and Amazon DynamoDB stream sources, specify either an Amazon SNS topic or Amazon SQS queue ARN."
(in the replay, <arn> was the Kinesis stream ARN taken from the request). Both the fixed wording and the shape of the message diverge: the emulator substitutes a request value into a message AWS keeps constant.

Why: StateReason is the only machine-readable explanation a client has for an asynchronous pipe failure. Operators and automation match on this text to classify why a pipe failed (and AWS's wording identifies it as an unsupported destination service, not as a DeadLetterConfig syntax problem). A client that matches AWS's sentence sees no match and reports an unknown failure cause; one that parses the emulator's sentence breaks against real AWS. Because the message embeds a per-request ARN, it is also not a stable string to match on at all.

Fix: the pipe stabilization/validation path that sets StateReason when a starting pipe's dead-letter destination type is rejected — it must emit AWS's fixed sentence for the unsupported-destination class instead of an emulator-authored, ARN-interpolated one. The surrounding behavior (CreatePipe accepting synchronously, the pipe settling to CREATE_FAILED, and the pipe remaining visible in ListPipes under its name) is already correct and must be preserved.

Note for the fixer: open issue 20261111T093805Z-pipes-4-net-new-causes-pipe-failure-statereason.md describes emulator-authored StateReason text for other pipe-failure classes (role denial, missing source queue). It does not carry this check's replay key and does not cover this destination-type class, so this is filed separately; a fix there may share the same code path.

Traces: kinesis__pipes__sqs.kinesis-source-dead-letter-config-requires-sqs-or-sns.

## Evidence
Failing check: kinesis__pipes__sqs.kinesis-source-dead-letter-config-requires-sqs-or-sns (verdict: mismatch).

Replay output:
  File "check_kinesis__pipes__sqs_kinesis_source_dead_letter_config_requires_sqs_or_sns.py", line 113, in run
    assert terminal["StateReason"] == (...)
  AssertionError: the CREATE_FAILED pipe's StateReason was 'DeadLetterConfig.Arn arn:aws:kinesis:us-west-2:000000000000:stream/sim2real-kdlq-stream-e95b6235bd9b is not valid. For Amazon Kinesis stream and Amazon DynamoDB stream sources, specify either an Amazon SNS topic or Amazon SQS queue ARN.'

Expected (recorded AWS behavior): CreatePipe 200, then DescribePipe with CurrentState = CREATE_FAILED and StateReason exactly the fixed text
  "Input parameter is invalid from the request due to : The service specified by the TargetArn isn't supported as a destination."
(no variable parts).

Actual (emulator): CreatePipe 200 and CurrentState = CREATE_FAILED (both correct), but StateReason = "DeadLetterConfig.Arn <stream-arn> is not valid. For Amazon Kinesis stream and Amazon DynamoDB stream sources, specify either an Amazon SNS topic or Amazon SQS queue ARN." Fixed text here: everything except <stream-arn>; the ARN shown in the replay (arn:aws:kinesis:us-west-2:000000000000:stream/sim2real-kdlq-stream-...) is a per-run value taken from the request and is not part of any requirement — AWS's message contains no such value at all.

## Steps to Reproduce
1. Create a Kinesis stream and wait for StreamStatus ACTIVE; create an SQS target queue and an IAM role the pipe can assume.
2. Call pipes CreatePipe with Name=<n>, Source=<stream ARN>, Target=<queue ARN>, RoleArn=<role>, DesiredState=RUNNING and SourceParameters.KinesisStreamParameters = {StartingPosition: TRIM_HORIZON, DeadLetterConfig: {Arn: <the same Kinesis stream ARN>}} — i.e. a dead-letter destination that is neither an SQS queue nor an SNS topic. The call is accepted (200).
3. Poll DescribePipe(Name=<n>) until CurrentState leaves CREATING. It becomes CREATE_FAILED, as AWS does.
4. Read StateReason. AWS serves "Input parameter is invalid from the request due to : The service specified by the TargetArn isn't supported as a destination."; the emulator serves its own "DeadLetterConfig.Arn <arn> is not valid. For Amazon Kinesis stream and Amazon DynamoDB stream sources, specify either an Amazon SNS topic or Amazon SQS queue ARN."
