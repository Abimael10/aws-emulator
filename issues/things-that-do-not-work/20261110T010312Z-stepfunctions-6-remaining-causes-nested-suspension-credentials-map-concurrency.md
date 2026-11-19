Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.sfn-parallel-branch-task-local-catch-handles-failure stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume stepfunctions.deep-sfn-2-sfn-jsonpath-timeout-caught-by-jsonata-catch stepfunctions.jsonata-task-error-two-levels-deep-propagates-through-map-and-parallel stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap stepfunctions.map-maxconcurrency-runtime-overflow-and-negative-path-fail-with-states-runtime stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution stepfunctions.express-sync-machine-timeoutseconds-shorter-than-wait-yields-timed-out
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.sfn-parallel-branch-task-local-catch-handles-failure|run#9|AssertionError|assert desc["status"] == "SUCCEEDED", (
Issue-Key: stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume|run#8|AssertionError|assert good["status"] == "SUCCEEDED", (
Issue-Key: stepfunctions.deep-sfn-2-sfn-jsonpath-timeout-caught-by-jsonata-catch|_claim_task#5|AssertionError|raise AssertionError(f"no task claimed from {activity_arn!r} within {timeout_s}s")
Issue-Key: stepfunctions.jsonata-task-error-two-levels-deep-propagates-through-map-and-parallel|_poll_activity_task#8|AssertionError|raise AssertionError(f"no activity task arrived on {activity_arn!r} within {timeout_s}s")
Issue-Key: stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap|run#5|AssertionError|assert few_count == _FEW_ITEMS, (
Issue-Key: stepfunctions.map-maxconcurrency-runtime-overflow-and-negative-path-fail-with-states-runtime|_assert_states_runtime#7|AssertionError|assert desc.get("cause") == expected_cause, f"{label}: unexpected cause: {desc.get('cause')!r}"
Issue-Key: stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space|run#7|AssertionError|assert distinct_resp["cause"] == (
Issue-Key: stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution|run#9|AssertionError|assert over_resp["cause"] == (
Issue-Key: stepfunctions.express-sync-machine-timeoutseconds-shorter-than-wait-yields-timed-out|run#12|AssertionError|assert elapsed >= _TIMEOUT_S, (
Issue-Commit: 7552cf7cb82a23878f689a33468aa3be5f606804

# stepfunctions: the causes still open after the Assign/Catch/Credentials bundle — a task that must WAIT inside a Map iteration or a Parallel branch, Task Credentials, Map concurrency, and four refusal wordings

> _This report carries the causes of
> `20261110T010311Z-stepfunctions-10-net-new-causes-assign-catch-credentials.md`
> that the change which resolved the rest of that bundle did NOT fix. Nothing here is
> claimed to work; each cause states what the next change has to build._

## Operation
StartExecution

## Description
Six causes remain, in three groups: a task whose answer comes from OUTSIDE the execution
cannot suspend inside a concurrent state, a `Task` may not be made under another role,
and a `Map` state's items do not really run concurrently. Four further divergences are
refusal WORDINGS, where the status and the error name already match.

Cause 1 of 6: an ACTIVITY task (and a callback task) cannot SUSPEND inside a `Map`
iteration or a `Parallel` branch.
AWS: a `Task` inside a `Parallel` branch may schedule an activity task and the branch
waits for the worker; a Task-level `Catch` on such a task then absorbs its failure and
the `Parallel` (declaring no `Catch` of its own) SUCCEEDS.
Emulator: the execution ends FAILED `States.Runtime` cause `state '<Name>' schedules an
activity task inside a 'Map' iteration or a 'Parallel' branch, which this emulator does
not wait at, so nothing was run for it`
(`services/service-com-amazonaws-sfn-awsstepfunctions/src/state/asl/walk.rs`, the
`self.depth > 0` refusal in `activity_task_state`, and the matching one for
`.waitForTaskToken` in `task_state`).
Why: a nested state list is walked to its own END inside the enclosing state's turn, so
only the run-a-job pattern can park there today — it carries the concurrent states it is
nested inside on the parked record (`ParkedJob::frames` / `ParkedFrame`) and the
continuation re-enters them (`asl.rs`, `Entry::Job`).
Fix: carry those frames on `ScheduledActivity`, on the stored `ActivityTask` and on
`ActivityReply`/`CallbackReply`, and re-enter them in `interpret` for `Entry::Reply` and
`Entry::Callback` exactly as `Entry::Job` does; then a branch that suspends reports its
frame from `run_branches` as a parked one does.
Traces: stepfunctions.sfn-parallel-branch-task-local-catch-handles-failure,
stepfunctions.jsonata-task-error-two-levels-deep-propagates-through-map-and-parallel,
stepfunctions.deep-sfn-2-sfn-jsonpath-timeout-caught-by-jsonata-catch

Cause 2 of 6: the Task `Credentials` member is not interpreted.
AWS: a Task may carry `Credentials.RoleArn` (a literal, a `.$` path, or an intrinsic over
a variable); Step Functions resolves it, assumes that role, and makes the integration
call under it.
Emulator: FAILED `States.Runtime` cause `state '<Name>' carries the member 'Credentials',
which this emulator does not interpret, so nothing was run for it`.
Why: resolving the ARN is the easy half. The call must then be made AS that role: both
the gate that decides whether the call may be made (`integration::Tasks::authorize`, a
closure over the EXECUTION role) and the credential it is signed with
(`task_authorization::acting`'s `signing`) are built from the state machine's execution
role for the whole walk, so a per-task role needs the role carried on
`integration::Call` and the gate judging that role's policies after an assume the
execution role must itself be allowed to make.
Fix: `Credentials` in the `Task` member sets, its resolution in `task_state`, and the
assume plus the per-call role in the task seam.
Traces: stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume

Cause 3 of 6: a `Map` state's items do not run concurrently.
AWS: `MaxConcurrency: 0` means unbounded, so ten items run at once (observed in-flight
10), and a larger item count plateaus at the service's own concurrency rather than at 1.
Emulator: with `MaxConcurrency` 0 over ten items the plateaued in-flight count is 1.
Why: the items of one chunk are interleaved by the logical clock
(`src/state/asl/map_items.rs`), and only the integration CALLS they make are really in
flight together (`Tasks::begin`); work that is not such a call is therefore serial.
Fix: the `Map` state's concurrency scheduler, for the work its items do besides an
integration call.
Traces: stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap

Cause 4 of 6: the cause an out-of-range `MaxConcurrency` fails with is not the recorded
one.
AWS: the status (`FAILED`) and the error (`States.Runtime`) now match; the recorded CAUSE
differs, and the replay does not disclose it — it reports only that the cause it holds is
not the one this emulator words.
Emulator: `An error occurred while executing the state '<Name>' (entered at the event id
#N). The value referenced by MaxConcurrency must be a whole number from 0 to 2147483647,
but the value was: 9.223372036854776e+18` — the sentence family this service words every
member-value rejection in, with the member the definition wrote, the range the member
carries and the value found.
Fix: the sentence the recording holds for this failure class, which needs the recorded
cause to be read off the trace.
Traces: stepfunctions.map-maxconcurrency-runtime-overflow-and-negative-path-fail-with-states-runtime

Cause 5 of 6: the cause an `Assign` past the variable space fails with is not the
recorded one.
AWS: the status (`FAILED`) and the error (`States.DataLimitExceeded`) now match for a
scope past the 262144 bytes an execution's variables may hold, and for one variable past
what a document may carry; both recorded CAUSES differ, and the replay discloses neither.
Emulator: `the variables this state assigns leave more than the 262144 bytes of variables
an execution may hold in scope` for the scope — the quota it went past, stated the way
every other ceiling of this module is — and the service's own result sentence
`The state/task '<Name>' returned a result with a size exceeding the maximum number of
bytes service limit.` for the single variable.
Fix: the two sentences the recording holds for these failure classes, which needs the
recorded causes to be read off the traces.
Traces: stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space,
stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution

Cause 6 of 6: `StartSyncExecution` answers before the run's own spans have passed.
AWS: a synchronous Express execution blocks for the spans its definition declares, so a
machine whose `TimeoutSeconds` is 5 answers after about five seconds with status/error
`States.Timeout` (the status and the error now match).
Emulator: the call returns after about 0.02 s, with the later `stopDate` the walk
computed.
Why: the walk computes a run's whole logical clock in the call that starts it and never
sleeps for it; an asynchronous execution's outcome is made due at an instant the store
settles to, and the synchronous call has no such seam.
Fix: hold the synchronous answer until the instant the walk made the outcome due.
Traces: stepfunctions.express-sync-machine-timeoutseconds-shorter-than-wait-yields-timed-out

## Evidence
1) `cause="state 'AlwaysTimesOut' schedules an activity task inside a 'Map' iteration or a 'Parallel' branch…"`, where the Parallel must have SUCCEEDED; and `no task claimed from '<activity arn>' within 20s` for an activity task scheduled two levels deep.
2) `cause="state 'T' carries the member 'Credentials', which this emulator does not interpret, so nothing was run for it"`.
3) `with only 10 items (well under the internal cap), MaxConcurrency 0 must run every one of them at once; the plateaued in-flight count was 1`.
4) `literal overflow, INLINE: unexpected cause: "An error occurred while executing the state 'M' (entered at the event id #2). The value referenced by MaxConcurrency must be a whole number from 0 to 2147483647, but the value was: 9.223372036854776e+18"`.
5) `unexpected cause: 'the variables this state assigns leave more than the 262144 bytes of variables an execution may hold in scope'`; `unexpected cause: "The state/task 'PassState' returned a result with a size exceeding the maximum number of bytes service limit."`.
6) `the synchronous call must block for the full TimeoutSeconds=5 budget before returning; it returned after only 0.02s`.

## Steps to Reproduce
1. A `Parallel` whose branch `Task` names an activity ARN and declares its own `Catch`; separately a `Map` whose iteration's `Task` names one.
2. A `Task` with `Credentials: {"RoleArn.$": "States.Format(…)"}` naming a role the execution role may assume.
3. An inline `Map` with `MaxConcurrency` 0 over ten slow items; measure the in-flight count.
4. An inline `Map` with a `MaxConcurrency` literal past `2147483647`.
5. A JSONata `Assign` of two distinct variables that together pass 262144 bytes, and one variable past it.
6. An EXPRESS machine whose `TimeoutSeconds` is shorter than its `Wait`, started with `StartSyncExecution`; measure how long the call takes.
