Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift-serverless.workgroup-create-conflict-survives-settling-to-available
Issue-Shape-Id: com.amazonaws.redshiftserverless#RedshiftServerless
Issue-Key: redshift-serverless|CreateWorkgroup|ConflictException
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# redshift-serverless CreateWorkgroup duplicate-name refusal carries an emulator-authored ConflictException message instead of AWS's wording

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateWorkgroup

## Description
Cause 1 of 1: the ConflictException message CreateWorkgroup returns for a workgroup name that is already taken is the emulator's own sentence, not AWS's.
AWS: Smithy: CreateWorkgroup takes required `workgroupName` (WorkgroupName, @length 3-64, @pattern ^[a-z0-9-]+$) and required `namespaceName`, and models ConflictException (HTTP 409) among its errors. Recorded behavior: once the workgroup is AVAILABLE, re-issuing CreateWorkgroup with the SAME workgroupName is refused with code ConflictException and the message whose fixed text is 'The workgroup with name ' + <workgroupName from the request> + ' already exists.' — a capitalized sentence ending in a period, with the only variable part being the requested workgroup name.
Emulator: the call is refused with the right code, ConflictException, but the message is 'a workgroup named ' + <workgroupName> + ' already exists' — lowercase, different wording, no trailing period.
Why: clients and CLI wrappers that match on, log, or surface the refusal text (a very common pattern for distinguishing 'name taken' from the other ConflictException causes this operation returns, such as 'There is an operation running on the workgroup.') do not recognize the emulator's sentence, so an already-exists conflict is reported to the user as an unknown conflict or retried forever.
Fix: the CreateWorkgroup handler's duplicate-name validation/refusal path in the redshift-serverless service — the message template, not the code or status, is wrong.
Traces: redshift-serverless.workgroup-create-conflict-survives-settling-to-available. Note: because the check stops at this first assertion, the later refusals it exercises (a NEW workgroup name in a namespace that already has one, expected ConflictException 'There is already a workgroup <residentWorkgroupName> associated with the namespace.') were never reached and are not asserted here; re-replaying this check after the fix will grade them.

## Evidence
Expected (code, message): ('ConflictException', 'The workgroup with name <workgroupName> already exists.') — fixed text 'The workgroup with name ' / ' already exists.', variable part the workgroupName taken from the request.
Actual: AssertionError: {'Message': 'a workgroup named sim2real-behav-rswg-43fdf7cdc515 already exists', 'Code': 'ConflictException'} — the name in that line is this run's generated workgroup name and is not part of the requirement; the diverging fixed text is 'a workgroup named ' / ' already exists'.

## Steps to Reproduce
CreateNamespace(namespaceName=<ns>, dbName='dev'). CreateWorkgroup(workgroupName=<wg>, namespaceName=<ns>, baseCapacity=8, publiclyAccessible=false). Poll GetWorkgroup(workgroupName=<wg>) until status AVAILABLE. Then call CreateWorkgroup again with the same workgroupName=<wg> and namespaceName=<ns> (baseCapacity=32): the emulator answers ConflictException with the message 'a workgroup named <wg> already exists' instead of 'The workgroup with name <wg> already exists.'
