Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.activation-past-expiration-date-rejected ssm.associate-ops-item-related-item-rejects-unknown-ops-item ssm.association-describe-list-and-delete ssm.start-associations-once-deleted-association-rejected ssm.automation-execute-aws-api-reaches-success ssm.create-association-batch-invalid-document-rejects-whole-call ssm.create-association-batch-malformed-document-name-rejected ssm.describe-association-execution-targets-nonexistent-execution-rejected ssm.describe-association-execution-targets-unknown-execution-rejected ssm.describe-ops-items-filter-operator-restricted-by-key-rejected ssm.get-default-patch-baseline-invalid-operating-system-rejected ssm.modify-document-permission-validation-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.activation-past-expiration-date-rejected|_error#9|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: ssm.associate-ops-item-related-item-rejects-unknown-ops-item|run#15|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: ssm.association-describe-list-and-delete|run#26|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: ssm.start-associations-once-deleted-association-rejected|run#14|AssertionError|assert got == expected, f"expected {expected!r}, got {got!r}"
Issue-Key: ssm.automation-execute-aws-api-reaches-success|run#48|AssertionError|assert missing_param == (
Issue-Key: ssm.create-association-batch-invalid-document-rejects-whole-call|_error#5|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: ssm.create-association-batch-malformed-document-name-rejected|run#5|AssertionError|assert got == expected, f"expected {expected!r}, got {got!r}"
Issue-Key: ssm|DescribeAssociationExecutionTargets|AssociationExecutionDoesNotExist
Issue-Key: ssm.describe-ops-items-filter-operator-restricted-by-key-rejected|_error#10|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: ssm.get-default-patch-baseline-invalid-operating-system-rejected|run#3|AssertionError|assert got == _UNKNOWN_ENUM, f"expected {_UNKNOWN_ENUM!r}, got {got!r}"
Issue-Key: ssm.modify-document-permission-validation-rejected|run#6|AssertionError|assert got == _NEITHER_ADD_NOR_REMOVE, f"expected {_NEITHER_ADD_NOR_REMOVE!r}, got {got!r}"
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# ssm: 10 net-new behavioral causes — CreateActivation accepts a past ExpirationDate, error responses carry no request-id header, and eight association/automation/OpsCenter/document refusals diverge

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateActivation

## Description
Ten independent net-new root causes in the ssm service, each grounded in its own failing behavioral check. A fixer must close all ten before submitting.

Cause 1 of 10: CreateActivation accepts an ExpirationDate already in the past.
AWS: Smithy com.amazonaws.ssm#CreateActivation, input member ExpirationDate (target ExpirationDate, timestamp, optional); modeled client error InvalidParameters (400). Recorded behavior: AWS refuses an ExpirationDate in the past with HTTP 400, Error.Code ValidationException and the message "expirationDate must be greater than now and lesser than 30 days." (entirely fixed text — no request value is interpolated), and mints no activation; the identical call with a future date returns ActivationId/ActivationCode.
Emulator: the call succeeds with HTTP 200 and returns an ActivationId/ActivationCode, so an activation that is already expired is created.
Why: a client that relies on the service to reject an unusable expiry registers hybrid nodes against an activation that can never be used, and never sees the validation error it must surface to its own user.
Fix: the CreateActivation handler's input validation in the ssm service crate — validate ExpirationDate against now and the 30-day ceiling before minting an activation.
Traces: ssm.activation-past-expiration-date-rejected.

Cause 2 of 10: ssm responses carry no request-id header, so ResponseMetadata.RequestId is absent while the error message embeds a request id.
AWS: recorded behavior only (the request-id header is a protocol concern, not a modeled member). AWS answers every ssm call with the x-amzn-RequestId response header, which botocore surfaces as ResponseMetadata.RequestId; AmazonSSM's OpsItemNotFoundException message embeds that same id: "OpsItem not found. (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemNotFoundException; Request ID: <the request id of this call>; Proxy: null)". The fixed text is everything but the id; the variable part is the request id, and it must be the SAME value the response header carries.
Emulator: AssociateOpsItemRelatedItem on an unknown OpsItemId answers Error.Code OpsItemNotFoundException with the message "OpsItem not found. (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemNotFoundException; Request ID: <uuid>; Proxy: null)" but no request-id header: botocore reports ResponseMetadata.RequestId = None (the replayed response headers are only content-type, content-length and date), so the id in the message corresponds to nothing the client can observe. This is not a per-run value difference — the defect is the missing header.
Why: a client correlating a failure with a support case, a log line or a retry trace has no request id at all, and any client that cross-checks the id in the message against ResponseMetadata.RequestId sees a mismatch.
Fix: the ssm service crate's response serialization/middleware — emit x-amzn-RequestId (and x-amz-request-id where ssm sends it) on every response, success and error, and interpolate that same id into messages that embed one.
Traces: ssm.associate-ops-item-related-item-rejects-unknown-ops-item.

Cause 3 of 10: the AssociationDoesNotExist message is emulator-authored and names no association id.
AWS: Smithy com.amazonaws.ssm#AssociationDoesNotExist is bound to DescribeAssociation, DeleteAssociation and StartAssociationsOnce (StartAssociationsOnce errors: AssociationDoesNotExist, InvalidAssociation). Recorded behavior: the message is "Association id <AssociationId> does not exist" — fixed text "Association id " + " does not exist" with the association id taken verbatim from the request.
Emulator: both operations answer AssociationDoesNotExist with the constant "The specified association doesn't exist.", naming no id.
Why: a caller batching association ids (StartAssociationsOnce takes up to 10) cannot tell WHICH id was rejected, and message-matching clients break.
Fix: the association-lookup error construction shared by the DescribeAssociation/DeleteAssociation/StartAssociationsOnce handlers in the ssm service crate.
Traces: ssm.association-describe-list-and-delete, ssm.start-associations-once-deleted-association-rejected.

Cause 4 of 10: StartAutomationExecution's missing-required-parameter message is emulator-authored.
AWS: Smithy com.amazonaws.ssm#StartAutomationExecution, modeled client error InvalidAutomationExecutionParametersException (400). Recorded behavior: omitting a required document parameter is refused with that code and the message "Missing required parameter: <ParameterName> in user inputs." — fixed text "Missing required parameter: " + " in user inputs.", the variable part being the declared document parameter name that was omitted.
Emulator: same code, but the message is "Missing required parameters: TargetName" (plural noun, no " in user inputs." suffix).
Why: clients and runbooks that parse or display this message cannot recognise it, and the plural form misstates a single-parameter failure.
Fix: the StartAutomationExecution handler's document-parameter validation in the ssm service crate.
Traces: ssm.automation-execute-aws-api-reaches-success.

Cause 5 of 10: CreateAssociationBatch reports a nonexistent document as a per-entry failure instead of refusing the whole call.
AWS: Smithy com.amazonaws.ssm#CreateAssociationBatch declares InvalidDocument (400, httpResponseCode 404) among its operation errors. Recorded behavior: a batch containing one entry whose Name is a document that does not exist is refused in whole — no entry is created and the response is an InvalidDocument error, not a 200 with a Failed list.
Emulator: the call returns HTTP 200 with the valid entry under Successful (a real AssociationId is minted) and the bad entry under Failed with Message "Document <name> doesn't exist" and Fault "Client".
Why: the batch is not atomic as AWS defines it — a caller that treats a 200 as "all or nothing" silently creates half the associations and must clean up state AWS never created.
Fix: the CreateAssociationBatch handler in the ssm service crate — validate every entry's document before creating any association and fail the request with InvalidDocument.
Traces: ssm.create-association-batch-invalid-document-rejects-whole-call.

Cause 6 of 10: CreateAssociationBatch's malformed-document-name refusal uses the generic shape-validation message instead of SSM's own.
AWS: recorded behavior: a batch entry whose Name is too short is refused with Error.Code ValidationException and the message "Invalid document name: <name>" — fixed text "Invalid document name: " plus the offending name from the request.
Emulator: same code, but the message is the generic constraint render "1 validation error detected: Value '<name>' at 'entries.1.member.name' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_\\-.:/]{3,128}".
Why: the emulator reports a shape-level constraint failure where AWS reports a service-level name validation, so a client keying on either the message or the member path takes the wrong branch.
Fix: the CreateAssociationBatch handler's per-entry name validation in the ssm service crate.
Traces: ssm.create-association-batch-malformed-document-name-rejected.

Cause 7 of 10: DescribeAssociationExecutionTargets refuses an unknown ExecutionId on an existing association instead of returning an empty list.
AWS: Smithy com.amazonaws.ssm#DescribeAssociationExecutionTargets, required AssociationId and ExecutionId, errors AssociationDoesNotExist, AssociationExecutionDoesNotExist, InvalidNextToken. Recorded behavior: with a REAL AssociationId and a well-formed but unknown ExecutionId, AWS answers HTTP 200 with AssociationExecutionTargets = []; AssociationExecutionDoesNotExist is not used for that case, and an unknown AssociationId is refused AssociationDoesNotExist with "Association <AssociationId> does not exist.".
Emulator: the same call is refused HTTP 400 AssociationExecutionDoesNotExist "The specified execution ID doesn't exist. Verify the ID number and try again.", so the check never reaches the association-level assertion.
Why: a client polling execution targets for an id it has not yet observed gets a hard error where AWS gives an empty page, turning a normal not-yet-there read into a failure.
Fix: the DescribeAssociationExecutionTargets handler in the ssm service crate — scope the not-found refusal to the AssociationId and return an empty target list for an unmatched ExecutionId on a known association.
Traces: ssm.describe-association-execution-targets-nonexistent-execution-rejected, ssm.describe-association-execution-targets-unknown-execution-rejected.

Cause 8 of 10: DescribeOpsItems accepts a filter operator the Priority key does not allow.
AWS: Smithy com.amazonaws.ssm#DescribeOpsItems models only InternalServerError, so this is recorded behavior: although GreaterThan is a valid OpsItemFilterOperator, the Priority key accepts only Equal, and a Priority filter with Operator GreaterThan is refused HTTP 400 with Error.Code OpsCenterInvalidArgumentException and the message "InvalidFilter" (fixed text, no interpolation). The identical filter with Equal succeeds.
Emulator: the GreaterThan filter is accepted and answers HTTP 200 with OpsItemSummaries.
Why: the per-key operator restriction is silently dropped, so a client gets results for a filter AWS rejects and never learns its query is invalid — results that will differ the moment it runs against AWS.
Fix: the DescribeOpsItems handler's OpsItemFilters validation in the ssm service crate — enforce the per-key allowed-operator table before filtering.
Traces: ssm.describe-ops-items-filter-operator-restricted-by-key-rejected.

Cause 9 of 10: GetDefaultPatchBaseline's unknown-enum ValidationException lists the OperatingSystem values in the wrong order.
AWS: Smithy com.amazonaws.ssm#OperatingSystem is an enum with 15 values. Recorded behavior: an unmodeled OperatingSystem is refused ValidationException with "1 validation error detected: Value '<value>' at 'operatingSystem' failed to satisfy constraint: Member must satisfy enum value set: [RASPBIAN, WINDOWS, AMAZON_LINUX_2022, AMAZON_LINUX_2023, UBUNTU, ROCKY_LINUX, REDHAT_ENTERPRISE_LINUX, CENTOS, AMAZON_LINUX_2, MACOS, ORACLE_LINUX, SUSE, ALMA_LINUX, DEBIAN, AMAZON_LINUX]" — the bracketed sequence is fixed text in exactly that order; only the rejected value is taken from the request.
Emulator: the same code and framing but the list is emitted as [... MACOS, ORACLE_LINUX, ALMA_LINUX, SUSE, DEBIAN, AMAZON_LINUX] — SUSE and ALMA_LINUX are transposed.
Why: the enum value set is part of the message contract; any client, test or snapshot comparing the refusal text sees a mismatch, and the emulator is demonstrably emitting its own ordering rather than AWS's.
Fix: the validation-message renderer for enum constraints used by the GetDefaultPatchBaseline handler in the ssm service crate — emit AWS's fixed ordering for OperatingSystem.
Traces: ssm.get-default-patch-baseline-invalid-operating-system-rejected.

Cause 10 of 10: ModifyDocumentPermission's neither-add-nor-remove refusal uses the wrong error class and message.
AWS: Smithy com.amazonaws.ssm#ModifyDocumentPermission, optional AccountIdsToAdd and AccountIdsToRemove. Recorded behavior: a call supplying neither is refused HTTP 400 with Error.Code InvalidParameterException and the message "Either AccountIdsToAdd or AccountIdsToRemove must be specified for API operation" (entirely fixed text).
Emulator: the call is refused with Error.Code ValidationException and the message "You must specify a value for the AccountIdsToAdd parameter or the AccountIdsToRemove parameter".
Why: both the exception class a client catches and the text it matches are wrong, so error handling written against AWS does not fire.
Fix: the ModifyDocumentPermission handler's argument validation in the ssm service crate.
Traces: ssm.modify-document-permission-validation-rejected.

## Evidence
Behavioral suite, service heading `### ssm`. Each cause's replay excerpt:

Cause 1 — ssm.activation-past-expiration-date-rejected:
expected: ValidationException / "expirationDate must be greater than now and lesser than 30 days."
actual: AssertionError: expected the call to be rejected, but it succeeded: {'ActivationId': '<uuid>', 'ActivationCode': '<code>', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}

Cause 2 — ssm.associate-ops-item-related-item-rejects-unknown-ops-item:
expected message 'OpsItem not found. (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemNotFoundException; Request ID: None; Proxy: null)' — the check interpolates ResponseMetadata.RequestId, which the emulator leaves None —
got 'OpsItem not found. (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemNotFoundException; Request ID: f5a1e09e-3512-402a-a6a7-816648048568; Proxy: null)'.
Replayed response headers across this service carry only content-type, content-length and date; no x-amzn-RequestId.

Cause 3 — ssm.association-describe-list-and-delete:
expected message 'Association id <uuid> does not exist', got "The specified association doesn't exist."
ssm.start-associations-once-deleted-association-rejected:
expected ('AssociationDoesNotExist', 'Association id <uuid> does not exist'), got ('AssociationDoesNotExist', "The specified association doesn't exist.")

Cause 4 — ssm.automation-execute-aws-api-reaches-success:
expected ('InvalidAutomationExecutionParametersException', 'Missing required parameter: TargetName in user inputs.'), got ('InvalidAutomationExecutionParametersException', 'Missing required parameters: TargetName')

Cause 5 — ssm.create-association-batch-invalid-document-rejects-whole-call:
AssertionError: expected the call to be rejected, but it succeeded: {'Successful': [{'Name': '<existing doc>', 'AssociationVersion': '1', 'AssociationId': '<uuid>', ...}], 'Failed': [{'Entry': {'Name': '<missing doc>', ...}, 'Message': "Document <missing doc> doesn't exist", 'Fault': 'Client'}], 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}

Cause 6 — ssm.create-association-batch-malformed-document-name-rejected:
expected ('ValidationException', 'Invalid document name: ab'), got ('ValidationException', "1 validation error detected: Value 'ab' at 'entries.1.member.name' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_\\-.:/]{3,128}")

Cause 7 — ssm.describe-association-execution-targets-nonexistent-execution-rejected and ssm.describe-association-execution-targets-unknown-execution-rejected, identical escape:
botocore.errorfactory.AssociationExecutionDoesNotExist: An error occurred (AssociationExecutionDoesNotExist) when calling the DescribeAssociationExecutionTargets operation: The specified execution ID doesn't exist. Verify the ID number and try again.
The checks assert `describe_association_execution_targets(AssociationId=<real>, ExecutionId=<fresh uuid4>)["AssociationExecutionTargets"] == []`.

Cause 8 — ssm.describe-ops-items-filter-operator-restricted-by-key-rejected:
expected err['Code'] == 'OpsCenterInvalidArgumentException' and Message == 'InvalidFilter' for OpsItemFilters=[{Source,Equal},{Key:'Priority',Values:['2'],Operator:'GreaterThan'}];
actual: the call SUCCEEDED and returned {'OpsItemSummaries': [ ... one summary ... ], 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}

Cause 9 — ssm.get-default-patch-baseline-invalid-operating-system-rejected:
expected "... enum value set: [RASPBIAN, WINDOWS, AMAZON_LINUX_2022, AMAZON_LINUX_2023, UBUNTU, ROCKY_LINUX, REDHAT_ENTERPRISE_LINUX, CENTOS, AMAZON_LINUX_2, MACOS, ORACLE_LINUX, SUSE, ALMA_LINUX, DEBIAN, AMAZON_LINUX]"
got  "... enum value set: [RASPBIAN, WINDOWS, AMAZON_LINUX_2022, AMAZON_LINUX_2023, UBUNTU, ROCKY_LINUX, REDHAT_ENTERPRISE_LINUX, CENTOS, AMAZON_LINUX_2, MACOS, ORACLE_LINUX, ALMA_LINUX, SUSE, DEBIAN, AMAZON_LINUX]"

Cause 10 — ssm.modify-document-permission-validation-rejected:
expected ('InvalidParameterException', 'Either AccountIdsToAdd or AccountIdsToRemove must be specified for API operation'), got ('ValidationException', 'You must specify a value for the AccountIdsToAdd parameter or the AccountIdsToRemove parameter')

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. Cause 1: ssm CreateActivation with IamRole=<an SSM-trusting role>, RegistrationLimit=1 and ExpirationDate = now minus 1 day. Observe HTTP 200 with ActivationId/ActivationCode; AWS answers 400 ValidationException "expirationDate must be greater than now and lesser than 30 days." and DescribeActivations lists nothing.
3. Cause 2: any ssm call, success or error — inspect the response headers. No x-amzn-RequestId is present (botocore ResponseMetadata.RequestId is None), yet AssociateOpsItemRelatedItem on an unknown OpsItemId embeds a request id in its message.
4. Cause 3: ssm DeleteAssociation / StartAssociationsOnce with a deleted or never-created AssociationId. Observe AssociationDoesNotExist with "The specified association doesn't exist." instead of "Association id <id> does not exist".
5. Cause 4: ssm StartAutomationExecution on a document with two required parameters, supplying only one. Observe "Missing required parameters: TargetName" instead of "Missing required parameter: TargetName in user inputs.".
6. Cause 5: ssm CreateAssociationBatch with two entries, one naming an existing document and one naming a nonexistent document. Observe HTTP 200 with a Successful entry; AWS refuses the whole call with InvalidDocument and creates nothing.
7. Cause 6: the same call with an entry whose Name is "ab". Observe the generic constraint message instead of "Invalid document name: ab".
8. Cause 7: create an association, StartAssociationsOnce it, then DescribeAssociationExecutionTargets with that AssociationId and a fresh uuid4 ExecutionId. Observe 400 AssociationExecutionDoesNotExist instead of 200 with an empty AssociationExecutionTargets list.
9. Cause 8: ssm DescribeOpsItems with OpsItemFilters=[{"Key":"Priority","Values":["2"],"Operator":"GreaterThan"}]. Observe HTTP 200 instead of 400 OpsCenterInvalidArgumentException "InvalidFilter".
10. Cause 9: ssm GetDefaultPatchBaseline with OperatingSystem="NOT_A_REAL_OS" and compare the bracketed enum value set with AWS's ordering (SUSE before ALMA_LINUX).
11. Cause 10: ssm ModifyDocumentPermission with Name=<a document>, PermissionType="Share" and neither AccountIdsToAdd nor AccountIdsToRemove. Observe ValidationException instead of InvalidParameterException "Either AccountIdsToAdd or AccountIdsToRemove must be specified for API operation".
12. Re-replay the 12 checks listed in trace_ids.
