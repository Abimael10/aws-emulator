Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-data-automation.library-delete-while-referenced-by-project-validation-rejected
Issue-Shape-Id: com.amazonaws.bedrockdataautomation#AmazonBedrockKeystoneBuildTimeService
Issue-Key: bedrock-data-automation.library-delete-while-referenced-by-project-validation-rejected|_client_error#18|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: 6da3155c33a76acb7439be1ec6d6d9dad4c485c0

# DeleteDataAutomationLibrary accepts deletion of a library still referenced by a DataAutomationProject instead of refusing with ValidationException

> _Note: this issue was identified against the code base at commit `6da3155c33a76acb7439be1ec6d6d9dad4c485c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteDataAutomationLibrary

## Description
Cause 1 of 1: DeleteDataAutomationLibrary performs no referential-integrity check against projects that reference the library, so a referenced library is deleted instead of refused. Failing check: bedrock-data-automation.library-delete-while-referenced-by-project-validation-rejected.

AWS: DeleteDataAutomationLibrary models ValidationException (HTTP 400, "thrown when the request's input validation fails") among its errors (Smithy: DeleteDataAutomationLibrary errors = AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; success is 200 with {libraryArn, status: ACTIVE|DELETING}). Recorded behavior: when a DataAutomationProject still names the library in dataAutomationLibraryConfiguration.libraries[].libraryArn, the delete is refused with HTTP 400, error code ValidationException and the message "Unable to delete the library as it is being referenced by some project. You must remove it from all referring projects before you can delete the library." That whole sentence is FIXED text for this error class — it carries no variable part (no ARN, project name or count is interpolated), so it must be emitted verbatim. The library is unaffected by the refused call: a following GetDataAutomationLibrary still reports status ACTIVE. After the referring project is deleted (DeleteDataAutomationProject), the identical DeleteDataAutomationLibrary call succeeds with 200 and status DELETING, and the library then settles to ResourceNotFoundException with message "The specified library Arn not found." on read. The model says nothing further about the cross-resource rule; the sentence and the 400 are from the recording.

Emulator: the call SUCCEEDS. The replay shows HTTP 200 with body {"libraryArn": "arn:aws:bedrock:<region>:<account>:data-automation-library/<id>", "status": "DELETING"} for a library that a just-created project references, and the check fails at the point where it expects a ClientError (AssertionError: expected the call to be rejected, but it SUCCEEDED and returned ... 'status': 'DELETING'). The handler only checks region admission on the input ARN, settles due deletions, and accepts the deletion if the ARN resolves; nothing consults the project store for referrers. The positive control in the same check (deleting an UNREFERENCED library) passes, so the defect is specifically the missing referenced-library refusal, not a broken delete.

Why: a client relying on AWS's behavior treats the 400 ValidationException as the guarantee that a library backing live projects cannot be removed out from under them. Against the emulator the library enters DELETING and then disappears, leaving every project that references it pointing at a non-existent library — projects silently become unusable and the client never sees the error it is written to handle, so dependency-ordering logic (remove the reference, then delete) is never exercised and teardown/ordering bugs pass in the emulator and fail against AWS.

Fix: the DeleteDataAutomationLibrary handler's validation step, before the deletion is accepted — it must scan the project store for any project whose dataAutomationLibraryConfiguration references the target libraryArn and, when one exists, return the modeled ValidationException with the fixed sentence above and leave the library record untouched (status ACTIVE), so a later delete after the referrer is gone still succeeds. The cross-resource lookup belongs with the library/project state so the read of referrers happens under the same store guard as the acceptance.

Traces: bedrock-data-automation.library-delete-while-referenced-by-project-validation-rejected. Handler as replayed: services/service-com-amazonaws-bedrockdataautomation-amazonbedrockkeystonebuildtimeservice/src/ops/delete_data_automation_library.rs lines 152-206 (region refusal, settle, accept_library_deletion — no referrer check). Note: the open issue 20261103T163536Z-bedrock-data-automation-deletedataautomationlibrary-stub.md concerns the operation being an unimplemented scaffold stub and does not cover this referential-integrity refusal; the operation now answers 200.

## Evidence
Expected (AWS): DeleteDataAutomationLibrary on a library referenced by a project -> HTTP 400, Error.Code = "ValidationException", Error.Message = "Unable to delete the library as it is being referenced by some project. You must remove it from all referring projects before you can delete the library." (entire sentence is fixed text); library unchanged, GetDataAutomationLibrary still returns status ACTIVE; after DeleteDataAutomationProject the same delete returns 200 {libraryArn, status: "DELETING"} and the library later reads as ResourceNotFoundException "The specified library Arn not found."

Actual (emulator): HTTP 200, body {"libraryArn": "arn:aws:bedrock:<region>:<account>:data-automation-library/<id>", "status": "DELETING"} — no error at all. Check failure: AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'libraryArn': 'arn:aws:bedrock:us-west-2:000000000000:data-automation-library/...', 'status': 'DELETING'} (check_bedrock_data_automation_library_delete_while_referenced_by_project_validation_rejected.py line 67 -> _client_error line 101). Positive control (delete of an unreferenced library) succeeded as expected, isolating the missing refusal.

## Steps to Reproduce
1. CreateDataAutomationLibrary(libraryName=<name>) -> capture libraryArn.
2. CreateDataAutomationProject(projectName=<name>, standardOutputConfiguration={}, dataAutomationLibraryConfiguration={"libraries": [{"libraryArn": <libraryArn>}]}) -> capture projectArn.
3. DeleteDataAutomationLibrary(libraryArn=<libraryArn>).
   Expected: HTTP 400 ValidationException with the fixed message "Unable to delete the library as it is being referenced by some project. You must remove it from all referring projects before you can delete the library."; observed: HTTP 200 with status DELETING.
4. GetDataAutomationLibrary(libraryArn=<libraryArn>) -> AWS still reports status ACTIVE; the emulator reports DELETING and the library later vanishes.
5. Control that must keep working: DeleteDataAutomationProject(projectArn=<projectArn>) then DeleteDataAutomationLibrary(libraryArn=<libraryArn>) -> 200 {status: "DELETING"}, and a read of the library eventually gives ResourceNotFoundException "The specified library Arn not found."
