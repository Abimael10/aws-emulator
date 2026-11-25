Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.send-task-failure-kms-access-denied
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.send-task-failure-kms-access-denied|run#18|AssertionError|assert stable == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: 9b644f2075576c170e34ded6fb832e3c5e4e0784

# stepfunctions: SendTaskFailure's KmsAccessDeniedException message names kms:GenerateDataKey instead of the kms:Decrypt the callback actually requires

> _Note: this issue was identified against the code base at commit `9b644f2075576c170e34ded6fb832e3c5e4e0784`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendTaskFailure

## Description
Cause 1 of 1: when SendTaskFailure is refused because the state machine's customer-managed key denies crypto, the emulator relays a KMS denial naming the WRONG action — "is not authorized to perform: kms:GenerateDataKey" — where AWS names kms:Decrypt, the action the callback path performs on the execution's data key.
Failing check id: stepfunctions.send-task-failure-kms-access-denied.

AWS: Smithy (services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json) models SendTaskFailure{taskToken (required, @length 1..2048, @sensitive), error (@length 0..256, @sensitive), cause (@length 0..32768, @sensitive)} -> empty output, errors InvalidToken | KmsAccessDeniedException | KmsInvalidStateException | KmsThrottlingException | TaskDoesNotExist | TaskTimedOut, all HTTP 400; KmsAccessDeniedException is documented as "Either your KMS key policy or API caller does not have the required permissions." For an execution of a CUSTOMER_MANAGED_KMS_KEY state machine, accepting a task callback requires Step Functions to DECRYPT the execution's stored data key as the execution role's STS session, so when the key policy explicitly denies crypto the call is refused HTTP 400 KmsAccessDeniedException whose message is the relayed KMS denial. Its fixed text is "User: <principal> is not authorized to perform: kms:Decrypt on resource: <key-arn> with an explicit deny in a resource-based policy (Service: Kms, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)" — every word there is fixed, including the action name kms:Decrypt and the explicit-deny clause for a resource-based (key-policy) deny. The variable parts are <principal>, the execution role's assumed-role session ARN arn:aws:sts::<account>:assumed-role/<role-name>/<session> built from the machine's roleArn plus the session Step Functions mints per call; <key-arn>, taken from the machine's encryptionConfiguration.kmsKeyId; and the per-call <request-id> and <n>.

Emulator: the status (400), the error code (KmsAccessDeniedException), the principal, the key ARN and the explicit-deny clause are all right, but the action in the message is kms:GenerateDataKey: 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session> is not authorized to perform: kms:GenerateDataKey on resource: <key-arn> with an explicit deny in a resource-based policy (Service: Kms, Status Code: 400, Request ID: <id>) (SDK Attempt Count: 1)'. So the callback path reports the data-key GENERATION grant it would need to write new ciphertext rather than the DECRYPT grant required to read the existing execution data key. (The <session>, <request-id> and <n> parts are masked by the check and are not part of the divergence.)

Why: a client relying on AWS's behavior cannot diagnose or remediate the denial from the message it is handed. The documented triage for KmsAccessDeniedException is to read the action out of the relayed KMS message and grant exactly that action on the key — an operator told kms:GenerateDataKey adds a grant that does not unblock the callback, while the missing kms:Decrypt stays missing. Automation that parses the action out of the message (to classify key-policy gaps, or to distinguish a decrypt-side from an encrypt-side denial) is routed to the wrong remedy.

Fix: the SendTaskFailure handler's KMS authorization step for CUSTOMER_MANAGED_KMS_KEY executions, and specifically the KMS-denial message builder it relays — the authorized action for a callback that reads the execution's data key is kms:Decrypt, so that is the action name the relayed denial must carry (the same applies to the sibling callback operations SendTaskSuccess and SendTaskHeartbeat, which model the identical KMS error set). Related but NOT covering this check (the replay report marked it neither ALREADY FILED nor OWNED): 20261116T073653Z-kms-stepfunctions-sendtasksuccess-on-a-cmk-encrypted-execution.md covers SendTaskSuccess performing NO KMS authorization at all for an identity-policy revocation; here SendTaskFailure does authorize and does refuse, and only the action named in the relayed key-policy denial is wrong.

Traces: stepfunctions.send-task-failure-kms-access-denied.

## Evidence
Check stepfunctions.send-task-failure-kms-access-denied, verdict mismatch.

Setup performed by the check: kms:CreateKey (KeyUsage=ENCRYPT_DECRYPT); sfn:CreateActivity; an IAM role trusting states.amazonaws.com with an inline policy allowing [kms:Decrypt, kms:GenerateDataKey] on the key ARN; sfn:CreateStateMachine(type=STANDARD, single activity Task with TimeoutSeconds=180, encryptionConfiguration={type: CUSTOMER_MANAGED_KMS_KEY, kmsKeyId: <key ARN>, kmsDataKeyReusePeriodSeconds: 60}); sfn:StartExecution; sfn:GetActivityTask to claim the taskToken. Only then kms:PutKeyPolicy rewrites the key policy to Allow kms:* to account root plus an explicit Deny of kms:Encrypt/Decrypt/ReEncryptFrom/ReEncryptTo/GenerateDataKey/GenerateDataKeyWithoutPlaintext to Principal "*", and the check waits out the 60 s kmsDataKeyReusePeriodSeconds (+10 s) so the callback must obtain a fresh data key against the denied policy.

Expected (AWS): sfn:SendTaskFailure(taskToken=<token>, error="BoomWithKms", cause="needs encryption") is refused HTTP 400 with Error.Code "KmsAccessDeniedException" and Error.Message 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session> is not authorized to perform: kms:Decrypt on resource: <key ARN> with an explicit deny in a resource-based policy (Service: Kms, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)'. The check masks exactly <session>, <request-id> and <n>; every other character is graded, and the role name and key ARN come from the machine's roleArn and encryptionConfiguration.

Actual (emulator): the code is correct (KmsAccessDeniedException) but the message names the wrong action. The harness reports:
  File "check_stepfunctions_send_task_failure_kms_access_denied.py", line 251, in run
    assert stable == expected
  AssertionError: expected message 'User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: kms:Decrypt on resource: arn:aws:kms:<region>:<account>:key/<key-id> with an explicit deny in a resource-based policy (Service: Kms, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)', got 'User: arn:aws:sts::<account>:assumed-role/<role>/<session-id> is not authorized to perform: kms:GenerateDataKey on resource: arn:aws:kms:<region>:<account>:key/<key-id> with an explicit deny in a resource-based policy (Service: Kms, Status Code: 400, Request ID: <id>) (SDK Attempt Count: 1)'
The sole difference is the action token: kms:GenerateDataKey where AWS has kms:Decrypt.

## Steps to Reproduce
1. kms:CreateKey(KeyUsage=ENCRYPT_DECRYPT) -> <key ARN>, <key id>.
2. sfn:CreateActivity(name=<act>) -> <activity ARN>.
3. iam:CreateRole trusting Service states.amazonaws.com for sts:AssumeRole; iam:PutRolePolicy(PolicyName="sfn-kms") allowing Action ["kms:Decrypt","kms:GenerateDataKey"] on <key ARN>.
4. sfn:CreateStateMachine(name=<sm>, type=STANDARD, roleArn=<role>, definition={"StartAt":"T","States":{"T":{"Type":"Task","Resource":"<activity ARN>","TimeoutSeconds":180,"End":true}}}, encryptionConfiguration={"type":"CUSTOMER_MANAGED_KMS_KEY","kmsKeyId":"<key ARN>","kmsDataKeyReusePeriodSeconds":60}).
5. sfn:DescribeStateMachine to read roleArn (the role name appears in the expected message); sfn:StartExecution(input={"probe":1}); sfn:GetActivityTask(activityArn=<activity ARN>) until a taskToken is returned.
6. kms:PutKeyPolicy(KeyId=<key id>, PolicyName="default") with Allow kms:* to arn:aws:iam::<account>:root plus an explicit Deny to Principal {"AWS":"*"} of [kms:Encrypt, kms:Decrypt, kms:ReEncryptFrom, kms:ReEncryptTo, kms:GenerateDataKey, kms:GenerateDataKeyWithoutPlaintext] on Resource "*"; poll kms:GenerateDataKey until it is refused AccessDeniedException.
7. Wait longer than kmsDataKeyReusePeriodSeconds (60 s) so no cached data key can be reused.
8. sfn:SendTaskFailure(taskToken=<token>, error="BoomWithKms", cause="needs encryption").
   Expected: HTTP 400, Error.Code KmsAccessDeniedException, Error.Message 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session> is not authorized to perform: kms:Decrypt on resource: <key ARN> with an explicit deny in a resource-based policy (Service: Kms, Status Code: 400, Request ID: <request-id>) (SDK Attempt Count: <n>)'.
   Observed: same status and code, but the action in the message is kms:GenerateDataKey.
