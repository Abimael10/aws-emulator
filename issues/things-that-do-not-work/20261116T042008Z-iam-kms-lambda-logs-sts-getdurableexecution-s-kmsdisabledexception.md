Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata
Issue-Key: iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata|run#14|AssertionError|assert error == ("KMSDisabledException", disabled_msg), (
Issue-Commit: ddd7f54964d818ea183d1c749ef5ca4926ba59fa

# iam__kms__lambda__logs__sts: GetDurableExecution's KMSDisabledException message is the emulator's own wording, not AWS's disabled-key text

> _Note: this issue was identified against the code base at commit `ddd7f54964d818ea183d1c749ef5ca4926ba59fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDurableExecution

## Description
Cause 1 of 1: the right error code with the wrong message for a disabled durable-execution CMK.

AWS: Smithy (com.amazonaws.lambda) models GetDurableExecution errors including KMSDisabledException (errorKind server, httpError 502); its IncludeExecutionData member defaults to true. Recorded behavior: with the function's durable-execution KMS key disabled, GetDurableExecution(IncludeExecutionData=true) fails KMSDisabledException whose message is the fixed text "Lambda was unable to access your execution data because the KMS key used is disabled. Check the KMS key configured in the function's durable config. KMS Exception: DisabledException KMS Message: <keyArn> is disabled." — everything but <keyArn> is AWS's fixed wording, and <keyArn> is the key ARN from the function's durable config (the trailing clause is the verbatim KMS DisabledException message).

Emulator: the code is right and the message is its own sentence — "Lambda was unable to decrypt the durable execution data because the KMS key <keyArn> is disabled. Please check the function's KMS key settings." Only the key ARN is taken from state; the whole surrounding sentence, the omission of the "KMS Exception: DisabledException KMS Message: ..." tail and the different opening clause are emulator wording.

Why: a client that distinguishes a disabled key from a missing grant or a deleted key by parsing the message (or that surfaces it to an operator / matches the embedded KMS exception name) matches nothing, even though the error code is correct; the embedded KMS exception name AWS passes through is lost.

Fix: the durable-execution data-access error path's message template for a disabled CMK (the GetDurableExecution / GetDurableExecutionHistory KMS error serializer) — it must reproduce AWS's fixed text and append the underlying KMS exception name and message.

Related: open issue 20261107T212447Z-lambda-5-net-new-causes-async-destinations-inert-durable.md describes this same wrong sentence under the `lambda` service token (its Cause 2, keys lambda.durable-execution-kms-errors / lambda.durable-execution-kms-enforcement-rejected); the check below carries neither of those replay keys, and open issue 20261110T080432Z-iam-kms-lambda-logs-sts-a-durable-execution-invoke-ends-status-failed-where.md is about the execution's Status, not this message.

Traces: iam__kms__lambda__logs__sts.durable-execution-kms-disabled-still-serves-metadata.

## Evidence
Expected (code, message): ('KMSDisabledException', "Lambda was unable to access your execution data because the KMS key used is disabled. Check the KMS key configured in the function's durable config. KMS Exception: DisabledException KMS Message: <keyArn> is disabled.")
Actual (replay): ('KMSDisabledException', "Lambda was unable to decrypt the durable execution data because the KMS key <keyArn> is disabled. Please check the function's KMS key settings.")
The key ARN that filled <keyArn> in this run (arn:aws:kms:us-west-2:000000000000:key/0a15e7f9-...) is a per-run value; the divergence is the fixed text around it.

## Steps to Reproduce
1. Create a customer-managed KMS key and a Lambda function with a durable config using it (role with AWSLambdaBasicExecutionRole + AWSLambdaBasicDurableExecutionRolePolicy).
2. Invoke the durable handler so an execution reaches SUCCEEDED; confirm GetDurableExecution(IncludeExecutionData=false) works and DurableConfig.KMSKeyArn is the key.
3. kms:DisableKey on that key.
4. Call GetDurableExecution(DurableExecutionArn=..., IncludeExecutionData=true) and compare Error.Message with AWS's disabled-key text.
