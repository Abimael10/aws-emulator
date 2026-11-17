Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__kms__lambda-web__s3.create-web-function-revision-rejects-a-disabled-kms-key-then-succeeds-once-enabled
Issue-Key: iam__kms__lambda-web__s3.create-web-function-revision-rejects-a-disabled-kms-key-then-succeeds-once-enabled|run#18|AssertionError|raise AssertionError("CreateWebFunctionRevision succeeded with a disabled KMS key")
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# lambda-web CreateWebFunctionRevision accepts a DISABLED customer KMS key — no key-state check, so the revision is created instead of refused ValidationException "<key-arn> is disabled."

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWebFunctionRevision

## Description
Cause 1 of 1: `CreateWebFunctionRevision` performs no KMS key-state validation on the `kmsKeyArn` it is handed, so a revision encrypted under a key that cannot be used is created and settles Active.

Failing check ids for this cause: iam__kms__lambda-web__s3.create-web-function-revision-rejects-a-disabled-kms-key-then-succeeds-once-enabled

AWS: Lambda resolves the customer managed CMK named by `CreateWebFunctionRevision.kmsKeyArn` before it accepts the revision, and refuses the request when that key is not usable. For a key whose KeyState is `Disabled` the call is refused with the modeled client error `ValidationException` (Smithy: lambdaweb#CreateWebFunctionRevision errors include ValidationException, errorKind=client, httpError=400; the operation otherwise returns 202 per `http {method: POST, uri: /2025-03-07/web-functions/{functionName}/revisions, code: 202}`; `kmsKeyArn` is a KmsKeyArn, len 20..2048, pattern ^arn:(aws[a-z-]*):kms:[a-z0-9-]+:[0-9]{12}:key/[a-z0-9-]+$). The model documents ValidationException as "The request failed validation because a parameter was missing, malformed, or outside the allowed range"; it does not spell the disabled-key sentence, so the message is recorded behavior only: the recording shows the message is the key ARN followed by the fixed text " is disabled." — i.e. `<kmsKeyArn> is disabled.`, where the variable part is the `kmsKeyArn` value taken verbatim from the request and the fixed text is " is disabled." (reference the check cites: https://docs.aws.amazon.com/lambda/latest/dg/web-functions-encryption.html). Because the call is refused, NO revision is created: ListWebFunctionRevisions still reports only the revisions that existed before the refused call. Once the key is re-enabled (KMS EnableKey) the byte-identical CreateWebFunctionRevision succeeds, mints a NEW revisionId distinct from every prior one, the revision reaches state Active, and GetWebFunctionRevision reports `kmsKeyArn` equal to the key ARN that was supplied.

Emulator: the request is accepted. `create_web_function_revision` resolves the function, checks only the per-function revision quota, and then stores `kms_key_arn: input.kms_key_arn` verbatim via `state.add_revision(...)` — the CMK is never looked up in KMS and its key state is never consulted. The replayed check calls DisableKey on the CMK and then CreateWebFunctionRevision with that `kmsKeyArn`; the emulator answers 202 with a new revision instead of 400/ValidationException, and the check fails at `raise AssertionError("CreateWebFunctionRevision succeeded with a disabled KMS key")`. The downstream assertions (the `<arn> is disabled.` message, ListWebFunctionRevisions still holding only the initial revision, and recovery after EnableKey) are therefore never reached.

Why: a client that relies on AWS's key-state gate gets the opposite outcome. Code that disables a CMK as a kill switch — or that tests its error handling for an unusable encryption key — sees the deployment go through and a revision reported Active that, on real AWS, could never have been built or served, and the account silently accumulates a revision AWS refused to create. Any retry/backoff logic keyed on ValidationException with the disabled-key message never fires, and the "refuse now, succeed after EnableKey" lifecycle every encryption runbook documents is unobservable.

Fix: the key-state precondition belongs in the CreateWebFunctionRevision handler's validation, before any revision is added to the store — resolve the `kmsKeyArn` against the emulator's KMS state and return the modeled ValidationException (HTTP 400) with the `<kmsKeyArn> is disabled.` message when the key's state is not Enabled, leaving the function's revision list untouched. The same check belongs on any other entry point that takes a revision `kmsKeyArn` (notably CreateWebFunction's revisionConfig), so an unusable key is refused consistently.

Traces: iam__kms__lambda-web__s3.create-web-function-revision-rejects-a-disabled-kms-key-then-succeeds-once-enabled (assertion at check line 251 after the DisableKey at line 238). Handler with no key-state check: services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_revision.rs lines 186-300, store write at lines 253-265.

## Evidence
Check: iam__kms__lambda-web__s3.create-web-function-revision-rejects-a-disabled-kms-key-then-succeeds-once-enabled [mismatch]

Sequence replayed: IAM CreateRole -> S3 CreateBucket (+versioning +bucket policy) -> PutObject app-v1.zip -> KMS CreateKey (symmetric CMK, policy allowing lambda.amazonaws.com kms:Decrypt scoped by aws:SourceArn to this function's revisions) -> lambda-web CreateWebFunction (revision 1 reaches Active) -> KMS DisableKey(KeyId=<key-arn>) -> PutObject app-v2.zip -> lambda-web CreateWebFunctionRevision(functionName=<fn>, kmsKeyArn=<key-arn>, buildConfig=app-v2.zip, serviceConfig=<role>).

Expected (AWS): HTTP 400, Error.Code = "ValidationException", Error.Message = "<key-arn> is disabled." — fixed text " is disabled.", variable part the `kmsKeyArn` echoed from the request. No revision created: ListWebFunctionRevisions returns exactly [<revision_id_1>]. After KMS EnableKey the identical call returns 202 with a NEW revisionId != revision_id_1, that revision reaches state Active, and GetWebFunctionRevision reports kmsKeyArn = <key-arn>.

Actual (emulator): CreateWebFunctionRevision returned success (202) and created a second revision under the disabled key. No ValidationException was raised, so the check aborted:
  File "check_iam__kms__lambda_web__s3_create_web_function_revision_rejects_a_disabled_kms_key_then_succeeds_once_enabled.py", line 228, in run
    raise AssertionError("CreateWebFunctionRevision succeeded with a disabled KMS key")
  AssertionError: CreateWebFunctionRevision succeeded with a disabled KMS key

Code evidence (services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_revision.rs): the handler's only refusals are ResourceNotFoundException for an unknown function (lines 220-231) and ServiceQuotaExceededException for the revision cap (lines 233-249); it then writes `kms_key_arn: input.kms_key_arn` straight into `state.add_revision` (lines 253-265). There is no KMS lookup anywhere in the handler, so no key state (Disabled, PendingDeletion, or absent) can ever be observed.

## Steps to Reproduce
1. IAM: CreateRole with a lambda.amazonaws.com trust policy; note the role ARN.
2. S3: CreateBucket (LocationConstraint when region != us-east-1), PutBucketVersioning Enabled, PutBucketPolicy allowing lambda.amazonaws.com s3:GetObject/s3:GetObjectVersion; PutObject key "app-v1.zip" and "app-v2.zip" with a zip containing index.js.
3. KMS: CreateKey (symmetric, default policy plus a statement letting lambda.amazonaws.com kms:Decrypt scoped by aws:SourceArn arn:aws:lambda:<region>:<account>:web-function/<fn>/revision/*); note KeyMetadata.Arn as <key-arn>.
4. lambda-web: CreateWebFunction(functionName=<fn>, revisionConfig={buildConfig:{codeConfig:{s3Object:{bucket,key:"app-v1.zip"}},runtimeConfig:{runtime:"nodejs24.x"}}, serviceConfig:{executionRoleArn:<role>}}, endpointConfig={endpointName:"home",endpointType:"HomeRegion",authType:"ApplicationManaged"}); poll GetWebFunctionRevision until the first revision is Active.
5. KMS: DisableKey(KeyId=<key-arn>).
6. lambda-web: CreateWebFunctionRevision(functionName=<fn>, kmsKeyArn=<key-arn>, buildConfig={codeConfig:{s3Object:{bucket,key:"app-v2.zip"}},runtimeConfig:{runtime:"nodejs24.x"}}, serviceConfig:{executionRoleArn:<role>}).
   -> AWS: 400 ValidationException, message "<key-arn> is disabled.", and no new revision.
   -> Emulator: 202 with a new revisionId.
7. lambda-web: ListWebFunctionRevisions(functionName=<fn>) — AWS lists only the initial revision; the emulator lists two.
8. KMS: EnableKey(KeyId=<key-arn>), then repeat step 6 — AWS now returns 202 with a new revisionId that reaches Active and reports kmsKeyArn=<key-arn>.
