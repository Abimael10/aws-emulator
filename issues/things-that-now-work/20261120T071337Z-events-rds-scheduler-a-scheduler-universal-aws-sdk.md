Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__rds__scheduler.xsvc-rds-scheduler-stop-db-instance-denied-without-permission-reaches-rds-with-it
Issue-Key: events__rds__scheduler.xsvc-rds-scheduler-stop-db-instance-denied-without-permission-reaches-rds-with-it|run#25|AssertionError|assert attrs.get("ERROR_CODE") == _DENIED_CODE, (
Issue-Commit: 5f184b6d595470414f1315ced82dbc505d6a982b

# events__rds__scheduler: a Scheduler universal (aws-sdk) rds:stopDBInstance occurrence is never authorized against Target.RoleArn, its Input members are not mapped to the RDS request, and the dead-letter ERROR_CODE/ERROR_MESSAGE carry 'InternalFailure' plus the raw XML ErrorResponse (3 causes)

> _Note: this issue was identified against the code base at commit `5f184b6d595470414f1315ced82dbc505d6a982b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSchedule

## Description
Three net-new root causes under the events__rds__scheduler heading, all on the EventBridge Scheduler occurrence-dispatch path for a universal (aws-sdk) target. All three are grounded in the one failing check events__rds__scheduler.xsvc-rds-scheduler-stop-db-instance-denied-without-permission-reaches-rds-with-it.

Context / relation to what is already filed: the open issue 20261111T033241Z-events-rds-scheduler-an-at-schedule-whose-aws.md carries the replay key for the EARLIER failure of this same check — "nothing arrived within 210s" on the dead-letter queue. In this replay a dead-letter message DOES arrive at or after the at() instant, so that symptom is no longer what fails; the check now fails further on, on the CONTENT of the occurrence and of the dead-letter envelope. The three causes below are those net-new content defects and are not covered by that issue (nor by the open scheduler-family issues 20261109T141116Z-scheduler-delivery-failure-handling-5-causes-deadletterconfig-never.md or 20261112T140641Z-scheduler-4-net-new-behavioral-causes-in-the.md, which are filed under the `scheduler` heading and do not carry this check's key). No refusal of a client-facing operation is involved: every API call the check makes (CreateQueue, CreateRole, PutRolePolicy, CreateSchedule, GetSchedule, ReceiveMessage) succeeds, so this is not a reopen of a fixed issue.

=== Cause 1 of 3: the universal-target Input is not mapped onto the target operation's request, so StopDBInstance is called with a null DBInstanceIdentifier ===
AWS: a schedule whose Target.Arn is the universal target form arn:aws:scheduler:::aws-sdk:<service>:<operation> invokes that SDK operation with Target.Input as the request body, each JSON member of the Input naming an input member of the operation in the SDK's lowercase-first spelling. Smithy (scheduler) Target.Input (TargetInput, @length min 1) — "The text, or well-formed JSON, passed to the target"; Target.Arn (TargetArn, @length 1..1600) is required. Smithy (rds) StopDBInstance input member DBInstanceIdentifier is @required (String), so the Input member DbInstanceIdentifier must arrive at RDS as the request's DBInstanceIdentifier. With a well-formed Input naming an existing identifier the call is a normal StopDBInstance; with an absent instance RDS answers its own modeled refusal DBInstanceNotFoundFault, whose query-protocol code is DBInstanceNotFound (httpError 404) and whose message is "DBInstance <identifier> not found." — fixed text apart from the identifier taken from the request.
Emulator: the occurrence reaches RDS with no DBInstanceIdentifier at all. The dead-letter ERROR_MESSAGE is RDS's own refusal of an empty request: Type=Sender, Code=InvalidParameterValue, Message="1 validation error detected: Value null at 'dBInstanceIdentifier' failed to satisfy constraint: Member must not be null". So the Input's DbInstanceIdentifier member was dropped rather than mapped onto the request parameter; nothing instance-specific ever reaches RDS and the DBInstanceNotFound leg can never occur.
Why: a client using a universal target — the mechanism Scheduler documents for 6,000+ API operations — gets a call that carries none of its parameters. Every universal-target occurrence fails identically on a required-member validation error no matter what the schedule asked for, so the target operation never actually happens, and a client cannot distinguish "my parameters were wrong" from "Scheduler discarded them".
Fix: the Scheduler occurrence dispatcher for universal (aws-sdk) targets — the step that parses Target.Arn into (service, operation) and binds Target.Input's members to that operation's input shape before issuing the call.

=== Cause 2 of 3: the occurrence is never authorized against the identity policy of Target.RoleArn, so a role lacking rds:StopDBInstance reaches RDS instead of being denied ===
AWS: Scheduler assumes Target.RoleArn (Smithy (scheduler) Target.RoleArn, @required, pattern ^arn:aws(-[a-z]+)?:iam::\d{12}:role\/[\w+=,.@\/-]+$ — "the IAM role that EventBridge Scheduler will use for this target when the schedule is invoked") and makes the target call as that role, so the call is authorized against the role's identity policy at invocation time. A role whose only statement is sqs:SendMessage on its dead-letter queue is not authorized for rds:StopDBInstance: the call is refused by IAM before RDS does anything, and the failure is dead-lettered with the query-protocol IAM code ERROR_CODE='AccessDenied' (not 'AccessDeniedException') and ERROR_MESSAGE='User: arn:aws:sts::<account>:assumed-role/<role-name>/<session> is not authorized to perform: rds:StopDBInstance on resource: <db instance ARN> because no identity-based policy allows the rds:StopDBInstance action'. In that message everything is fixed text except the account, the role name, the per-invocation session name Scheduler mints (32 lowercase hex — a per-run value, not part of the requirement) and the instance ARN, all taken from the request and the caller's identity. Because an identity denial is permanent, RETRY_ATTEMPTS is '0'.
Emulator: there is no authorization step at all. The denied role's occurrence is dispatched to RDS, which answers InvalidParameterValue (see Cause 1); the dead-letter reports ERROR_CODE 'InternalFailure'. The role's inline policy — which allows only sqs:SendMessage — has no effect on whether the target call is attempted.
Why: Target.RoleArn is the only authorization boundary a schedule has. A client that relies on it to scope what a schedule may do gets a schedule that performs target calls its role does not permit, and the two failure classes AWS keeps distinct — an identity denial (AccessDenied, never retryable) versus the target service's own refusal (e.g. DBInstanceNotFound) — collapse into one, so least-privilege review, alarms and retry tooling keyed on ERROR_CODE cannot work.
Fix: the Scheduler occurrence dispatcher's assume-role/authorize step for universal (aws-sdk) targets — the invocation must be authorized against the assumed role's identity policy for the target action and resource, and an identity denial routed to the failure path without retries.

=== Cause 3 of 3: the dead-letter ERROR_CODE is the constant 'InternalFailure' and ERROR_MESSAGE is the whole raw XML ErrorResponse document instead of the target error's code and message ===
AWS: when an occurrence's target call fails, Scheduler writes one message to Target.DeadLetterConfig.Arn (Smithy (scheduler) DeadLetterConfig.Arn, ResourceArn, pattern ^arn:aws(-[a-z]+)?:sqs:[a-z0-9\-]+:\d{12}:[a-zA-Z0-9\-_]+$ — "EventBridge Scheduler delivers failed events that could not be successfully delivered to a target to the queue"). Recorded behavior for the envelope: ERROR_CODE is the target failure's own error code as that target's protocol spells it (for RDS's query protocol, the awsQueryError code — e.g. DBInstanceNotFound for DBInstanceNotFoundFault, InvalidParameterValue for a request-validation refusal, AccessDenied for an IAM denial), and ERROR_MESSAGE is only that error's message TEXT, never the serialized error document. The other attributes (SCHEDULE_ARN, TARGET_ARN, RETRY_ATTEMPTS) matched in this replay and are not at issue.
Emulator: ERROR_CODE is 'InternalFailure' even though the underlying failure is a client-class refusal the target service classified itself, and ERROR_MESSAGE is the entire response document verbatim — '<?xml version="1.0"?><ErrorResponse><Error><Type>Sender</Type><Code>InvalidParameterValue</Code><Message>…</Message></Error><RequestId>…</RequestId></ErrorResponse>' — including the XML prolog, the Type/Code elements and a RequestId. So the real code (InvalidParameterValue) is present in the payload but is not lifted into ERROR_CODE, and the message is not extracted from the document.
Why: the dead-letter attributes are the only machine-readable record of why an occurrence failed. 'InternalFailure' tells a client the service broke rather than that its request or its role was refused, which inverts retry decisions (an internal failure invites a retry, a Sender-class refusal must not be retried), and an ERROR_MESSAGE that is an XML document rather than a sentence breaks any consumer that logs, matches or alarms on it.
Fix: the Scheduler dead-letter envelope serializer on the occurrence-failure path — it must classify the target failure and populate ERROR_CODE from the target error's code and ERROR_MESSAGE from that error's message text only.

Traces (all three causes): events__rds__scheduler.xsvc-rds-scheduler-stop-db-instance-denied-without-permission-reaches-rds-with-it.

## Evidence
Failing check: events__rds__scheduler.xsvc-rds-scheduler-stop-db-instance-denied-without-permission-reaches-rds-with-it — [mismatch]

Assertion that failed (check line 99):
  assert attrs.get("ERROR_CODE") == _DENIED_CODE
  AssertionError: ERROR_CODE is 'InternalFailure', expected 'AccessDenied'

Dead-letter message attributes the emulator produced on the denied schedule's DLQ:
  ERROR_CODE    = 'InternalFailure'
  ERROR_MESSAGE = '<?xml version="1.0"?><ErrorResponse><Error><Type>Sender</Type><Code>InvalidParameterValue</Code><Message>1 validation error detected: Value null at &apos;dBInstanceIdentifier&apos; failed to satisfy constraint: Member must not be null</Message></Error><RequestId>…</RequestId></ErrorResponse>'
  TARGET_ARN    = 'arn:aws:scheduler:::aws-sdk:rds:stopDBInstance'   (matches)
  SCHEDULE_ARN  = the denied schedule's ARN                          (matches)
  RETRY_ATTEMPTS= '0'                                               (matches)

Expected on the denied schedule's DLQ (role trusts scheduler.amazonaws.com and allows ONLY sqs:SendMessage on its DLQ):
  ERROR_CODE    = 'AccessDenied'
  ERROR_MESSAGE = 'User: arn:aws:sts::<account>:assumed-role/<role-name>/<session> is not authorized to perform: rds:StopDBInstance on resource: arn:aws:rds:<region>:<account>:db:<identifier> because no identity-based policy allows the rds:StopDBInstance action'
                  Fixed text: 'User: ', ' is not authorized to perform: rds:StopDBInstance on resource: ', ' because no identity-based policy allows the rds:StopDBInstance action'.
                  Values from the request/state: the account, the role name created by the request, the instance ARN built from the Input's identifier. The 32-hex session segment is a per-invocation value the emulator mints and is NOT part of the requirement.
  Body          = the schedule's Target.Input verbatim, {"DbInstanceIdentifier": "<identifier>"}

Expected on the permitted (control) schedule's DLQ — the role allows rds:StopDBInstance on the instance ARN, so the call passes IAM and RDS answers for the absent instance:
  ERROR_CODE    = 'DBInstanceNotFound'   (Smithy rds: DBInstanceNotFoundFault, awsQueryError code DBInstanceNotFound, httpError 404)
  ERROR_MESSAGE = 'DBInstance <identifier> not found.'  (fixed text apart from the identifier taken from the request)
This leg is unreachable in the emulator: because the Input is never mapped (Cause 1) the request RDS sees has DBInstanceIdentifier null, so RDS refuses on the required member instead of reporting the instance missing.

Supporting model facts:
  Smithy (rds) StopDBInstance: input DBInstanceIdentifier @required (String); errors include DBInstanceNotFoundFault {awsQueryError code 'DBInstanceNotFound', httpResponseCode 404}.
  Smithy (scheduler) Target.RoleArn @required, pattern ^arn:aws(-[a-z]+)?:iam::\d{12}:role\/[\w+=,.@\/-]+$ — "the IAM role that EventBridge Scheduler will use for this target when the schedule is invoked".
  Smithy (scheduler) Target.Input (TargetInput, @length min 1) — "The text, or well-formed JSON, passed to the target".
  Smithy (scheduler) DeadLetterConfig.Arn pattern ^arn:aws(-[a-z]+)?:sqs:[a-z0-9\-]+:\d{12}:[a-zA-Z0-9\-_]+$ — "EventBridge Scheduler delivers failed events that could not be successfully delivered to a target to the queue".

## Steps to Reproduce
1. sqs:CreateQueue twice — one dead-letter queue per schedule leg; GetQueueAttributes for each QueueArn.
2. iam:CreateRole twice, both trusting scheduler.amazonaws.com for sts:AssumeRole:
   - denied leg: PutRolePolicy allowing ONLY sqs:SendMessage on its DLQ ARN;
   - control leg: PutRolePolicy allowing rds:StopDBInstance on arn:aws:rds:<region>:<account>:db:<identifier> plus sqs:SendMessage on its DLQ ARN.
   The DB instance <identifier> is deliberately never created.
3. scheduler:CreateSchedule twice with ScheduleExpression at(<now + 30s>), ScheduleExpressionTimezone=UTC, FlexibleTimeWindow {Mode: OFF}, State=ENABLED and Target {Arn: 'arn:aws:scheduler:::aws-sdk:rds:stopDBInstance', RoleArn: <that leg's role>, Input: '{"DbInstanceIdentifier": "<identifier>"}', RetryPolicy {MaximumRetryAttempts: 1, MaximumEventAgeInSeconds: 60}, DeadLetterConfig {Arn: <that leg's DLQ>}}.
4. scheduler:GetSchedule on both (the echo of ScheduleExpression, Target.RoleArn, Target.Input and RetryPolicy already matches today) and wait past the at() instant.
5. sqs:ReceiveMessage with MessageAttributeNames=['All'] on the denied leg's DLQ.
   Expected: exactly one message, ERROR_CODE='AccessDenied', ERROR_MESSAGE the IAM denial sentence for rds:StopDBInstance on the instance ARN, RETRY_ATTEMPTS='0', Body = the Target.Input verbatim.
   Emulator: one message arrives but ERROR_CODE='InternalFailure' and ERROR_MESSAGE is the raw '<?xml version="1.0"?><ErrorResponse>…InvalidParameterValue…Value null at 'dBInstanceIdentifier'…</ErrorResponse>' document — the role was never authorized (Cause 2), the Input member was never mapped onto the RDS request (Cause 1), and the target error's code/message were never extracted into the envelope (Cause 3).
6. sqs:ReceiveMessage with MessageAttributeNames=['All'] on the control leg's DLQ.
   Expected: exactly one message, ERROR_CODE='DBInstanceNotFound', ERROR_MESSAGE='DBInstance <identifier> not found.'
   Emulator: unreachable — the same null-identifier InvalidParameterValue refusal occurs for the permitted role too.
