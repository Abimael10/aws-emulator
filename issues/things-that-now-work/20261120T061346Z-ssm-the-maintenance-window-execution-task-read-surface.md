Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.maintenance-window-execution-lookup-nonexistent-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.maintenance-window-execution-lookup-nonexistent-rejected|run#14|AssertionError|assert task_code == "DoesNotExistException", (
Issue-Commit: afcdf7be57b3fef95e777182b2b5896511e2ffa2

# ssm: the maintenance-window execution-task read surface answers 501 NotImplemented instead of DoesNotExistException / empty listings

> _Note: this issue was identified against the code base at commit `afcdf7be57b3fef95e777182b2b5896511e2ffa2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMaintenanceWindowExecutionTask

## Description
One net-new SSM fidelity defect found by replaying the behavioral suite against this checkout. This bundle carries 1 cause; a fixer must close it before submitting this issue. Smithy model used: services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json.

Cause 1 of 1: the maintenance-window EXECUTION-TASK read surface is unserved — GetMaintenanceWindowExecutionTask (and, on the same code path, GetMaintenanceWindowExecutionTaskInvocation, DescribeMaintenanceWindowExecutionTasks and DescribeMaintenanceWindowExecutionTaskInvocations) answer HTTP 501 with error code NotImplemented, so neither the modeled not-found refusal nor the empty-listing behavior of this surface can be observed.
Failing check ids for this cause: ssm.maintenance-window-execution-lookup-nonexistent-rejected.

- AWS: Smithy GetMaintenanceWindowExecutionTask input WindowExecutionId + TaskId (both required, MaintenanceWindowExecutionId / MaintenanceWindowExecutionTaskId, @length 36..36, @pattern ^[0-9a-fA-F]{8}\-[0-9a-fA-F]{4}\-[0-9a-fA-F]{4}\-[0-9a-fA-F]{4}\-[0-9a-fA-F]{12}$); modeled errors DoesNotExistException (client, httpError 400) and InternalServerError (server, 500). GetMaintenanceWindowExecutionTaskInvocation adds a required InvocationId with the same constraints and models the same two errors. For a well-formed but nonexistent execution/task (or execution/task/invocation) id, AWS refuses with HTTP 400, error code DoesNotExistException and the message "Resource does not exist in the data store." — that sentence is entirely fixed text for this error class and carries NO value from the request (recorded behavior; the Smithy doc for DoesNotExistException only says the id specified for a resource such as a maintenance window or patch baseline doesn't exist). The two listing operations DescribeMaintenanceWindowExecutionTasks (output WindowExecutionTaskIdentities) and DescribeMaintenanceWindowExecutionTaskInvocations (output WindowExecutionTaskInvocationIdentities) model the same DoesNotExistException but, per the recording, SUCCEED with HTTP 200 and an EMPTY identity list for a nonexistent execution/task rather than refusing.
- Emulator: GetMaintenanceWindowExecutionTask answers error code 'NotImplemented' for the nonexistent execution/task pair (the operation is not served at all). Its handler is still the scaffold: services/service-com-amazonaws-ssm-amazonssm/src/ops/get_maintenance_window_execution_task.rs returns Err(crate::scaffolded::Scaffolded::not_implemented("GetMaintenanceWindowExecutionTask")) and still carries the SCAFFOLDED marker; get_maintenance_window_execution_task_invocation.rs and describe_maintenance_window_execution_tasks.rs are in the same state, so the rest of this check's calls are unreachable too. The sibling GetMaintenanceWindowExecution on the same check DID answer 400 DoesNotExistException with AWS's sentence (the check's positive control and first refusal assertion passed), so this is scoped to the execution-TASK level of the surface, not to maintenance windows in general.
- Why: a client cannot distinguish "that execution/task does not exist" from "this emulator cannot read maintenance-window execution tasks". 501/NotImplemented is not a modeled error of the operation, so SDK error handling that branches on DoesNotExistException (to stop polling, to fall back to listing, or to report a vanished execution) never takes its branch and instead raises an unmodeled, non-retryable server error; and because the two Describe listings are unserved as well, no caller can enumerate the tasks or invocations of ANY maintenance-window execution, so run history for maintenance windows is entirely unavailable.
- Fix: implement the four handlers in the ssm service crate over the maintenance-window execution store — services/service-com-amazonaws-ssm-amazonssm/src/ops/get_maintenance_window_execution_task.rs, .../get_maintenance_window_execution_task_invocation.rs, .../describe_maintenance_window_execution_tasks.rs and .../describe_maintenance_window_execution_task_invocations.rs — returning 400 DoesNotExistException with AWS's fixed sentence for an unknown id on the two Get operations, an empty identity list (HTTP 200) for an unknown execution/task on the two Describe operations, and deleting the SCAFFOLDED marker lines once served.
- Traces: ssm.maintenance-window-execution-lookup-nonexistent-rejected.
- Related open issues: 20261111T143427Z-ssm-getmaintenancewindowexecution-stub.md covers GetMaintenanceWindowExecution only (which now answers the refusal correctly in this replay), and 20261115T084659Z-ssm-10-net-new-behavioral-causes-maintenance-window.md covers maintenance-window mutation/deregistration state; neither covers the execution-task read operations named here.

## Evidence
Behavioral suite replay of this checkout: 384/639 checks passed (60.1%); 171 mismatched, 84 errored, bootstrap_signal false (the emulator served every call below). Service heading 'ssm': 201/309 passed, 108 failed. This cause is grounded in the one failing check the report marked neither 'ALREADY FILED' nor 'OWNED by'.

Cause 1 — ssm.maintenance-window-execution-lookup-nonexistent-rejected [mismatch]:
  AssertionError: expected DoesNotExistException from GetMaintenanceWindowExecutionTask for a nonexistent execution/task pair, got NotImplemented

Sequence in the check: CreateMaintenanceWindow(Schedule='rate(30 days)', Duration=1, Cutoff=0, AllowUnassociatedTargets=True) succeeds; GetMaintenanceWindow on the new WindowId echoes it (positive control passes, so the account is not refusing everything); GetMaintenanceWindowExecution(WindowExecutionId=<random uuid>) is refused DoesNotExistException with Message 'Resource does not exist in the data store.' (passes); GetMaintenanceWindowExecutionTask(WindowExecutionId=<random uuid>, TaskId=<random uuid>) returns error code 'NotImplemented' where AWS returns 400 DoesNotExistException with that same fixed sentence. The uuids are per-run values; the requirement is the status, code and fixed message text for a well-formed id that resolves to nothing. The check's later calls — GetMaintenanceWindowExecutionTaskInvocation (expect 400 DoesNotExistException, same sentence), DescribeMaintenanceWindowExecutionTasks (expect 200 with WindowExecutionTaskIdentities == []) and DescribeMaintenanceWindowExecutionTaskInvocations (expect 200 with WindowExecutionTaskInvocationIdentities == []) — were never reached, and their handler files are the same unimplemented scaffold.

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral suite (simparity-behavioral-aws-v1), or run the named check individually:
1. ssm.CreateMaintenanceWindow(Name=<unique>, Schedule='rate(30 days)', Duration=1, Cutoff=0, AllowUnassociatedTargets=True) and keep the WindowId; GetMaintenanceWindow(WindowId) to confirm the control path works.
2. Generate three random uuid4 strings: FAKE_EXEC, FAKE_TASK, FAKE_INVOCATION.
3. ssm.GetMaintenanceWindowExecution(WindowExecutionId=FAKE_EXEC) — observe 400 DoesNotExistException, Message 'Resource does not exist in the data store.' (already correct).
4. ssm.GetMaintenanceWindowExecutionTask(WindowExecutionId=FAKE_EXEC, TaskId=FAKE_TASK) — expect 400 DoesNotExistException with Message 'Resource does not exist in the data store.'; observe error code 'NotImplemented' (HTTP 501).
5. ssm.GetMaintenanceWindowExecutionTaskInvocation(WindowExecutionId=FAKE_EXEC, TaskId=FAKE_TASK, InvocationId=FAKE_INVOCATION) — expect the same 400 DoesNotExistException refusal.
6. ssm.DescribeMaintenanceWindowExecutionTasks(WindowExecutionId=FAKE_EXEC) — expect HTTP 200 with WindowExecutionTaskIdentities == []; and ssm.DescribeMaintenanceWindowExecutionTaskInvocations(WindowExecutionId=FAKE_EXEC, TaskId=FAKE_TASK) — expect HTTP 200 with WindowExecutionTaskInvocationIdentities == [].
7. Clean up with DeleteMaintenanceWindow(WindowId).
