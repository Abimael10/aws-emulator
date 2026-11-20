Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__stepfunctions.published-versions-freeze-definition-and-description
Issue-Key: iam__stepfunctions.published-versions-freeze-definition-and-description|run#27|AssertionError|assert base_described.get("description") is None, (
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# DescribeStateMachine on the unqualified ARN reports the published version's description

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStateMachine

## Description
Cause 1 of 1: a description supplied when publishing a version leaks onto the unqualified state machine.

AWS: Smithy (sfn PublishStateMachineVersion takes `description`; DescribeStateMachine returns `description` for a version ARN) — a published version is an immutable snapshot: the description given at publish time belongs to that version ARN only, and DescribeStateMachine on the unqualified state machine ARN carries no `description` member unless the state machine itself was created/updated with one. The check asserts exactly that: the unqualified ARN must carry no description.

Emulator: DescribeStateMachine on the unqualified ARN returns the version's description: `AssertionError: the unqualified ARN must carry no description, got 'v1 snapshot'` (the value is whatever was passed to PublishStateMachineVersion).

Why: a client reading the state machine's own metadata sees a value that belongs to a version, so the unqualified definition/description no longer describes the live state machine and version publication is not the immutable, side-effect-free snapshot AWS guarantees.

Fix: the state-machine state/DescribeStateMachine handler — store a publish description on the version record only, and omit `description` on the unqualified ARN unless it was set on the state machine itself.

Traces: iam__stepfunctions.published-versions-freeze-definition-and-description.

## Evidence
Expected: `DescribeStateMachine(stateMachineArn=<unqualified ARN>)` carries no `description` member after a version is published with one.
Actual: `AssertionError: the unqualified ARN must carry no description, got 'v1 snapshot'`

## Steps to Reproduce
1. CreateStateMachine (no description) and a role Step Functions can assume.
2. PublishStateMachineVersion(stateMachineArn=<arn>, description='v1 snapshot').
3. DescribeStateMachine(stateMachineArn=<unqualified arn>).
Observed: description 'v1 snapshot'. Expected: no description member; the version ARN alone carries it.
