Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.concurrency-scheduled-audit-name-conflict-rejected
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.concurrency-scheduled-audit-name-conflict-rejected|run#17|AssertionError|assert err == ("ResourceAlreadyExistsException", (
Issue-Commit: d1977ef89f0c5c4dfc7f04e8d3c082237c1f7677

# iot CreateScheduledAudit duplicate-name ResourceAlreadyExistsException message is emulator-authored instead of AWS's ARN-bearing sentence (1 cause)

> _Note: this issue was identified against the code base at commit `d1977ef89f0c5c4dfc7f04e8d3c082237c1f7677`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateScheduledAudit

## Description
Cause 1 of 1: CreateScheduledAudit refuses a name the account already holds a scheduled audit under with the right error code and status, but with the emulator's own wording instead of AWS's. Failing check: iot.concurrency-scheduled-audit-name-conflict-rejected.

AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json) models CreateScheduledAudit POST /audit/scheduledaudits/{scheduledAuditName} with ResourceAlreadyExistsException (errorKind client, httpError 409, "The resource already exists") and with scheduledAuditName as the schedule's identity (@length 1..128, @pattern ^[a-zA-Z0-9_-]+$). The model does not state the message text; per the recorded behavior the refusal message for a re-created scheduled audit name is the fixed sentence "Resource already exists for account with different attributes, " followed by the ARN of the scheduled audit the account already holds. The fixed text is "Resource already exists for account with different attributes,"; the only variable part is that ARN, built as arn:aws:iot:<region>:<account-id>:scheduledaudit/<scheduledAuditName>, whose region/account come from the serving endpoint's credentials and whose name is the scheduledAuditName from the request. No other value from the request or the stored record appears in the message.

Emulator: the same request is refused as ResourceAlreadyExistsException (code and 409 are correct) but with the message "ScheduledAudit cannot be created - name already exists (name=<scheduledAuditName>)" — a differently-worded sentence that omits the resource ARN entirely and instead interpolates the bare name in a parenthesised key=value tail. Reported by the replay as AssertionError ('ResourceAlreadyExistsException', 'ScheduledAudit cannot be created - name already exists (name=<name>)') against the expected ('ResourceAlreadyExistsException', 'Resource already exists for account with different attributes, arn:aws:iot:<region>:<account>:scheduledaudit/<name>').

Why: a client that classifies or surfaces IoT conflicts by message — AWS's own console/CLI text, log scrapers, and tests that assert on the AWS sentence — sees text AWS never emits, and loses the ARN AWS puts in the message, which is the only place the conflicting resource's full identity (partition, region, account, resource path) is reported for this refusal. Error-message-matching retry/branch logic diverges even though the code and HTTP status agree.

Fix: the already-exists message builder this operation's conflict branch uses — crate::state::existing_scheduled_audit_message, called from the duplicate-name branch of the CreateScheduledAudit handler — must produce AWS's fixed sentence and interpolate the existing schedule's ARN (the same ARN the record carries / scheduled_audit_arn computes) rather than the name. Only the message is wrong; the error shape, code and 409 status are already right, and no state is written on the refusal, which is also correct.

Traces: iot.concurrency-scheduled-audit-name-conflict-rejected (mismatch). Related but distinct, and NOT this cause: open issue 20261116T040307Z-iot-createscheduledaudit-pattern-refusal-drops-the-offending-value.md covers the @pattern InvalidRequestException wording for the same operation; this cause is the duplicate-name 409 message.

## Evidence
Failing check: iot.concurrency-scheduled-audit-name-conflict-rejected [mismatch]

Assertion in check_iot_concurrency_scheduled_audit_name_conflict_rejected.py line 69:
  assert err == ("ResourceAlreadyExistsException",
      "Resource already exists for account with different attributes, "
      f"arn:aws:iot:{ctx.region}:{ctx.account}:scheduledaudit/{audit_name}")

Expected (AWS): code ResourceAlreadyExistsException, HTTP 409, message
  "Resource already exists for account with different attributes, arn:aws:iot:<region>:<account>:scheduledaudit/<scheduledAuditName>"
  (fixed text: "Resource already exists for account with different attributes,"; variable: the ARN, whose region/account come from the caller's credentials and whose name is the request's scheduledAuditName)

Actual (emulator): code ResourceAlreadyExistsException, message
  "ScheduledAudit cannot be created - name already exists (name=<scheduledAuditName>)"

Source of the diverging text: services/service-com-amazonaws-iot-awsiotservice/src/ops/create_scheduled_audit.rs lines 228-241, which words the conflict with crate::state::existing_scheduled_audit_message(&existing.scheduled_audit_name) and sets resource_id/resource_arn to None.

Model: CreateScheduledAudit errors include ResourceAlreadyExistsException (client, httpError 409).

## Steps to Reproduce
1. Boot the emulator and point an iot client at it.
2. iot.update_account_audit_configuration(roleArn=<an IAM role assumable by iot.amazonaws.com>, auditCheckConfigurations={"DEVICE_CERTIFICATE_EXPIRING_CHECK": {"enabled": True}}).
3. iot.create_scheduled_audit(scheduledAuditName=<NAME>, frequency="DAILY", targetCheckNames=["DEVICE_CERTIFICATE_EXPIRING_CHECK"]) -> succeeds.
4. Call create_scheduled_audit again with the SAME scheduledAuditName but frequency="WEEKLY", dayOfWeek="MON", same targetCheckNames.
5. Observe the ClientError: Code is ResourceAlreadyExistsException (correct) but Message is "ScheduledAudit cannot be created - name already exists (name=<NAME>)" instead of AWS's "Resource already exists for account with different attributes, arn:aws:iot:<region>:<account>:scheduledaudit/<NAME>".
6. Re-replay check iot.concurrency-scheduled-audit-name-conflict-rejected to confirm a fix.
