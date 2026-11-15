Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudformation.deploy-appsync-source-api-association-invalid-merge-type-rejected
Issue-Key: appsync__cloudformation.deploy-appsync-source-api-association-invalid-merge-type-rejected|run#18|AssertionError|assert rows == want, f"validation rows {rows!r}, expected {want!r}"
Issue-Commit: 0a5e4e1aa59af41a3d3c27e0082d824b0afa7ca4

# appsync__cloudformation: a property-validation failure (invalid AWS::AppSync::SourceApiAssociation MergeType enum) produces no VALIDATION_ERROR OperationEvent, so DescribeEvents reports no validation row

> _Note: this issue was identified against the code base at commit `0a5e4e1aa59af41a3d3c27e0082d824b0afa7ca4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
One net-new root cause found replaying the appsync__cloudformation deploy checks. The failing check is not marked ALREADY FILED or OWNED, and no open issue carries its replay key.

Cause 1 of 1: CloudFormation records no VALIDATION_ERROR operation event when a resource property fails enum validation, so DescribeEvents returns no validation rows for the update it rolled back.
AWS: Smithy — cloudformation DescribeEvents (paginated over OperationEvents) returns OperationEvent rows, and OperationEvent models EventType plus the validation members ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason and ValidationPath ("The path within the resource where the validation was applied"). appsync Smithy — SourceApiAssociationConfig$mergeType targets the MergeType enum whose only values are MANUAL_MERGE and AUTO_MERGE. Recorded behavior: an update_stack whose AWS::AppSync::SourceApiAssociation carries a MergeType outside that enum ends UPDATE_ROLLBACK_COMPLETE and DescribeEvents reports exactly one row with EventType VALIDATION_ERROR, LogicalResourceId = the offending resource's logical id, ValidationName "PROPERTY_VALIDATION", ValidationStatus "FAILED", ValidationFailureMode "FAIL", ValidationStatusReason "<rejected value> is not a valid enum value. Supported values: [AUTO_MERGE, MANUAL_MERGE]" (fixed text " is not a valid enum value. Supported values: [AUTO_MERGE, MANUAL_MERGE]", with the rejected value taken from the template) and ValidationPath the JSON pointer of the property in the template ("/Resources/<LogicalId>/Properties/SourceApiAssociationConfig/MergeType").
Emulator: the stack status is right — the update is refused and the stack reaches UPDATE_ROLLBACK_COMPLETE — but DescribeEvents returns no OperationEvent with EventType VALIDATION_ERROR at all: the collected validation rows are []. Nothing names the resource, the property path, or the reason.
Why: a client (and the console) diagnoses a refused deploy from the VALIDATION_ERROR operation events; with none emitted, a rolled-back update is indistinguishable from any other failure and automation cannot report which property it rejected or what the supported values are.
Fix: the cloudformation operation-event recording path behind DescribeEvents — the template/resource property-validation stage must record a VALIDATION_ERROR OperationEvent (ValidationName PROPERTY_VALIDATION, ValidationStatus FAILED, ValidationFailureMode FAIL, the enum reason text and the property's JSON pointer) for the resource whose property it refused, and DescribeEvents must serve it.
Related open issue (does not carry this check): 20261109T123922Z-appsync-cloudformation-3-net-new-causes-aws-appsync.md, about AWS::AppSync::{ApiCache,SourceApiAssociation,Type} as resource types — here the association resource itself deploys and merges correctly.
Traces: appsync__cloudformation.deploy-appsync-source-api-association-invalid-merge-type-rejected.

## Evidence
appsync__cloudformation.deploy-appsync-source-api-association-invalid-merge-type-rejected:
  positive control passes: the stack with MergeType MANUAL_MERGE reaches CREATE_COMPLETE and GetSourceApiAssociation reads ('MERGE_SUCCESS', {'mergeType': 'MANUAL_MERGE'}); the update with MergeType NOT_A_REAL_MERGE_TYPE correctly ends UPDATE_ROLLBACK_COMPLETE.
  expected (rows from DescribeEvents OperationEvents where EventType == 'VALIDATION_ERROR', as (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath)):
    [('Association', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'NOT_A_REAL_MERGE_TYPE is not a valid enum value. Supported values: [AUTO_MERGE, MANUAL_MERGE]', '/Resources/Association/Properties/SourceApiAssociationConfig/MergeType')]
  actual:
    AssertionError: validation rows [], expected [('Association', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'NOT_A_REAL_MERGE_TYPE is not a valid enum value. Supported values: [AUTO_MERGE, MANUAL_MERGE]', '/Resources/Association/Properties/SourceApiAssociationConfig/MergeType')]
  ('Association' is the template's logical id and NOT_A_REAL_MERGE_TYPE the value sent — both come from the request; the fixed parts are the EventType VALIDATION_ERROR row itself, ValidationName PROPERTY_VALIDATION, ValidationStatus FAILED, ValidationFailureMode FAIL, the reason's " is not a valid enum value. Supported values: [AUTO_MERGE, MANUAL_MERGE]" text and the property's JSON-pointer path.)

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (service appsync__cloudformation).
2. Create a stack with a source AWS::AppSync::GraphQLApi + schema, a MERGED AWS::AppSync::GraphQLApi with a MergedApiExecutionRoleArn, and an AWS::AppSync::SourceApiAssociation whose SourceApiAssociationConfig.MergeType is MANUAL_MERGE; wait for CREATE_COMPLETE.
3. update_stack with the same template but MergeType set to NOT_A_REAL_MERGE_TYPE; wait for the terminal status (UPDATE_ROLLBACK_COMPLETE).
4. Call cloudformation DescribeEvents for that stack and keep the OperationEvents whose EventType is VALIDATION_ERROR: the list is empty, instead of the single PROPERTY_VALIDATION/FAILED/FAIL row naming the enum reason and the /Resources/<LogicalId>/Properties/SourceApiAssociationConfig/MergeType path.
