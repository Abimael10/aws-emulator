Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-maintenance-window-lifecycle
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-maintenance-window-lifecycle
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|UpdateMaintenanceWindowTask|2xx->2xx|$.ServiceRoleArn
Issue-Key: ssm|UpdateMaintenanceWindowTask|2xx->2xx|$.TaskParameters
Issue-Commit: cd8f73d475d74adb0ca8a470eb19f8c740cf481a

# ssm UpdateMaintenanceWindowTask response omits the defaulted ServiceRoleArn and TaskParameters

> _Note: this issue was identified against the code base at commit `cd8f73d475d74adb0ca8a470eb19f8c740cf481a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateMaintenanceWindowTask

## Description
One net-new cause in the SSM maintenance-window write path: the UpdateMaintenanceWindowTask 200-path projection drops two modeled output members that AWS always materializes for a registered task.

=== Cause 1 of 1: UpdateMaintenanceWindowTask does not echo the resolved service-linked ServiceRoleArn or the empty TaskParameters map ===
AWS: Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): UpdateMaintenanceWindowTask takes WindowId (required, @length 20/20, @pattern ^mw-[0-9a-f]{17}$) and WindowTaskId (required, @length 36/36, UUID @pattern) plus the optional task fields (Targets, TaskArn, ServiceRoleArn -> com.amazonaws.ssm#ServiceRole, TaskParameters -> MaintenanceWindowTaskParameters (map, sensitive), TaskInvocationParameters, Priority, MaxConcurrency, MaxErrors, LoggingInfo, Name, Description, Replace, CutoffBehavior, AlarmConfiguration); modeled errors DoesNotExistException and InternalServerError (httpError 400 / 500). UpdateMaintenanceWindowTaskResult models the updated task back, and it models BOTH ServiceRoleArn (target ServiceRole) and TaskParameters (target MaintenanceWindowTaskParameters, map, sensitive). The result's ServiceRoleArn member documentation states the rule: "The Amazon Resource Name (ARN) of the IAM service role for Amazon Web Services Systems Manager to assume when running a maintenance window task. If you do not specify a service role ARN, Systems Manager uses a service-linked role in your account. If no appropriate service-linked role for Systems Manager exists in your account, it is created when you run RegisterTaskWithMaintenanceWindow." So when the task was registered (and is updated) WITHOUT an explicit ServiceRoleArn, the accepted update still resolves to the account's SSM service-linked role and echoes it. That ARN's fixed text is 'arn:aws:iam::' + ':role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM', with the account-id segment the only variable part, taken from the caller's account (the recording's 000000000000 is this environment's account id, not part of the requirement). TaskParameters is a map and is projected as an empty map {} on the 200 path when the task carries no legacy task parameters — the member is PRESENT and empty, not absent.
Emulator: HTTP 200 (the expected status) for the accepted update of the registered task, but $.ServiceRoleArn is missing entirely (expected the account's service-linked role ARN, got None) and $.TaskParameters is missing entirely (expected {}, got None). Status, error code and every other member matched; only these two modeled members are dropped.
Why: a client that does a read-modify-write on a maintenance-window task uses the Update response as the authoritative post-update view. Because the response never names the IAM role the window will assume, the caller cannot confirm which identity the task now runs as, and a caller that feeds the Update response back into another UpdateMaintenanceWindowTask (the documented Replace flow) silently drops the role binding and rewrites who the task runs as. A missing TaskParameters map likewise erases the distinction between "this task has no task parameters" (an empty map) and "the server did not return the field", so consumers that index TaskParameters break against the emulator although they work against AWS.
Fix: the UpdateMaintenanceWindowTask handler's 200-path response projection/serializer, together with the registration state it reads from — the stored task must carry the resolved service-linked role ARN when neither the registration nor the update supplies ServiceRoleArn, and the serializer must always emit ServiceRoleArn and a (possibly empty) TaskParameters map. Related but NOT covering this: the open issue 20261119T230444Z-ssm-getmaintenancewindowtask-omits-the-defaulted-servicerolearn-and-taskparameters.md reports the same two members dropped by GetMaintenanceWindowTask (fixture step ssm-maintenance-window-lifecycle#task#9); it carries only that read's replay key, so this write-path cluster is unowned and is filed separately — a fixer should expect a shared root cause in the maintenance-window task state/projection and must close BOTH replay keys.
Traces: ssm-maintenance-window-lifecycle#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, bootstrap_signal false, 907 steps, 813 passed, 93 mismatched, 1 blocked, score 89.6%, 58 divergence clusters). 57 clusters carried an owned_by open issue and were skipped; no cluster carried a stub_path. This is the single cluster with an empty owned_by.

Cluster: service=ssm | UpdateMaintenanceWindowTask | status=200->200 | keys=$.ServiceRoleArn,$.TaskParameters
Fixture: ssm-maintenance-window-lifecycle, step task#11 (affected_count 1 — ssm-maintenance-window-lifecycle#task#11, no other members).
Verdict: mismatch. Expected status 200, actual status 200.
Detail, verbatim from the replay:
  $.ServiceRoleArn: missing (expected 'arn:aws:iam::000000000000:role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM', got None)
  $.TaskParameters: missing (expected {}, got None)

Fixed vs variable text in the expected value: 'arn:aws:iam::' and ':role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM' are the fixed text of the SSM service-linked role ARN; the account-id segment is a value taken from the caller's account, and the recorded 000000000000 is this environment's account id, not a requirement. TaskParameters' expected value is an empty map, i.e. the member present and empty.

Model evidence (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): UpdateMaintenanceWindowTaskResult members are WindowId, WindowTaskId, Targets, TaskArn, ServiceRoleArn (target com.amazonaws.ssm#ServiceRole), TaskParameters (target MaintenanceWindowTaskParameters, map, sensitive), TaskInvocationParameters, Priority (default 0), MaxConcurrency, MaxErrors, LoggingInfo, Name, Description, CutoffBehavior, AlarmConfiguration. ServiceRoleArn is OPTIONAL on input for both RegisterTaskWithMaintenanceWindow and UpdateMaintenanceWindowTask, and the result member's documentation states Systems Manager substitutes the account's service-linked role when none is specified.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Call replay_wire_corpus() to replay the simparity-wire-aws-v1 wire corpus.
3. Replay fixture ssm-maintenance-window-lifecycle through step task#11: the fixture creates a maintenance window, registers a task with RegisterTaskWithMaintenanceWindow WITHOUT a ServiceRoleArn and without TaskParameters, then calls UpdateMaintenanceWindowTask for that WindowId/WindowTaskId.
4. Inspect the 200 response body of that UpdateMaintenanceWindowTask call: expect $.ServiceRoleArn to be the account's SSM service-linked role ARN (arn:aws:iam::<accountId>:role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM) and $.TaskParameters to be present as an empty map {}.
5. Observe both members are absent from the emulator's response, while every other member and the 200 status match.
