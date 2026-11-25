Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.maintenance-window-task-full-lifecycle
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.maintenance-window-task-full-lifecycle|run#28|AssertionError|assert get_message == f"Maintenance window task {task_id} does not exist", get_message
Issue-Commit: 512d9228f1967f96aef73bc4796a27c6865a9663

# ssm: the DoesNotExistException message for a WindowTaskId that names no task of the window is the emulator's own sentence, not AWS's "Maintenance window task <id> does not exist"

> _Note: this issue was identified against the code base at commit `512d9228f1967f96aef73bc4796a27c6865a9663`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMaintenanceWindowTask

## Description
One net-new root cause in the ssm service, grounded in one failing behavioral check that the replay report marks neither ALREADY FILED nor OWNED.

=== Cause 1 of 1: every maintenance-window-task read/mutate addressed by a WindowTaskId the window does not hold reports a non-AWS DoesNotExistException message ===

AWS: Smithy com.amazonaws.ssm#GetMaintenanceWindowTask — input WindowId (MaintenanceWindowId, required, @length 20, @pattern ^mw-[0-9a-f]{17}$) and WindowTaskId (MaintenanceWindowTaskId, required, @length 36, 8-4-4-4-12 hex UUID @pattern); modeled client error DoesNotExistException (httpError 400, awsQueryError code DoesNotExistException), documented as "Error returned when the ID specified for a resource, such as a maintenance window or patch baseline, doesn't exist". com.amazonaws.ssm#UpdateMaintenanceWindowTask takes the same two required members and models the same error. Recorded behavior: once a task has been deregistered (or was never registered with that window), both GetMaintenanceWindowTask and UpdateMaintenanceWindowTask on that id are refused HTTP 400, Error.Code DoesNotExistException, Error.Message "Maintenance window task <WindowTaskId> does not exist" — fixed text "Maintenance window task " + " does not exist" with the single variable part being the WindowTaskId copied from the request; the WindowId does not appear in it. The window itself still exists in this flow, so the window-level not-found sentence is not the one in play.

Emulator: the status (400) and the code (DoesNotExistException) are right, but the message is the emulator's own invented sentence: "Maintenance window <WindowId> has no task <WindowTaskId>" — different fixed text, and it additionally interpolates the WindowId, which AWS's message never carries. The text is produced in one place, services/service-com-amazonaws-ssm-amazonssm/src/state/maintenance_window_tasks.rs::task_not_found_message (lines 167-173, format!("Maintenance window {window_id} has no task {window_task_id}")), and that helper is what the task-addressing handlers pass into DoesNotExistException.message — so Get, Update and any other operation that resolves a WindowTaskId through it all diverge the same way from the same root cause.

Why: the error message is part of the operation's contract. A client that distinguishes a dead task id from a dead window id by the refusal text (SDK error-message matching, CDK/Terraform resource-import and drift paths, operator runbooks and log assertions all do) sees a sentence AWS never emits, cannot match it, and treats the refusal as unclassified. Because the code is correct, the divergence is silent until something reads the message.

Fix: the shared not-found message helper for maintenance-window tasks — task_not_found_message in the maintenance-window-task state module — must render AWS's sentence, "Maintenance window task " + the WindowTaskId from the request + " does not exist", and must not interpolate the WindowId. The handlers that call it (the GetMaintenanceWindowTask and UpdateMaintenanceWindowTask handlers, and any other task-addressing operation using the same helper) then need no change; the window-level not-found message helper is a separate sentence and is not in scope. File and line citations are under Evidence.

Traces: ssm.maintenance-window-task-full-lifecycle (the whole register → get → describe → update → deregister lifecycle succeeds; only the two dead-id refusal messages diverge). Note: the open issue 20261123T180339Z-ssm-4-net-new-behavioral-causes-the-public.md covers the sibling check ssm.maintenance-window-task-update-and-schedule, whose failure is CancelMaintenanceWindowExecution answering 501 NotImplemented; it does not carry this check's replay key and says nothing about this message, so it does not cover this cause. 20261122T161603Z-ssm-2-net-new-causes-a-maintenance-window.md covers window EXECUTIONS never being minted, also a different cause.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 467/639 passed (73.1%); emulator serving (no bootstrap signal). Service heading `### ssm`; verdict [mismatch].

ssm.maintenance-window-task-full-lifecycle:
  File "check_ssm_maintenance_window_task_full_lifecycle.py", line 80, in run
    assert get_message == f"Maintenance window task {task_id} does not exist", get_message
  AssertionError: Maintenance window mw-83f11fd9b8c671e0e has no task 8e90fcb8-b58b-4ed0-b0fa-c19f09c98812

(The ids above are one run's generated values and are not part of the requirement; the requirement is the template.)

Sequence the check ran, all of it successful up to the assertion: CreateMaintenanceWindow(Schedule="cron(0 0 ? * SUN *)", Duration=2, Cutoff=1, AllowUnassociatedTargets=True) → RegisterTargetWithMaintenanceWindow(ResourceType=INSTANCE) → RegisterTaskWithMaintenanceWindow(TaskArn="AWS-RunShellScript", TaskType=RUN_COMMAND, Targets=[{Key: WindowTargetIds, Values:[<target id>]}], MaxConcurrency="1", MaxErrors="1", Priority=1) → GetMaintenanceWindowTask (TaskArn/Priority/MaxConcurrency all correct) → DescribeMaintenanceWindowTasks (task listed) → UpdateMaintenanceWindowTask(Priority=5) (partial update correct, untouched fields intact) → DeregisterTaskFromMaintenanceWindow (task gone from the listing).

The divergence is only in the two refusals that follow:
  GetMaintenanceWindowTask(WindowId=<the live window>, WindowTaskId=<the deregistered task>)
    expected: HTTP 400, DoesNotExistException, Message "Maintenance window task <WindowTaskId> does not exist"
    actual:   HTTP 400, DoesNotExistException, Message "Maintenance window <WindowId> has no task <WindowTaskId>"
  UpdateMaintenanceWindowTask(WindowId=<same>, WindowTaskId=<same>, Priority=9)
    expected and actual identical to the above (the check asserts the same message for it at line 154; the run stopped at the Get assertion first).
The error CODE assertion (`assert get_code == "DoesNotExistException"`) passed, so only the message text is wrong.

Code producing it:
  services/service-com-amazonaws-ssm-amazonssm/src/state/maintenance_window_tasks.rs:171-173 — pub fn task_not_found_message(window_id, window_task_id) -> format!("Maintenance window {window_id} has no task {window_task_id}")
  services/service-com-amazonaws-ssm-amazonssm/src/ops/get_maintenance_window_task.rs:244-258 — the `else` branch of store.maintenance_window_task(..) returns DoesNotExistException { message: Some(maintenance_window_tasks::task_not_found_message(&input.window_id, &input.window_task_id)) }
  The UpdateMaintenanceWindowTask handler resolves the same id through the same helper.

Model citations: GetMaintenanceWindowTask / UpdateMaintenanceWindowTask / DeregisterTaskFromMaintenanceWindow each model DoesNotExistException (client, httpError 400); DoesNotExistException carries a single member Message (String).

## Steps to Reproduce
1. git_clone the repo, build and boot the emulator, replay the simparity-behavioral-aws-v1 behavioral suite.
2. aws ssm create-maintenance-window --name <unique> --schedule "cron(0 0 ? * SUN *)" --duration 2 --cutoff 1 --allow-unassociated-targets  → note WindowId.
3. aws ssm register-target-with-maintenance-window --window-id <WindowId> --resource-type INSTANCE --targets Key=tag:<any-key>,Values=true  → note WindowTargetId.
4. aws ssm register-task-with-maintenance-window --window-id <WindowId> --task-arn AWS-RunShellScript --task-type RUN_COMMAND --targets Key=WindowTargetIds,Values=<WindowTargetId> --max-concurrency 1 --max-errors 1 --priority 1 --task-invocation-parameters '{"RunCommand":{"Parameters":{"commands":["echo hi"]}}}'  → note WindowTaskId.
5. aws ssm deregister-task-from-maintenance-window --window-id <WindowId> --window-task-id <WindowTaskId>.
6. aws ssm get-maintenance-window-task --window-id <WindowId> --window-task-id <WindowTaskId>. Observe HTTP 400 DoesNotExistException with Message "Maintenance window <WindowId> has no task <WindowTaskId>"; AWS answers HTTP 400 DoesNotExistException with Message "Maintenance window task <WindowTaskId> does not exist" (no WindowId in the text).
7. aws ssm update-maintenance-window-task --window-id <WindowId> --window-task-id <WindowTaskId> --priority 9. Observe the same wrong message where AWS emits the same sentence as step 6.
8. Re-replay ssm.maintenance-window-task-full-lifecycle to confirm the cause is closed.
