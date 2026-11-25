Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space stepfunctions.execution-role-trust-source-arn-naming-another-account-blocks-task
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume|run#14|AssertionError|assert bad.get("cause") == (
Issue-Key: stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap|run#5|AssertionError|assert few_count == _FEW_ITEMS, (
Issue-Key: stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution|run#9|AssertionError|assert over_resp["cause"] == (
Issue-Key: stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space|run#7|AssertionError|assert distinct_resp["cause"] == (
Issue-Key: stepfunctions.execution-role-trust-source-arn-naming-another-account-blocks-task|_start_sync_execution#14|Timeout|return sfn.start_sync_execution(stateMachineArn=sm_arn, input="{}")
Issue-Commit: a28aa3cbb761848919217b218a8a95a512704746

# stepfunctions: 5 net-new causes — Credentials assume denial leaks STS's own text, Map MaxConcurrency 0 runs items serially, the three variable-size DataLimitExceeded sentences and the 10 MiB execution cap, and a foreign-SourceArn execution-role trust never settles

> _Note: this issue was identified against the code base at commit `a28aa3cbb761848919217b218a8a95a512704746`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Five distinct root causes in the Step Functions interpreter, all reached through CreateStateMachine + StartExecution/StartSyncExecution/DescribeExecution on plain state machines. They are separate work items; all five must be closed.

Context for the fixer: four of these five failing checks are named in the Issue-Trace-Ids of the RESOLVED issue 20261110T010312Z-stepfunctions-6-remaining-causes-nested-suspension-credentials-map-concurrency.md (its causes 2, 3, 4 and 5), which is in the resolved folder but whose work these checks still fail on. That report explicitly could not disclose AWS's recorded sentences for its causes 4 and 5; the four exact templates are quoted below, read off the failing checks themselves. No API call here is refused, so nothing is being reopened — the status and the error name already match in every cause; what diverges is the cause text, the concurrency and the accounting.

Cause 1 of 5: the cause of a refused Task-Credentials assume is STS's own AccessDeniedException text instead of Step Functions' sentence. Failing check id: stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume.
AWS:      recorded behavior only for the text — Smithy com.amazonaws.sfn#DescribeExecutionOutput models only the generic `error` (SensitiveError, @length 0-256) and `cause` (SensitiveCause, @length 0-32768). The recording shows a Task whose `Credentials.RoleArn.$` resolves (through States.Format over a variable) to a role the execution role may not assume fail with status FAILED, error `States.TaskFailed` and cause exactly `The role <execution-role ARN> is not authorized to assume the task state's role, <target-role ARN>.` Fixed text: `The role `, ` is not authorized to assume the task state's role, ` and the closing period; variable parts: the state machine's roleArn and the Task's resolved Credentials.RoleArn.
Emulator: status FAILED and error States.TaskFailed match, but the cause is the underlying STS refusal passed straight through: `User: arn:aws:sts::<account>:assumed-role/<exec-role>/<session> is not authorized to perform: sts:AssumeRole on resource: <target-role ARN> because no identity-based policy allows the sts:AssumeRole action (Service: AWSSecurityTokenService; Status Code: 403; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)` — an IAM sentence plus an SDK-v1-style suffix, where AWS states its own one-line sentence naming the two roles. (The resolve-through-an-intrinsic positive control now SUCCEEDS and the task does run AS the resolved role, so this is past cause 2 of the resolved bundle.)
Why:      a client that matches the cause to tell "this task's role could not be assumed" from any other task failure sees a different sentence, and the leaked STS text names sts:AssumeRole on the execution role's session instead of the Task's role — an operator reading it looks for the wrong misconfiguration.
Fix:      the Task-credentials assume seam's cause serializer in the interpreter — on a refused assume, word AWS's sentence from the execution role and the resolved task role instead of forwarding the STS error.
Traces:   stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume.

Cause 2 of 5: a Map state's items do not run concurrently — MaxConcurrency 0 runs them one at a time. Failing check id: stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap.
AWS:      recorded behavior only (MaxConcurrency is an ASL field, not a member of the Smithy API model). The recording shows MaxConcurrency 0 means UNBOUNDED: with 10 items whose ItemProcessor is a 20-second Wait, the observed in-flight count plateaus at 10 (every item at once), and with 150 such items it plateaus at the service's own internal concurrency cap, not at 1.
Emulator: with 10 items under MaxConcurrency 0 the plateaued in-flight count is 1 — the iterations are serialized, so the Map takes items x 20s instead of 20s. The 150-item arm was not reached.
Why:      a client relying on AWS's behavior gets wall-clock times longer by a factor of the item count, so a Map AWS finishes well inside the execution's TimeoutSeconds instead TIMED_OUTs, and no concurrency-sensitive behavior (ordering, throttling, aggregate throughput) can be observed.
Fix:      the Map state's concurrency scheduler for the work its items do besides an integration call (the resolved report named services/service-com-amazonaws-sfn-awsstepfunctions/src/state/asl/map_items.rs, where one chunk's items are interleaved by the logical clock, and Tasks::begin, where only integration calls are truly in flight).
Traces:   stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap. (The OPEN issue 20261123T063602Z-stepfunctions-2-net-new-causes-map-maxconcurrency-maxconcurrencypath.md covers out-of-range MaxConcurrency VALIDATION, a different cause, and does not carry this check.)

Cause 3 of 5: the three variable-size States.DataLimitExceeded causes are not AWS's — an over-cap Assign is reported with the result-size sentence. Failing check id: stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution.
AWS:      recorded behavior only. Status FAILED and error `States.DataLimitExceeded` already match; the cause is one of three sentences, one per scope, each quoted here as fixed text with its variable parts filled from the definition and the evaluated values (a value's size is its JSON serialization, quotes included):
  - one variable past the 262,144-byte per-variable cap: `Variable size limit exceeded. Variable '<name>' in state '<state>' was assigned <n> bytes.`
  - one state's combined Assign past the 262,144-byte state cap: `State total variable size limit exceeded. Variable '<name>' in state '<state>' was assigned <n> bytes, which caused the size of variables assigned in the state to exceed its limit by <m> bytes.`
  - the resident total past the 10 MiB execution cap: `Execution total variable size limit exceeded. State '<state>' assigned variables that exceed the total execution limit by <n> bytes.`
  A value exactly AT the per-variable cap succeeds (the emulator passes that control).
Emulator: for a single variable 2 bytes over the per-variable cap it answers the RESULT-size sentence of another failure class — `The state/task 'PassState' returned a result with a size exceeding the maximum number of bytes service limit.` — naming neither the variable nor the byte count, and describing an Assign as a returned result. The state-total and execution-total arms were not reached and share the same serializer.
Why:      a client cannot tell which of the three quotas it hit, nor which variable or state caused it; an operator reading the cause is told the state returned an oversized result, which is a different defect from an over-cap variable assignment, and any assertion or alarm on AWS's sentence fails.
Fix:      the Assign variable-size check's cause serializer in the interpreter — three separate templates, one per scope, carrying the variable name, the state name, the assigned byte count and the overage.
Traces:   stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution.

Cause 4 of 5: the execution-wide variable space is capped at 262,144 bytes instead of 10 MiB, and its cause is emulator-authored prose. Failing check id: stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space.
AWS:      recorded behavior only (documented under step-functions/latest/dg/workflow-variables.html). The execution-wide ceiling is 10 MiB (10,485,760 bytes) of RESIDENT variable data: 40 variables each exactly at the 262,144-byte per-variable cap fill it exactly, and the 41st state's assignment fails with `Execution total variable size limit exceeded. State '<state>' assigned variables that exceed the total execution limit by <n> bytes.` — fixed text with the failing state's name from the definition and the overage (here the whole 262,144 bytes of the assignment) computed against the 10 MiB cap. The cap tracks resident data, not a running sum, so reassigning ONE name 60 times, and assigning a cap-sized variable inside each of 60 Map iterations, both SUCCEED.
Emulator: status FAILED and error States.DataLimitExceeded match, but the cause is `the variables this state assigns leave more than the 262144 bytes of variables an execution may hold in scope` — the emulator's own wording, naming neither the state nor the overage, and quoting 262,144 bytes as the EXECUTION ceiling where 262,144 is AWS's PER-VARIABLE/PER-STATE cap and the execution cap is 10 MiB. The two freeing controls (reassignment, Map-scope exit) were not reached, so whether the accounting is resident or cumulative is unverified and belongs to the same cause.
Why:      a workflow that AWS runs happily — up to 10 MiB of live variables, and any amount of gross data under reassignment or Map-local scoping — fails in the emulator as soon as its variables pass 256 KiB, and the cause tells the client neither the state nor the quota it actually hit.
Fix:      the interpreter's variable-space accounting and its cause serializer — an execution-scope ceiling of 10 MiB over RESIDENT variables (a replaced value and an exited Map iteration's locals are freed), reported with AWS's execution-total sentence.
Traces:   stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space.

Cause 5 of 5: an execution role whose trust Condition names another account's aws:SourceArn is not refused, and the scenario never reaches a verdict. Failing check id: stepfunctions.execution-role-trust-source-arn-naming-another-account-blocks-task.
AWS:      recorded behavior only. A role trusting states.amazonaws.com under `StringEquals aws:SourceAccount <this account>` and `ArnLike aws:SourceArn arn:aws:states:<region>:<other account>:stateMachine:*` can never be assumed for this account's machines: the STANDARD execution ends FAILED with error `States.TaskFailed` and cause `The principal states.amazonaws.com is not authorized to assume the provided role. (role: <execution-role ARN>)`, the EXPRESS StartSyncExecution answers the identical status/error/cause, and a same-account twin SUCCEEDS. Fixed text: that whole sentence and the ` (role: ...)` group; the only variable part is the role ARN from the state machine's roleArn.
Emulator: the check produced no verdict at all — it was killed at the suite's 60-second budget, where the recording resolved all three executions (the two failures each exhausting a Retry block of 4 attempts at 2s/4s/8s/16s, plus the positive control) inside it. Hypothesis: the trust policy's aws:SourceArn/aws:SourceAccount Conditions are not evaluated when states.amazonaws.com assumes the execution role, so instead of failing at the assume each attempt proceeds to the Lambda invoke and the retried executions consume the whole window; the OPEN issue 20261121T042805Z-kms-stepfunctions-startexecution-ignores-the-execution-role-s.md reports exactly that non-evaluation for StartExecution on a CMK-encrypted machine, keyed to a kms__stepfunctions check, so it does not carry this one.
Why:      the trust Conditions AWS recommends for confused-deputy protection have no effect in the emulator, so a state machine whose role is scoped to another account's machines runs anyway; a client validating that boundary gets no refusal, and this scenario yields no answer at all rather than a wrong one.
Fix:      the execution-role assume seam — evaluate the trust policy's Conditions (aws:SourceArn, aws:SourceAccount) against the state machine ARN and account when states.amazonaws.com assumes the role, and fail the task immediately with States.TaskFailed carrying AWS's principal-not-authorized cause, identically for STANDARD history/DescribeExecution and EXPRESS StartSyncExecution.
Traces:   stepfunctions.execution-role-trust-source-arn-naming-another-account-blocks-task.

## Evidence
All five checks are listed under the suite report's ### stepfunctions heading. ARNs, role/machine names, session names, request ids and timestamps below are per-run values, never part of the requirement.

Cause 1 — stepfunctions.task-credentials-role-arn-from-variable-resolved-before-assume [mismatch]
Definition: {"StartAt":"T","States":{"T":{"Type":"Task","Resource":"arn:aws:states:::aws-sdk:sts:getCallerIdentity","Parameters":{},"Credentials":{"RoleArn.$":"States.Format('{}', $.roleArn)"},"End":true}}} (STANDARD). Positive input names a role trusting the execution role (SUCCEEDED, and the identity Arn is the assumed target role — matching AWS); negative input names a role trusting ec2.amazonaws.com instead.
  Expected cause: "The role <execution-role ARN> is not authorized to assume the task state's role, <target-role ARN>."
  Actual cause:   "User: arn:aws:sts::<account>:assumed-role/<exec-role>/<session> is not authorized to perform: sts:AssumeRole on resource: <target-role ARN> because no identity-based policy allows the sts:AssumeRole action (Service: AWSSecurityTokenService; Status Code: 403; Error Code: AccessDeniedException; Request ID: <id>; Proxy: null)"
  Failure text: check_stepfunctions_task_credentials_role_arn_from_variable_resolved_before_assume.py line 60, `assert bad.get("cause") == (...)` — AssertionError: unexpected Credentials rejection cause. (status FAILED and error States.TaskFailed matched.)

Cause 2 — stepfunctions.map-maxconcurrency-zero-over-many-items-plateaus-at-an-internal-cap [mismatch]
Definition: {"StartAt":"M","States":{"M":{"Type":"Map","ItemsPath":"$.items","MaxConcurrency":0,"ItemProcessor":{"ProcessorConfig":{"Mode":"INLINE"},"StartAt":"Hold","States":{"Hold":{"Type":"Wait","Seconds":20,"End":true}}},"End":true}}} (STANDARD), run with 10 items and then 150.
  Expected: the plateaued in-flight iteration count is 10 for 10 items; for 150 items it plateaus at the service's internal cap.
  Actual:   the plateaued in-flight count was 1.
  Failure text: check_stepfunctions_map_maxconcurrency_zero_over_many_items_plateaus_at_an_internal_cap.py line 144, `assert few_count == _FEW_ITEMS` — AssertionError: with only 10 items (well under the internal cap), MaxConcurrency 0 must run every one of them at once; the plateaued in-flight count was 1.

Cause 3 — stepfunctions.sfn-variable-size-limit-exceeded-per-variable-state-and-execution [mismatch]
All definitions are EXPRESS, QueryLanguage JSONata, values produced by $pad('', <width>, 'x') so no literal payload is sent.
  (a) {"PassState":{"Type":"Pass","Assign":{"one":"{% $pad('', 262142, 'x') %}"},"End":true}} — SUCCEEDS in both (exactly at the 262,144-byte cap).
  (b) same with width 262144 (serialized 262,146 bytes): status FAILED and error States.DataLimitExceeded match.
      Expected cause: "Variable size limit exceeded. Variable 'one' in state 'PassState' was assigned 262146 bytes."
      Actual cause:   "The state/task 'PassState' returned a result with a size exceeding the maximum number of bytes service limit."
      Failure text: check_stepfunctions_sfn_variable_size_limit_exceeded_per_variable_state_and_execution.py line 109, `assert over_resp["cause"] == (...)` — AssertionError: unexpected cause.
  (c) not reached: Assign {"a": pad(150000), "b": pad(150000)} expects "State total variable size limit exceeded. Variable 'b' in state 'PassState' was assigned 150002 bytes, which caused the size of variables assigned in the state to exceed its limit by 37860 bytes."
  (d) not reached: 40 states each assigning a 262,144-byte variable then a 41st assigning an empty one expects "Execution total variable size limit exceeded. State 'S41' assigned variables that exceed the total execution limit by 2 bytes."
  The byte counts, variable names and state names are the templates' variable parts, computed from the definition and the evaluated values.

Cause 4 — stepfunctions.sfn-reassigning-or-leaving-map-scope-frees-variable-space [mismatch]
60 sequential Pass states, each assigning its own cap-sized variable ($pad('', 262142, 'x'), 262,144 serialized bytes): status FAILED and error States.DataLimitExceeded match.
  Expected cause: "Execution total variable size limit exceeded. State 'S41' assigned variables that exceed the total execution limit by 262144 bytes." (10,485,760 // 262,144 = 40 states fill the 10 MiB cap exactly, so the 41st is the first over, by its whole size.)
  Actual cause:   "the variables this state assigns leave more than the 262144 bytes of variables an execution may hold in scope"
  Failure text: check_stepfunctions_sfn_reassigning_or_leaving_map_scope_frees_variable_space.py line 147, `assert distinct_resp["cause"] == (...)` — AssertionError: unexpected cause.
  Not reached (same cause): 60 Pass states all reassigning ONE name must SUCCEED, and a Map of 60 iterations each assigning an iteration-local cap-sized variable must SUCCEED.

Cause 5 — stepfunctions.execution-role-trust-source-arn-naming-another-account-blocks-task [mismatch]
Execution role trust: {"Effect":"Allow","Principal":{"Service":"states.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"aws:SourceAccount":"<this account>"},"ArnLike":{"aws:SourceArn":"arn:aws:states:<region>:111122223333:stateMachine:*"}}}; definition is a lambda:invoke Task with Retry on States.TaskFailed (IntervalSeconds 2, MaxAttempts 4, BackoffRate 2.0). Three executions: STANDARD under the foreign trust, EXPRESS (StartSyncExecution) under the foreign trust, STANDARD under a same-account twin trust.
  Expected: the two foreign-trust executions report status FAILED, error States.TaskFailed and cause "The principal states.amazonaws.com is not authorized to assume the provided role. (role: <execution-role ARN>)"; the same-account twin SUCCEEDS.
  Actual:   "Check timed out after 60.0s" — no assertion produced a verdict within the suite's budget, which the recording fits comfortably.
  Failure text: Check timed out after 60.0s.

## Steps to Reproduce
Cause 1 (Credentials assume-denial cause):
1. CreateRole an execution role trusting states.amazonaws.com; CreateRole a target role whose trust principal is {"Service":"ec2.amazonaws.com"}; CreateStateMachine (STANDARD) with a Task Resource arn:aws:states:::aws-sdk:sts:getCallerIdentity and "Credentials":{"RoleArn.$":"States.Format('{}', $.roleArn)"}.
2. StartExecution with input {"roleArn":"<target role ARN>"} and poll DescribeExecution to terminal.
   Expected: FAILED / States.TaskFailed / cause "The role <execution-role ARN> is not authorized to assume the task state's role, <target role ARN>."
   Actual: FAILED / States.TaskFailed / STS's own "User: ... is not authorized to perform: sts:AssumeRole on resource: ... (Service: AWSSecurityTokenService; Status Code: 403; Error Code: AccessDeniedException; Request ID: ...; Proxy: null)".

Cause 2 (Map MaxConcurrency 0 concurrency):
1. CreateStateMachine (STANDARD) with {"StartAt":"M","States":{"M":{"Type":"Map","ItemsPath":"$.items","MaxConcurrency":0,"ItemProcessor":{"ProcessorConfig":{"Mode":"INLINE"},"StartAt":"Hold","States":{"Hold":{"Type":"Wait","Seconds":20,"End":true}}},"End":true}}}.
2. StartExecution with 10 items; poll GetExecutionHistory and count iterations entered but not exited.
   Expected: the in-flight count plateaus at 10. Actual: it plateaus at 1.
3. Repeat with 150 items: AWS plateaus at its internal cap, not at 1.

Cause 3 (variable-size cause sentences):
1. CreateStateMachine (EXPRESS, QueryLanguage JSONata) with {"PassState":{"Type":"Pass","Assign":{"one":"{% $pad('', 262144, 'x') %}"},"End":true}}; StartSyncExecution.
   Expected: FAILED / States.DataLimitExceeded / "Variable size limit exceeded. Variable 'one' in state 'PassState' was assigned 262146 bytes."
   Actual: the result-size sentence "The state/task 'PassState' returned a result with a size exceeding the maximum number of bytes service limit."
2. Width 262142 must SUCCEED (it does).
3. Assign {"a": pad(150000,'x'), "b": pad(150000,'y')} in one state: expect "State total variable size limit exceeded. Variable 'b' in state 'PassState' was assigned 150002 bytes, which caused the size of variables assigned in the state to exceed its limit by 37860 bytes."
4. 40 states each assigning a 262,144-byte variable plus a 41st assigning an empty string: expect "Execution total variable size limit exceeded. State 'S41' assigned variables that exceed the total execution limit by 2 bytes."

Cause 4 (10 MiB resident execution cap):
1. CreateStateMachine (EXPRESS, JSONata) with 60 sequential Pass states S1..S60, each "Assign": {"v<i>": "{% $pad('', 262142, 'x') %}"}; StartSyncExecution.
   Expected: FAILED / States.DataLimitExceeded / "Execution total variable size limit exceeded. State 'S41' assigned variables that exceed the total execution limit by 262144 bytes."
   Actual: the same status/error with cause "the variables this state assigns leave more than the 262144 bytes of variables an execution may hold in scope".
2. The same 60 states all assigning the SAME name must SUCCEED (the replaced value is freed).
3. A Map with 60 iterations, each assigning one cap-sized iteration-local variable, must SUCCEED (the local is freed on exit).

Cause 5 (foreign aws:SourceArn execution-role trust):
1. CreateRole with trust {"Principal":{"Service":"states.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"aws:SourceAccount":"<this account>"},"ArnLike":{"aws:SourceArn":"arn:aws:states:<region>:111122223333:stateMachine:*"}}} and an inline lambda:InvokeFunction grant; CreateFunction a trivial function.
2. CreateStateMachine (STANDARD) with a lambda:invoke Task carrying Retry on States.TaskFailed; StartExecution and poll DescribeExecution.
   Expected: FAILED / States.TaskFailed / "The principal states.amazonaws.com is not authorized to assume the provided role. (role: <role ARN>)".
3. Repeat as EXPRESS via StartSyncExecution: identical status/error/cause in the response.
4. Control: the same machine under a role whose aws:SourceArn names THIS account must SUCCEED.
   Actual: the scenario produces no verdict inside 60 s.
