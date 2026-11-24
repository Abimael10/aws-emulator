Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.associate-ops-item-related-item-duplicate-rejected ssm.modify-document-permission-validation-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.associate-ops-item-related-item-duplicate-rejected|run#15|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: ssm.modify-document-permission-validation-rejected|run#8|AssertionError|assert got == _MALFORMED_ACCOUNT_ID, f"expected {_MALFORMED_ACCOUNT_ID!r}, got {got!r}"
Issue-Commit: f94f81a4e4acb5f06aec5c20cf2f41781bd39454

# SSM error messages diverge: AssociateOpsItemRelatedItem omits the response request id, ModifyDocumentPermission omits the account-id length constraints (2 net-new causes)

> _Note: this issue was identified against the code base at commit `f94f81a4e4acb5f06aec5c20cf2f41781bd39454`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateOpsItemRelatedItem

## Description
Two net-new root causes in the SSM emulator's error-message construction. Both calls are refused with the right HTTP status and the right error code; the Error.Message text diverges from the text AWS returns for that error class.

Cause 1 of 2: AssociateOpsItemRelatedItem's duplicate-association refusal puts the literal string "None" where AWS puts the request id of that very response.
AWS: Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model): AssociateOpsItemRelatedItem models error OpsItemRelatedItemAlreadyExistsException, errorKind client, httpError 400 ("The Amazon Resource Name (ARN) is already associated with the OpsItem"). Associating an already-associated ResourceUri to the same OpsItem a second time is refused HTTP 400 / Error.Code OpsItemRelatedItemAlreadyExistsException. The model does not state the message text, so for the message this is recorded behavior only: AWS returns the Java-SDK-style composite message whose fixed text is "<offending input member> (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemRelatedItemAlreadyExistsException; Request ID: <request id>; Proxy: null)". Fixed parts: the member name that caused the refusal (here "ResourceUri"), and the literals "(Service: AmazonSSM; Status Code: 400; Error Code: ", "; Request ID: ", "; Proxy: null)". The only variable part is <request id>, and its required value is not a recorded constant: it is the request id of the SAME response (ResponseMetadata.RequestId / x-amzn-RequestId). The status code and error code embedded in the text must agree with the response's own 400 / OpsItemRelatedItemAlreadyExistsException.
Emulator: the emulator answers 400 with Error.Code OpsItemRelatedItemAlreadyExistsException (correct) but Error.Message "ResourceUri (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemRelatedItemAlreadyExistsException; Request ID: None; Proxy: null)" — the literal four-character token "None" (a Python/Rust None rendered into the string) in place of the request id, even though the response itself does carry a request id (the replay read one from ResponseMetadata and built the expected text from it).
Why: the message is the only place AWS surfaces the request id for this error class to a caller that logs or parses the message; clients that correlate a support case, a log line or a retry decision by scraping "Request ID: <id>" out of the message get the fixed word "None" for every occurrence, so every duplicate-association failure is indistinguishable and uncorrelatable, and a parser that expects an id-shaped token there fails.
Fix: the message construction for OpsItemRelatedItemAlreadyExistsException in the AssociateOpsItemRelatedItem handler (and whatever shared composite-message helper builds "(Service: ...; Request ID: ...)" texts for SSM) must interpolate the request id the response serializer is about to emit, rather than an unset/absent value; the request id must be threaded into the error path instead of being read as empty there.
Traces: ssm.associate-ops-item-related-item-duplicate-rejected.

Cause 2 of 2: ModifyDocumentPermission's ValidationException for a malformed account id enumerates only the pattern constraint, dropping the two length clauses AWS lists.
AWS: Smithy: ModifyDocumentPermission.AccountIdsToAdd targets AccountIdList (@length min 0 max 20), whose member targets AccountId with @pattern ^(?i)all|[0-9]{12}$. A member value that is not 12 digits and not "all" is refused HTTP 400 / Error.Code ValidationException. The model carries only the pattern on AccountId, so the message enumeration is recorded behavior only: AWS reports ALL THREE member constraints, in this fixed order, in one bracketed list — fixed text "1 validation error detected: Value '<value list>' at '<memberName>' failed to satisfy constraint: Member must satisfy constraint: [Member must have length less than or equal to 12, Member must have length greater than or equal to 0, Member must satisfy regular expression pattern: (?i)all|[0-9]{12}]". Variable parts taken from the request: the rejected value list rendered as '[<v1>,...]' (here '[123]') and the serialized member name ('accountIdsToAdd', or 'accountIdsToRemove' for the other list); the three constraint clauses and the limits 12 / 0 / the pattern are fixed text of this error class.
Emulator: answers 400 / ValidationException (correct code and status) with Error.Message "1 validation error detected: Value '[123]' at 'accountIdsToAdd' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: (?i)all|[0-9]{12}]" — the bracketed list contains only the pattern clause; "Member must have length less than or equal to 12, " and "Member must have length greater than or equal to 0, " are missing.
Why: a client (or a user-facing wrapper, or a test asserting AWS's exact validation text) that surfaces or matches the constraint list sees a different, shorter requirement list than AWS states, so remediation guidance and any exact-message assertion diverge; the emulator understates which constraints AWS enforces on an account id.
Fix: the input validation for ModifyDocumentPermission's AccountIdsToAdd/AccountIdsToRemove members (the shared constrained-string validation message builder that renders the bracketed clause list for SSM ValidationExceptions) must emit the length-max, length-min and pattern clauses in AWS's order for AccountId, not the pattern clause alone.
Traces: ssm.modify-document-permission-validation-rejected.

## Evidence
Cause 1 — ssm.associate-ops-item-related-item-duplicate-rejected (mismatch):
  expected message: 'ResourceUri (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemRelatedItemAlreadyExistsException; Request ID: <the request id of this response>; Proxy: null)'
  got message:      'ResourceUri (Service: AmazonSSM; Status Code: 400; Error Code: OpsItemRelatedItemAlreadyExistsException; Request ID: None; Proxy: null)'
  (the replay built the expected text from the emulator's OWN ResponseMetadata.RequestId, so the divergence is the literal 'None' in the message, not a per-run id difference; Error.Code and HTTP 400 matched)

Cause 2 — ssm.modify-document-permission-validation-rejected (mismatch):
  expected ('ValidationException', "1 validation error detected: Value '[123]' at 'accountIdsToAdd' failed to satisfy constraint: Member must satisfy constraint: [Member must have length less than or equal to 12, Member must have length greater than or equal to 0, Member must satisfy regular expression pattern: (?i)all|[0-9]{12}]")
  got      ('ValidationException', "1 validation error detected: Value '[123]' at 'accountIdsToAdd' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: (?i)all|[0-9]{12}]")
  (the earlier leg of the same check — ModifyDocumentPermission naming neither account list -> InvalidParameterException "Either AccountIdsToAdd or AccountIdsToRemove must be specified for API operation" — passed)

## Steps to Reproduce
Cause 1:
1. CreateOpsItem (any Title/Description/Source) -> OpsItemId.
2. CreateDocument a caller-owned Command document; build its ARN arn:aws:ssm:<region>:<account>:document/<name>.
3. AssociateOpsItemRelatedItem(OpsItemId, AssociationType="RelatesTo", ResourceType="AWS::SSM::Document", ResourceUri=<that ARN>) -> succeeds, returns AssociationId.
4. Repeat the identical AssociateOpsItemRelatedItem call. Observe HTTP 400 / OpsItemRelatedItemAlreadyExistsException, and compare Error.Message's "Request ID: ..." segment against the same response's ResponseMetadata.RequestId: the emulator prints "None" instead of that id.

Cause 2:
1. CreateDocument a Command document (schemaVersion 2.2, aws:runShellScript step).
2. ModifyDocumentPermission(Name=<doc>, PermissionType="Share", AccountIdsToAdd=["123"]).
3. Observe 400 / ValidationException and read Error.Message: the bracketed constraint list contains only the regular-expression clause; AWS lists the length<=12 and length>=0 clauses before it.

Replay: run_behavioral_suite and re-run checks ssm.associate-ops-item-related-item-duplicate-rejected and ssm.modify-document-permission-validation-rejected.
