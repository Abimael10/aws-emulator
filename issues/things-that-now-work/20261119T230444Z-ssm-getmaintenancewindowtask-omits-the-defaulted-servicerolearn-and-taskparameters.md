Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-maintenance-window-lifecycle
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-maintenance-window-lifecycle
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|GetMaintenanceWindowTask|2xx->2xx|$.ServiceRoleArn
Issue-Key: ssm|GetMaintenanceWindowTask|2xx->2xx|$.TaskParameters
Issue-Commit: 360dea7200cfaf15b2eb2bc3d0288559a6d49d3a

# ssm GetMaintenanceWindowTask omits the defaulted ServiceRoleArn and TaskParameters

> _Note: this issue was identified against the code base at commit `360dea7200cfaf15b2eb2bc3d0288559a6d49d3a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMaintenanceWindowTask

## Description
One net-new cause in the SSM maintenance-window read path: GetMaintenanceWindowTask's 200-path projection drops two modeled output members that AWS always materializes for a registered task.

=== Cause 1 of 1: GetMaintenanceWindowTask does not materialize the service-linked ServiceRoleArn or the empty TaskParameters map ===
AWS: Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): GetMaintenanceWindowTask takes WindowId (required, @length 20, @pattern ^mw-[0-9a-f]{17}$) and WindowTaskId (required, @length 36, UUID @pattern); GetMaintenanceWindowTaskResult models ServiceRoleArn -> com.amazonaws.ssm#ServiceRole and TaskParameters -> MaintenanceWindowTaskParameters (a sensitive map); modeled errors DoesNotExistException and InternalServerError (both httpError 400/500). The ServiceRoleArn member is documented: "If you do not specify a service role ARN, Systems Manager uses a service-linked role in your account. If no appropriate service-linked role for Systems Manager exists in your account, it is created when you run RegisterTaskWithMaintenanceWindow." So when RegisterTaskWithMaintenanceWindow is called WITHOUT ServiceRoleArn, the stored task still carries a resolved role ARN and the read projects it: the SSM service-linked role, whose ARN has the fixed form arn:aws:iam::<accountId>:role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM — the only variable part is the account id, taken from the caller's account (the recording's 000000000000 is this environment's account id, not part of the requirement). TaskParameters is a map and is projected as an empty map when the task registered no legacy task parameters; the member is present on the 200 path, not absent.
Emulator: HTTP 200 (the expected status) for the same registered task, but $.ServiceRoleArn is missing entirely (expected the service-linked role ARN, got None) and $.TaskParameters is missing entirely (expected {}, got None).
Why: a client reading back a maintenance-window task cannot learn which IAM role the window will assume when the task runs, so permission diagnosis and any read-modify-write that re-submits the task through UpdateTaskWithMaintenanceWindow silently drops the role binding and changes who the task runs as. A missing TaskParameters map likewise forces consumers that index TaskParameters to handle an absent member rather than reading an empty map, so round-tripping the task description loses the distinction between "no task parameters" and "field not returned".
Fix: the GetMaintenanceWindowTask handler's response projection, plus the registration-time state it reads from — RegisterTaskWithMaintenanceWindow must store the resolved service-linked role ARN when the request omits ServiceRoleArn, and the read's serializer must always emit ServiceRoleArn and a (possibly empty) TaskParameters map. No open issue owns this cluster's replay key; the open issue 20261116T061000Z-ssm-ops-center-maintenance-window-and-association-reads.md covers other maintenance-window causes (DeregisterTargetFromMaintenanceWindow interlock, DescribeMaintenanceWindows Duration/NextExecutionTime) and does not cover this projection, and 20261115T084659Z-ssm-10-net-new-behavioral-causes-maintenance-window.md is a behavioral-suite issue with no fixture step for this read.
Traces: ssm-maintenance-window-lifecycle#task#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, bootstrap_signal false, 907 steps, 755 passed, score 83.2%, 63 divergence clusters). 62 clusters carried an owned_by open issue and were skipped; this is the one cluster with an empty owned_by and no stub_path.

Cluster: service=ssm | GetMaintenanceWindowTask | status=200->200 | keys=$.ServiceRoleArn,$.TaskParameters
Fixture: ssm-maintenance-window-lifecycle, step task#9 (affected_count 1, no other members).
Verdict: mismatch. Expected status 200, actual status 200.
Detail, verbatim from the replay:
  $.ServiceRoleArn: missing (expected 'arn:aws:iam::000000000000:role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM', got None)
  $.TaskParameters: missing (expected {}, got None)

The expected ServiceRoleArn's fixed text is 'arn:aws:iam::' + '<accountId>' + ':role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM'; only the account-id segment is a value from the environment/caller, and the recorded 000000000000 is that environment's value rather than a requirement.

Model evidence (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): GetMaintenanceWindowTaskResult members include ServiceRoleArn (target com.amazonaws.ssm#ServiceRole) and TaskParameters (target MaintenanceWindowTaskParameters, map, sensitive), alongside WindowId, WindowTaskId, Targets, TaskArn, TaskType, TaskInvocationParameters, Priority, MaxConcurrency, MaxErrors, LoggingInfo, Name, Description, CutoffBehavior, AlarmConfiguration. RegisterTaskWithMaintenanceWindow's ServiceRoleArn input member is OPTIONAL, and the GetMaintenanceWindowTaskResult.ServiceRoleArn documentation states Systems Manager substitutes the account's service-linked role when the registration omits one.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Replay fixture ssm-maintenance-window-lifecycle up to step task#9: the fixture creates a maintenance window, registers a task with RegisterTaskWithMaintenanceWindow WITHOUT a ServiceRoleArn and without TaskParameters, then calls GetMaintenanceWindowTask for that WindowId/WindowTaskId.
4. Inspect the 200 response body: expect $.ServiceRoleArn to be the account's SSM service-linked role ARN (arn:aws:iam::<accountId>:role/aws-service-role/ssm.amazonaws.com/AWSServiceRoleForAmazonSSM) and $.TaskParameters to be present as an empty map {}.
5. Observe that both members are absent from the emulator's response.
