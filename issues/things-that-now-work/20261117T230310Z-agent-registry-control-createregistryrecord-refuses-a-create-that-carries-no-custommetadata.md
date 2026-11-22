Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: agent-registry-control.submit-rejects-record-noncompliant-with-metadata-schema
Issue-Shape-Id: com.amazonaws.agentregistrycontrol#AgentRegistryControl
Issue-Key: agent-registry-control|CreateRegistryRecord|ValidationException
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# CreateRegistryRecord refuses a create that carries no customMetadata with ValidationException instead of creating the record and reading it back NON_COMPLIANT

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRegistryRecord

## Description
REOPENS 20261110T223112Z-agent-registry-control-registry-configuration-blocks-dropped-custom-metadata-schema-never.md: its fix left the call refused.

This issue has ONE cause.

Cause 1 of 1: CreateRegistryRecord applies the registry's custom-metadata schema to a request that supplies NO customMetadata, refusing the create, where AWS creates the record and records the non-compliance as a read-back verdict.

AWS: Smithy: CreateRegistryRecord POST /registries/{registryId}/records, success code 202; required input members are registryId, name, recordType and descriptors — `customMetadata` -> CustomMetadataDocument (document, @sensitive) is NOT required. The owning registry's customMetadataSchemaConfiguration { defaultSchema, recordTypeSchemaOverrides } constrains the metadata a record carries, but a create that supplies no customMetadata at all is accepted: it returns 202 with {recordArn, status} and the record settles in DRAFT. GetRegistryRecord then reports the verdict through output member `customMetadataSchemaComplianceStatus` -> enum CustomMetadataSchemaComplianceStatus (COMPLIANT | NON_COMPLIANT), documented "Indicates whether a registry record's custom metadata conforms to the registry's current schema. COMPLIANT means all required fields are present and all values match their declared types. NON_COMPLIANT means the metadata does not satisfy the current schema, for example because the schema was updated after the record was last modified" — so a record whose metadata is absent while the applicable schema declares required properties reads NON_COMPLIANT and is not refused at create. The required-property refusal belongs to SubmitRegistryRecordForApproval (modeled ValidationException, errorKind client, httpError 400): submitting such a record is refused 400 / x-amzn-errortype: ValidationException with the message whose fixed text is "Custom metadata validation failed for record type <recordType>: : required property '<member>' not found", where <recordType> is the recordType from the request and <member> is the required property name taken from the registry's stored schema; the record stays DRAFT. (Recorded behavior confirms the split: a create that DOES supply violating customMetadata is refused 400 at create — that refusal is the already-open 20261111T015454Z-agent-registry-control-3-net-new-causes-createregistryrecord.md Cause 2, which does NOT cover this check — while a create that supplies none succeeds.)

Emulator: the create of a CUSTOM record with descriptors={'custom': {'data': '{}'}} and no customMetadata member, under a registry whose defaultSchema requires 'environment', is refused with HTTP 400 ValidationException "Custom metadata validation failed for record type CUSTOM: : required property 'environment' not found". No record is created, so the record never exists to read a compliance verdict from and the submit-time refusal can never be observed. The schema evaluation treats missing metadata as a violation of the create request rather than as a NON_COMPLIANT state of the created record.

Why: a client that creates a record first and fills its metadata afterwards (the documented workflow behind NON_COMPLIANT, and the only workflow available when a registry's schema is tightened later) cannot create the record at all: every create without complete metadata is rejected. Remediation tooling that lists NON_COMPLIANT records and fixes them can never reach that state, and a client's error path for a refused submission is never exercised because the refusal has been moved to the wrong operation.

Fix: CreateRegistryRecord's input validation (the operation's handler) must skip the schema evaluation as a request rejection when the request carries no customMetadata, and record state must instead store the computed compliance verdict (NON_COMPLIANT when the applicable schema's required properties are unsatisfied) for GetRegistryRecord/ListRegistryRecords to serve; the required-property ValidationException stays on SubmitRegistryRecordForApproval, which must refuse a NON_COMPLIANT record with that message and leave it DRAFT.

Traces: agent-registry-control.submit-rejects-record-noncompliant-with-metadata-schema.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (12/21 checks passed; agent-registry-control answered 12/18, so the emulator is serving).

[error] agent-registry-control.submit-rejects-record-noncompliant-with-metadata-schema — the check creates a registry with approvalConfiguration.autoApprovalRules=['APPROVE_ALL'] and customMetadataSchemaConfiguration.defaultSchema = {"type":"object","properties":{"environment":{"type":"string","enum":["dev","prod"]},"critical":{"type":"boolean"}},"required":["environment"]}, then calls CreateRegistryRecord(recordType='CUSTOM', descriptors={'custom': {'data': '{}'}}) with NO customMetadata. Harness output:

  botocore.errorfactory.ValidationException: An error occurred (ValidationException) when calling the CreateRegistryRecord operation: Custom metadata validation failed for record type CUSTOM: : required property 'environment' not found

Expected: 202 {recordArn, status} and the record reaching DRAFT, then GetRegistryRecord reporting customMetadataSchemaComplianceStatus == 'NON_COMPLIANT', then SubmitRegistryRecordForApproval refused 400 ValidationException with exactly that same message while the record remains DRAFT. Actual: the create itself is refused with that message, so no record exists and the remaining assertions are unreachable.

Message anatomy: fixed text "Custom metadata validation failed for record type " + <recordType from the request> + ": : required property '" + <required member name from the registry's stored schema> + "' not found". The wording is right; the operation it is attached to is wrong.

This check is listed in Issue-Trace-Ids of the open 20261111T015454Z-agent-registry-control-3-net-new-causes-createregistryrecord.md, but under a different failure (KeyError on the missing customMetadataSchemaComplianceStatus member at a 200 GetRegistryRecord body); the create is now refused outright, which that issue does not describe and the replay did not mark as owned.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 (run_behavioral_suite) and look at check agent-registry-control.submit-rejects-record-noncompliant-with-metadata-schema.
3. CreateRegistry with customMetadataSchemaConfiguration.defaultSchema requiring 'environment' (enum ["dev","prod"]) and approvalConfiguration.autoApprovalRules=['APPROVE_ALL']; wait for READY.
4. CreateRegistryRecord(registryId=<id>, name=<unique>, recordType='CUSTOM', descriptors={'custom': {'data': '{}'}}) with no customMetadata -> expect 202 and the record settling in DRAFT; the emulator returns 400 ValidationException "Custom metadata validation failed for record type CUSTOM: : required property 'environment' not found".
5. GetRegistryRecord on that record -> expect customMetadataSchemaComplianceStatus == 'NON_COMPLIANT'.
6. SubmitRegistryRecordForApproval on it -> expect 400 ValidationException with the message of step 4 and the record still DRAFT (not auto-approved despite APPROVE_ALL).
