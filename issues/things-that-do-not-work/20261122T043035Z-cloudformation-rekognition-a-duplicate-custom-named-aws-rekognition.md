Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__rekognition.deploy-rekognition-collection-duplicate-id-rejected
Issue-Key: cloudformation__rekognition.deploy-rekognition-collection-duplicate-id-rejected|run#19|AssertionError|assert rows == want, f"validation rows {rows!r}, expected {want!r}"
Issue-Commit: 622c214bd0a84106899ec4445dc3396f8892f16d

# cloudformation__rekognition: a duplicate custom-named AWS::Rekognition::Collection produces no NAME_CONFLICT_VALIDATION operation event, so the name-conflict pre-provision validation is invisible

> _Note: this issue was identified against the code base at commit `622c214bd0a84106899ec4445dc3396f8892f16d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
One net-new root cause: CloudFormation's pre-provision name-conflict validation emits no VALIDATION_ERROR operation event for a custom-named AWS::Rekognition::Collection whose name already exists, so DescribeEvents reports no validation row at all. Failing check: cloudformation__rekognition.deploy-rekognition-collection-duplicate-id-rejected

AWS:      Smithy (cloudformation): OperationEvent carries EventType (enum EventType, value VALIDATION_ERROR), LogicalResourceId, ValidationName (string), ValidationStatus (enum ValidationStatus: FAILED|SKIPPED), ValidationFailureMode (enum HookFailureMode: FAIL|WARN), ValidationStatusReason and ValidationPath; DescribeEvents paginates OperationEvents for a stack operation. A stack that declares a resource with an explicit name already taken is refused by the NAME_CONFLICT_VALIDATION check before provisioning, and that refusal is recorded as exactly one VALIDATION_ERROR operation event for the colliding logical id:
          ValidationName 'NAME_CONFLICT_VALIDATION', ValidationStatus 'FAILED', ValidationFailureMode 'FAIL', ValidationPath '/Resources/<LogicalResourceId>', ValidationStatusReason `Resource of type 'AWS::Rekognition::Collection' with identifier '<collection id>' already exists.`
          Fixed text of the reason: `Resource of type '`, `' with identifier '`, `' already exists.`; variable parts: the resource type and the explicit name taken from the template. ValidationPath's fixed part is `/Resources/`, followed by the template's logical id.
Emulator: DescribeEvents returns NO event with EventType 'VALIDATION_ERROR' for the operation — the validation-row list is empty. The name conflict is either not detected by the pre-provision validation at all, or detected without recording its operation event.
Why:      a client that reads the operation's validation events to learn WHY a stack was refused before provisioning (the console's validation panel, CI gates that fail on a NAME_CONFLICT_VALIDATION row, any triage keyed on ValidationName/ValidationPath) sees nothing and cannot tell a name collision from any other pre-provision refusal, nor which template path caused it.
Fix:      CloudFormation's pre-provision NAME_CONFLICT_VALIDATION pass and the OperationEvent it must record (ValidationName/ValidationStatus/ValidationFailureMode/ValidationStatusReason/ValidationPath) for custom-named resources, here for AWS::Rekognition::Collection.
Traces:   cloudformation__rekognition.deploy-rekognition-collection-duplicate-id-rejected (assertion at check line 180).

## Evidence
Check cloudformation__rekognition.deploy-rekognition-collection-duplicate-id-rejected [mismatch]

Validation rows read from DescribeEvents (OperationEvents with EventType == 'VALIDATION_ERROR'), as tuples (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath):

expected: [('Coll', 'NAME_CONFLICT_VALIDATION', 'FAILED', 'FAIL', "Resource of type 'AWS::Rekognition::Collection' with identifier '<collection id>' already exists.", '/Resources/Coll')]
actual:   []

AssertionError at check line 180: "validation rows [], expected [...]". The collection id in any run is a per-run value filling the identifier slot of the reason.

## Steps to Reproduce
1. Create a Rekognition collection named N (or deploy a stack that does), so the name is taken.
2. CreateStack with a template declaring an AWS::Rekognition::Collection under logical id 'Coll' with CollectionId = N.
3. Call DescribeEvents for that stack operation and keep the OperationEvents whose EventType is 'VALIDATION_ERROR'.
4. The emulator returns none; AWS returns one row for 'Coll' with ValidationName 'NAME_CONFLICT_VALIDATION', ValidationStatus 'FAILED', ValidationFailureMode 'FAIL', ValidationPath '/Resources/Coll' and ValidationStatusReason "Resource of type 'AWS::Rekognition::Collection' with identifier '<N>' already exists."
