Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__stepfunctions__xray.xray-segment-tree-marks-map-and-parallel-cancellation-and-a-stopped-executions-fault
Issue-Key: lambda__stepfunctions__xray.xray-segment-tree-marks-map-and-parallel-cancellation-and-a-stopped-executions-fault|run#42|AssertionError|assert long_running.get("error") is True and long_running["cause"]["message"] == "Canceled", (
Issue-Commit: 1945f04af551ab9122ba85c8ecd7b899fcb2daf4

# lambda__stepfunctions__xray: StopExecution does not mark the running task's X-Ray subsegment error=True with cause 'Canceled'

> _Note: this issue was identified against the code base at commit `1945f04af551ab9122ba85c8ecd7b899fcb2daf4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopExecution

## Description
Cause 1 of 1: a task cancelled by StopExecution leaves no cancellation marker on its X-Ray subsegment.
AWS: recorded behavior only (Step Functions X-Ray tracing documented at concepts-xray-tracing.html; the segment documents are read through X-Ray BatchGetTraces/GetTraceSummaries). When StopExecution lands on an execution that has a task in flight, Step Functions closes that task's own subsegment marked error=True (the 'E' annotation) with a cause whose exception message is the fixed string "Canceled", and faults the root segment. The sibling-cancellation renderings of the same rule do work here: a Parallel branch's running task and the branch subsegment itself, and a Map iteration cancelled because another iteration failed, are all marked error=True with cause message "Canceled" — only the StopExecution rendering is missing. "Canceled" is fixed text; the segment/subsegment names come from the state names in the definition.
Emulator: the running task's subsegment at the moment StopExecution lands carries error=None and cause=None — neither the error flag nor the cause object is written, so the subsegment is indistinguishable from one that completed normally.
Why: a client reading the trace tree to find out why an execution stopped (the documented use of X-Ray tracing for Step Functions) sees an unmarked, apparently healthy subsegment for the very task that was cancelled, and cannot attribute the stop to it.
Fix: the X-Ray segment emitter for execution cancellation — the StopExecution path must close the in-flight task's subsegment with error=true and a cause whose exception message is "Canceled", the same way the sibling-cancellation path already does, and fault the root segment.
Traces: lambda__stepfunctions__xray.xray-segment-tree-marks-map-and-parallel-cancellation-and-a-stopped-executions-fault

## Evidence
AssertionError: the running task subsegment at the moment StopExecution lands must carry error=True (E) and cause 'Canceled'; got error=None cause=None. The earlier assertions in the same trace tree passed, i.e. the Parallel sibling branch's task, the Branch subsegment, and the cancelled Map iteration were all correctly marked error=True / cause.message == "Canceled"; only the StopExecution rendering is unmarked.

## Steps to Reproduce
1. Create a Lambda function whose handler sleeps ~20 s, and a state machine (tracingConfiguration enabled, role granting lambda:InvokeFunction plus xray:PutTraceSegments/PutTelemetryRecords/GetSamplingRules/GetSamplingTargets) with a Task invoking it.
2. StartExecution, wait until the task is in flight, then StopExecution.
3. Read the trace tree for the execution's trace root (X-Ray BatchGetTraces) and inspect the subsegment named after that Task state: it must carry error=true and cause.exceptions[].message == "Canceled", and the root segment must be faulted.
