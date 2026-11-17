Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: fis__iam__sts.start-experiment-cancelled-while-safety-lever-engaged
Issue-Key: fis|StartExperiment|ConflictException
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# StartExperiment is refused with ConflictException while the safety lever is engaged instead of being accepted and cancelled

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExperiment

## Description
Cause 1 of 1: the engaged safety lever is enforced at StartExperiment time instead of cancelling the started experiment.

AWS: recorded behavior only for the lever's effect (the Smithy model declares ConflictException on StartExperiment but does not say the lever uses it). The recording the check encodes is explicit: while the account's safety lever is `engaged`, StartExperiment still returns 200 with an experiment, and that experiment then settles to state `cancelled` with reason `Experiment <experiment-id> has been cancelled by the safety lever 'arn:aws:fis:<region>:<account>:safety-lever/default'.` and every action in state `cancelled`, never having run. (Fixed text as quoted; the variable parts are the experiment id, region and account id.) Once the lever is disengaged the same StartExperiment runs to `completed`.

Emulator: StartExperiment is refused outright: `botocore.errorfactory.ConflictException: An error occurred (ConflictException) when calling the StartExperiment operation: The safety lever is engaged in this account. No experiment can be started.` No experiment is created, so nothing can be observed as cancelled.

Why: a client gets a hard 409 where AWS returns an experiment id and an auditable `cancelled` experiment; tooling that records every attempted experiment (and operators who inspect why an experiment did not run) loses the record entirely, and code paths that only handle the documented cancelled-experiment outcome break.

Fix: the StartExperiment handler and the experiment state machine — accept the start while the lever is engaged, create the experiment, and have the experiment settle to `cancelled` with the safety-lever reason and all actions cancelled, instead of refusing with ConflictException.

Traces: fis__iam__sts.start-experiment-cancelled-while-safety-lever-engaged.

## Evidence
Expected: StartExperiment returns 200 while the lever is engaged; the experiment settles state.status `cancelled` with state.reason `Experiment <id> has been cancelled by the safety lever 'arn:aws:fis:<region>:<account>:safety-lever/default'.` and waitAction cancelled.
Actual: `botocore.errorfactory.ConflictException: An error occurred (ConflictException) when calling the StartExperiment operation: The safety lever is engaged in this account. No experiment can be started.`

## Steps to Reproduce
1. UpdateSafetyLeverState(id='default', state={'status':'engaged','reason': ...}) and confirm GetSafetyLever reports engaged with that reason.
2. CreateExperimentTemplate with a targetless aws:fis:wait action and a role FIS can assume; call StartExperiment.
Observed: ConflictException "The safety lever is engaged in this account. No experiment can be started." Expected: 200 with an experiment that then settles `cancelled` naming the experiment and the lever ARN.
3. Disengage the lever and StartExperiment again — it must run to `completed`.
