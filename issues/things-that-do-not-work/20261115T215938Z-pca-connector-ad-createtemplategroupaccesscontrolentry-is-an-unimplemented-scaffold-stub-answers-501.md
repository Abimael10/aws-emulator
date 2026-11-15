Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pca-connector-ad.create-template-group-access-control-entry-invalid-access-rights-enum-rejected pca-connector-ad.create-template-group-access-control-entry-invalid-sid-pattern-rejected
Issue-Shape-Id: com.amazonaws.pcaconnectorad#PcaConnectorAd
Issue-Key: pca-connector-ad.create-template-group-access-control-entry-invalid-access-rights-enum-rejected|run#9|AssertionError|assert control_code == "ResourceNotFoundException", (
Issue-Key: pca-connector-ad.create-template-group-access-control-entry-invalid-sid-pattern-rejected|run#9|AssertionError|assert control_code == "ResourceNotFoundException"
Issue-Commit: f6d87ce57ca9b25a0fb0bb3a40dc7f114673a905

# CreateTemplateGroupAccessControlEntry is an unimplemented scaffold stub: answers 501 NotImplemented instead of the Template ResourceNotFoundException (or 200)

> _Note: this issue was identified against the code base at commit `f6d87ce57ca9b25a0fb0bb3a40dc7f114673a905`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTemplateGroupAccessControlEntry

## Description
Cause 1 of 1: CreateTemplateGroupAccessControlEntry never reaches a real handler — every request whose input satisfies the modeled constraints falls through to the scaffold's not-implemented branch, so the operation answers HTTP 501 with error code NotImplemented instead of any behaviour AWS exhibits. The service's centralized validation layer DOES run and is correct (both failing checks got the exact ValidationException wording they expected for an out-of-enum AccessRights.Enroll and for a GroupSecurityIdentifier that fails the SID pattern); the divergence is entirely downstream of validation, on the valid-input path.

AWS: CreateTemplateGroupAccessControlEntry is POST /templates/{TemplateArn}/accessControlEntries with modeled success status 200 and an empty (Unit) output, and models ResourceNotFoundException @httpError 404 among its errors (Smithy: CreateTemplateGroupAccessControlEntry input TemplateArn @required @httpLabel @length 5..200 @pattern ^arn:[\w-]+:pca-connector-ad:[\w-]+:[0-9]+:connector\/<uuid>\/template\/<uuid>$, GroupSecurityIdentifier @required @length 7..256 @pattern ^S-[0-9]-([0-9]+-){1,14}[0-9]+$, GroupDisplayName @required, AccessRights @required; errors AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ServiceQuotaExceededException 402, ThrottlingException 429, ValidationException 400; NotImplemented is not a modeled error of this operation and 501 is not a modeled status). For a well-formed TemplateArn naming a template that does not exist, AWS refuses with 404 ResourceNotFoundException whose Message is this service's not-found sentence — fixed text '<ResourceType> was not found: ' + the identifier exactly as supplied in the request + a trailing '.', i.e. 'Template was not found: <TemplateArn>.' (message text: recorded behavior only; the status, error class and required members ResourceId/ResourceType are the model's). For a TemplateArn that does exist, the call succeeds with 200 and an empty body and the entry becomes readable via ListTemplateGroupAccessControlEntries/GetTemplateGroupAccessControlEntry.

Emulator: any request that clears modeled validation returns HTTP 501, error code NotImplemented — see services/service-com-amazonaws-pcaconnectorad-pcaconnectorad/src/ops/create_template_group_access_control_entry.rs:209, `Err(crate::scaffolded::Scaffolded::not_implemented("CreateTemplateGroupAccessControlEntry"))`, with the `//! SCAFFOLDED, NOT IMPLEMENTED` marker still at line 4. Both failing checks observed control_code 'NotImplemented' where ResourceNotFoundException was required.

Why: a client gets an unmodeled error code and an unmodeled status from this operation on every call. botocore cannot map NotImplemented to any modeled shape, so error handling branches keyed on ResourceNotFoundException (the normal signal that a template ARN is stale or was deleted) never fire and retry/backoff logic keyed on the modeled 4xx/5xx classes misclassifies it as a server fault. Nothing can be done with template group access control entries at all: they can be neither created nor, consequently, listed or read back, so the whole AD-group enroll/autoenroll permission surface of the connector is unusable.

Fix: implement the handler body in services/service-com-amazonaws-pcaconnectorad-pcaconnectorad/src/ops/create_template_group_access_control_entry.rs (the only file for this operation): resolve TemplateArn in the service state; when no such template exists return ResourceNotFoundException with ResourceType 'Template', ResourceId the request's TemplateArn, and Message 'Template was not found: <that TemplateArn>.'; when it exists, record the entry (keyed by TemplateArn + GroupSecurityIdentifier, honouring ClientToken idempotency) and answer 200 with the empty output. Delete the SCAFFOLDED marker line once done. Note the existing open issue 20261029T224504Z-pca-connector-ad-resourcenotfoundexception-message-wording-is-inverted-on-the-connector.md covers the inverted not-found wording on GetConnector/CreateTemplate only; it does NOT cover this operation, which never reaches a not-found path at all — use the same sentence form when adding it here.

Traces: pca-connector-ad.create-template-group-access-control-entry-invalid-access-rights-enum-rejected, pca-connector-ad.create-template-group-access-control-entry-invalid-sid-pattern-rejected.

## Evidence
Both failing checks send a valid-input positive control to the same well-formed, nonexistent TemplateArn (arn:aws:pca-connector-ad:<region>:<account>:connector/<uuid>/template/<uuid>) after a rejection assertion that PASSED.

check create-template-group-access-control-entry-invalid-access-rights-enum-rejected:
  step 1 (passed): AccessRights={'Enroll':'MAYBE','AutoEnroll':'DENY'} -> 400 ValidationException, Message '[instance value ("MAYBE") not found in enum (possible values: ["ALLOW","DENY"])]' — matched.
  step 2 (failed): identical call with AccessRights={'Enroll':'ALLOW','AutoEnroll':'DENY'}
    expected: error code 'ResourceNotFoundException' (HTTP 404), Message 'Template was not found: <the request's TemplateArn>.'
    actual:   error code 'NotImplemented' (HTTP 501)
    AssertionError: "expected ResourceNotFoundException for a valid enum against the same nonexistent TemplateArn (proves the enum check, not a broken call path, caused the first rejection), got 'NotImplemented'"

check create-template-group-access-control-entry-invalid-sid-pattern-rejected:
  step 1 (passed): GroupSecurityIdentifier='not-a-valid-sid' -> 400 ValidationException, Message '[ECMA 262 regex "^S-[0-9]-([0-9]+-){1,14}[0-9]+$" does not match input string "not-a-valid-sid"]' — matched.
  step 2 (failed): identical call with GroupSecurityIdentifier='S-1-5-21-...-1401'
    expected: 'ResourceNotFoundException'; actual: not ResourceNotFoundException (the scaffold's 501 NotImplemented) — AssertionError at `assert control_code == "ResourceNotFoundException"`.

Fixed vs variable parts of the required message: fixed text is 'Template was not found: ' and the trailing '.'; the variable part is the TemplateArn echoed from the request. The ARNs/UUIDs/account/region in any single run are per-run values, not part of the requirement.

Source: create_template_group_access_control_entry.rs:4 ('//! SCAFFOLDED, NOT IMPLEMENTED: this operation answers HTTP 501 NotImplemented') and :209 (the not_implemented return).

## Steps to Reproduce
Boot the emulator and replay the two checks above, or drive it directly:
1. Build a well-formed TemplateArn for a template that does not exist: arn:aws:pca-connector-ad:<region>:<account>:connector/<uuid>/template/<uuid> (satisfies the modeled @pattern).
2. Call CreateTemplateGroupAccessControlEntry(TemplateArn=<that ARN>, GroupSecurityIdentifier='S-1-5-21-1004336348-1177238915-682003330-1401', GroupDisplayName='sim2real-group', AccessRights={'Enroll':'ALLOW','AutoEnroll':'DENY'}).
   Observed: HTTP 501, error code NotImplemented. Expected: HTTP 404, ResourceNotFoundException, Message 'Template was not found: <that ARN>.' with ResourceType 'Template' and ResourceId the ARN.
3. For the success path: CreateConnector + CreateTemplate to obtain a real TemplateArn, then repeat step 2 against it. Observed: HTTP 501 NotImplemented. Expected: HTTP 200 with an empty body, and the entry visible from ListTemplateGroupAccessControlEntries.
4. Control (already correct, do not regress): the same call with AccessRights.Enroll='MAYBE', or with GroupSecurityIdentifier='not-a-valid-sid', is correctly refused with 400 ValidationException and the exact messages quoted in Evidence.
