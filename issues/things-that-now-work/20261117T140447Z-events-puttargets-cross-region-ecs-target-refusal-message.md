Issue-Source: trace-sweep-divergence
Issue-Trace: events-ebecs-task-definition-reference-forms
Issue-Step: task#5
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: events-ebecs-task-definition-reference-forms
Issue-Shape-Id: com.amazonaws.eventbridge#AWSEvents
Issue-Key: events|PutTargets|4xx->4xx|$.message
Issue-Commit: eab59dd3d13731329d6515ebc512ab3051ad49e1

# events: PutTargets cross-region ECS target refusal message drops AWS's trailing period

> _Note: this issue was identified against the code base at commit `eab59dd3d13731329d6515ebc512ab3051ad49e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTargets

## Description
Cause 1 of 1 — the region-mismatch refusal message for an ECS target is emitted without its final period.
AWS: PutTargets validates each target's ECS parameters and answers HTTP 400 when the target's ECS cluster ARN and the ECS task-definition ARN it references name different regions. The message is the fixed text `Region of ` + the cluster ARN exactly as the request wrote it + ` does not match region of ` + the task-definition ARN exactly as the request wrote it + `.` — the two ARNs are the only variable parts, taken from the request, and the sentence ends with a period. Smithy: PutTargets.Targets is @required, TargetList @length 1..100, and the operation models ValidationException-class client refusals plus ResourceNotFoundException/LimitExceededException/ManagedRuleException/ConcurrentModificationException (all errorKind client, httpError 400); the region-consistency rule itself is recorded behavior only, the model does not state it.
Emulator: the correct status (400) and error code, and the correct ARNs, but the message terminates right after the task-definition ARN with no period.
Why: the message text is part of the contract. A client (or test harness) that compares the refusal sentence verbatim, or splits it on the sentence terminator to extract the two ARNs, does not recognise the emulator's message as this error class and mis-handles a correctly refused cross-region target.
Fix: the PutTargets ECS-target validation's refusal-message formatter — append the sentence-terminating period after the task-definition ARN.
Traces: events-ebecs-task-definition-reference-forms#task#5.

## Evidence
Replay of simparity-wire-aws-v1, fixture events-ebecs-task-definition-reference-forms step task#5 (PutTargets): status 400 on both sides, $.message value_mismatch.
Expected: 'Region of arn:aws:ecs:us-west-2:<account>:cluster/<cluster-name> does not match region of arn:aws:ecs:us-east-1:<account>:task-definition/<td-name>:1.'
Actual:   the same sentence without the trailing '.' .
Fixed text: 'Region of ' ... ' does not match region of ' ... '.'; variable parts: the cluster ARN and the task-definition ARN, both echoed from the request (the account id, region, cluster name, task-definition name and revision in the recording are that run's values, not part of the template).

## Steps to Reproduce
1. Replay the wire corpus simparity-wire-aws-v1 and re-run fixture events-ebecs-task-definition-reference-forms, step task#5.
2. Create an ECS cluster in one region and a task definition in another, create an EventBridge rule, then call PutTargets with an ECS target whose Arn is the cluster in region A and whose EcsParameters.TaskDefinitionArn is the task definition in region B.
3. Compare Error.Message byte-for-byte with AWS's: it must end with a period after the task-definition ARN.
