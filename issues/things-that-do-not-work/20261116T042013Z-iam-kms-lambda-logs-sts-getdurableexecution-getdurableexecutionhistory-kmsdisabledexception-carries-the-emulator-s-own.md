Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata
Issue-Key: iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata|run#14|AssertionError|assert error == ("KMSDisabledException", disabled_msg), (
Issue-Commit: 8b470128a234e006a6a5db9480cc7fea934afa39

# GetDurableExecution/GetDurableExecutionHistory KMSDisabledException carries the emulator's own sentence instead of AWS's disabled-key message

> _Note: this issue was identified against the code base at commit `8b470128a234e006a6a5db9480cc7fea934afa39`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDurableExecution

## Description
One net-new root cause.

Cause 1 of 1: with a durable execution's customer-managed KMS key disabled, the data-bearing reads (IncludeExecutionData=true) are correctly refused with the KMSDisabledException code, but the message text is the emulator's own wording rather than AWS's message for that error class.

AWS: Smithy (lambda#GetDurableExecution and lambda#GetDurableExecutionHistory, uri /2025-12-01/durable-executions/{DurableExecutionArn}[/history]) models KMSDisabledException with errorKind server / httpError 502, alongside KMSAccessDeniedException, KMSInvalidStateException and KMSNotFoundException. The model does not carry message text, so the message is recorded behavior only: the recording shows AWS answering KMSDisabledException whose message is the fixed sentence pair "Lambda was unable to access your execution data because the KMS key used is disabled. Check the KMS key configured in the function's durable config. KMS Exception: DisabledException KMS Message: <key-arn> is disabled." Everything in that text except <key-arn> is AWS's fixed wording — including the "KMS Exception: DisabledException KMS Message: ..." tail that forwards the underlying KMS exception name and KMS's own sentence; the only variable part is the key ARN, taken from the function's DurableConfig.KMSKeyArn (the recording's arn:aws:kms:us-west-2:<account>:key/<uuid> is one run's value, not part of the requirement). The same message is served for both GetDurableExecution(IncludeExecutionData=true) and GetDurableExecutionHistory(IncludeExecutionData=true).
Emulator: the code is right and the call is refused, but the message is "Lambda was unable to decrypt the durable execution data because the KMS key <key-arn> is disabled. Please check the function's KMS key settings." — the whole sentence pair is emulator wording, with only the key ARN taken from the function's configuration. It omits the KMS-exception forwarding tail ("KMS Exception: DisabledException KMS Message: <key-arn> is disabled.") entirely.
Why: a client that distinguishes a disabled key from a missing grant, a key in another invalid state, or a deleted key by matching the forwarded "KMS Exception: <Name>" / "KMS Message: ..." text (the only place the underlying KMS exception is surfaced) matches nothing, and an operator shown the message is told the data could not be decrypted rather than that the key configured in the function's durable config is disabled.
Fix: the durable-execution error serializer / the KMS-gated read path of the GetDurableExecution and GetDurableExecutionHistory handlers that builds the KMSDisabledException message — the fixed text must be AWS's, with the key ARN read from the function's DurableConfig and the forwarded KMS exception name and KMS message included.
Note on neighbouring issues: the identical wrong sentence is described as part of Cause 2 of open issue 20261107T212447Z-lambda-5-net-new-causes-async-destinations-inert-durable.md, whose replay keys name lambda.durable-execution-kms-errors and lambda.durable-execution-kms-enforcement-rejected and do not carry this check, so this failure is filed here rather than left uncovered. Open issue 20261110T080432Z-iam-kms-lambda-logs-sts-a-durable-execution-invoke-ends-status-failed-where.md carries this check's trace id but is keyed on its earlier Status-SUCCEEDED assertion (run#7), which now passes — the check advanced to the message assertion reported here.
Traces: iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata.

## Evidence
From the behavioral replay of iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata [mismatch]:

AssertionError: expected GetDurableExecution(IncludeExecutionData=True) to fail with ('KMSDisabledException', "Lambda was unable to access your execution data because the KMS key used is disabled. Check the KMS key configured in the function's durable config. KMS Exception: DisabledException KMS Message: <key-arn> is disabled.") once its KMS key was disabled, got ('KMSDisabledException', "Lambda was unable to decrypt the durable execution data because the KMS key <key-arn> is disabled. Please check the function's KMS key settings.")

- Expected error code: KMSDisabledException (Smithy: server error, httpError 502) — matches.
- Expected message: AWS's fixed text "Lambda was unable to access your execution data because the KMS key used is disabled. Check the KMS key configured in the function's durable config. KMS Exception: DisabledException KMS Message: <key-arn> is disabled."; the only variable part is <key-arn>, the function's DurableConfig.KMSKeyArn.
- Actual message: "Lambda was unable to decrypt the durable execution data because the KMS key <key-arn> is disabled. Please check the function's KMS key settings." — different fixed text, and no forwarded "KMS Exception: DisabledException KMS Message: ..." tail.
- The run reached this assertion only after the earlier metadata-only leg passed (terminal Status SUCCEEDED, ExecutionDataIncluded=false, DurableConfig.KMSKeyArn correct), so the divergence is the refusal message alone. The later GetDurableExecutionHistory(IncludeExecutionData=True) assertion, which expects the same message, was not reached.

## Steps to Reproduce
Replay iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata. In outline: create a Lambda execution role trusted by lambda.amazonaws.com with AWSLambdaBasicExecutionRole and AWSLambdaBasicDurableExecutionRolePolicy; create a KMS key whose policy allows lambda.amazonaws.com and that role kms:GenerateDataKey/kms:Decrypt/kms:DescribeKey; CreateFunction (python3.13, a handler using the durable-execution SDK with one @durable_step returned via context.step) with DurableConfig={ExecutionTimeout:900, KMSKeyArn:<key arn>}; PublishVersion; Invoke RequestResponse against that version with DurableExecutionName set and keep the returned DurableExecutionArn; poll GetDurableExecution(IncludeExecutionData=False) to a terminal status; call kms:DisableKey on the key; then call GetDurableExecution(DurableExecutionArn, IncludeExecutionData=True) and compare the Error.Message with AWS's (and likewise GetDurableExecutionHistory(IncludeExecutionData=True)).
