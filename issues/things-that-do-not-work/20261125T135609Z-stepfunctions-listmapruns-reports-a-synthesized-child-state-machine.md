Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.conc-distributed-map-child-executions-listed-and-describable-per-item
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.conc-distributed-map-child-executions-listed-and-describable-per-item|run#20|AssertionError|assert child_sms == {map_run["stateMachineArn"]}, (
Issue-Commit: 73c230b6452965c726d43dc4b03e705450f73977

# stepfunctions: ListMapRuns reports a synthesized "child" state machine ARN (parent ARN + "/<mapRunLabel>") as mapRuns[].stateMachineArn instead of the state machine that started the Map Run

> _Note: this issue was identified against the code base at commit `73c230b6452965c726d43dc4b03e705450f73977`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListMapRuns

## Description
One net-new root cause.

Cause 1 of 1: `ListMapRuns` returns a `mapRuns[].stateMachineArn` that is not a state machine ARN at all — it is the parent state machine ARN with the Map Run's label appended after a `/`. Failing check: stepfunctions.conc-distributed-map-child-executions-listed-and-describable-per-item

AWS: Smithy (com.amazonaws.sfn) `MapRunListItem.stateMachineArn` — required, target `Arn` with @length min 1 max 256, documented as "The Amazon Resource Name (ARN) of the executed state machine." A Step Functions state machine ARN has the form `arn:aws:states:<region>:<account>:stateMachine:<name>` (optionally `:<version|alias>`); it never contains a `/`. The `/<label>` segment belongs only to the Map Run ARN (`arn:aws:states:<region>:<account>:mapRun:<stateMachineName>/<mapRunId>`) and to the child execution ARNs it dispatches. Recorded behavior: the child workflow executions of a DISTRIBUTED Map read `$$.StateMachine.Id` as the plain parent state machine ARN (`arn:aws:states:<region>:<account>:stateMachine:<name>`), and `DescribeExecution` on a child returns that same plain ARN in `stateMachineArn` plus the Map Run in `mapRunArn` — so the ARN `ListMapRuns` names for a Map Run must equal the ARN its children run under.

Emulator: for every Map Run item the emulator substitutes a fabricated "child state machine" ARN: the parent ARN with the Map Run label appended, e.g. it answers `arn:aws:states:<region>:<account>:stateMachine:<name>/<mapRunLabelUuid>` while the children of that same Map Run report, and `DescribeExecution` returns, `arn:aws:states:<region>:<account>:stateMachine:<name>`. The two can never be joined. (Shapes shown with their variable parts named; the concrete name/uuid in the replay are per-run values, not part of the requirement.)

Why: a client that correlates Map Runs with their state machine or their child executions — pairing `ListMapRuns` output with `DescribeStateMachine`, `ListExecutions(stateMachineArn=…)`, `DescribeExecution(child).stateMachineArn`, a tag/metric lookup, or just `$$.StateMachine.Id` written by the children — gets no match for any Map Run. Worse, the value is syntactically not a state machine ARN, so feeding it back into any Step Functions API that takes a state machine ARN is refused with `InvalidArn`.

Fix: the `ListMapRuns` handler's construction of `MapRunListItem.stateMachineArn` (services/service-com-amazonaws-sfn-awsstepfunctions/src/ops/list_map_runs.rs) and the Map-Run identity helper it calls in the service state (`crate::state::map_run_identity::child_state_machine_arn`): the listing must report the stored Map Run's own `state_machine_arn` — the state machine whose execution entered the Distributed Map state — with no label appended. The label stays where AWS puts it, in `mapRunArn` and in the child execution ARNs.

Traces: stepfunctions.conc-distributed-map-child-executions-listed-and-describable-per-item — a DISTRIBUTED Map over 8 items; the children record `$$.Execution.Id` and `$$.StateMachine.Id` to DynamoDB, and the check compares the recorded state machine ids against `ListMapRuns`.

## Evidence
Check stepfunctions.conc-distributed-map-child-executions-listed-and-describable-per-item (mismatch):

  check_stepfunctions_conc_distributed_map_child_executions_listed_and_describable_per_item.py, line 95:
    assert child_sms == {map_run["stateMachineArn"]}
  AssertionError: the children ran under state machines
    ['arn:aws:states:us-west-2:000000000000:stateMachine:sim2real-behav-dmap-5c63cb9aff6f'];
    ListMapRuns names
    'arn:aws:states:us-west-2:000000000000:stateMachine:sim2real-behav-dmap-5c63cb9aff6f/eab7d779-0aac-33a6-b19a-cb3f099fcb8c'

Expected (AWS): mapRuns[].stateMachineArn == the plain state machine ARN that started the Map Run, i.e. the same value the children report as $$.StateMachine.Id and the same value DescribeExecution(child).stateMachineArn returns — `arn:aws:states:<region>:<account>:stateMachine:<name>`.
Actual (emulator): `arn:aws:states:<region>:<account>:stateMachine:<name>/<mapRunLabel>` — the parent ARN with the Map Run label appended, a form that is not a valid state machine ARN.

Source of the fabrication (services/service-com-amazonaws-sfn-awsstepfunctions/src/ops/list_map_runs.rs, lines 218-229):
    state_machine_arn: crate::state::map_run_identity::child_state_machine_arn(
        &map_run.state_machine_arn,
        &map_run.label,
    ),
with the comment asserting "the state machine a Distributed Map state's Map Run executed is the Map state's own CHILD state machine, whose ARN is the parent's with this Map Run's label after a `/`" — AWS defines no such child state machine resource.

The name `sim2real-behav-dmap-5c63cb9aff6f`, the account `000000000000` and the label uuid are per-run values; the requirement is the ARN shape and the equality with the children's state machine id.

## Steps to Reproduce
1. Create an IAM role Step Functions can assume (with DynamoDB write permission) and a DynamoDB table.
2. CreateStateMachine (STANDARD) with a single `Map` state whose ItemProcessor has ProcessorConfig {"Mode": "DISTRIBUTED", "ExecutionType": "STANDARD"}; inside the ItemProcessor, a Task on `arn:aws:states:::dynamodb:updateItem` that writes `$$.Execution.Id` and `$$.StateMachine.Id` for each item.
3. StartExecution with 8 items in `$.items` and wait for the execution to reach SUCCEEDED.
4. ListMapRuns(executionArn=<parent execution ARN>) and read mapRuns[0].stateMachineArn.
5. Scan the DynamoDB table and read the `$$.StateMachine.Id` value each child wrote; also call DescribeExecution on each child execution ARN from ListExecutions(mapRunArn=<mapRunArn>) and read its stateMachineArn.
6. Observe mapRuns[0].stateMachineArn carries a trailing `/<mapRunLabel>` and therefore does not equal the plain state machine ARN the children and DescribeExecution report; on AWS all three are the identical plain state machine ARN.
