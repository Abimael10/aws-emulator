Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.document-get-resolves-default-not-latest
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.document-get-resolves-default-not-latest|run#53|AssertionError|assert bad_default["Message"] == (
Issue-Commit: 11009514ecfa49e7fbff448c76359031cc8c2c5e

# SSM: UpdateDocumentDefaultVersion's InvalidDocumentVersion message reuses GetDocument's "Document version: N is invalid, latest version is M" wording instead of naming the document

> _Note: this issue was identified against the code base at commit `11009514ecfa49e7fbff448c76359031cc8c2c5e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateDocumentDefaultVersion

## Description
One net-new root cause in the ssm crate, grounded in a failing behavioral check listed under '### ssm'. 1 CAUSE — it must be closed before submitting.

Cause 1 of 1: UpdateDocumentDefaultVersion refuses a version the document does not hold with the wrong message text.

AWS: UpdateDocumentDefaultVersion(Name, DocumentVersion) where DocumentVersion is a positive integer the named document holds no version of is refused HTTP 400 with error code InvalidDocumentVersion (Smithy: UpdateDocumentDefaultVersion input Name -> DocumentName @pattern ^[a-zA-Z0-9_\-.]{3,128}$ required, DocumentVersion -> DocumentVersionNumber @pattern ^(^[1-9][0-9]*$)$ required; modeled errors InvalidDocument, InvalidDocumentVersion, InvalidDocumentSchemaVersion, InternalServerError, all httpError 400/500; InvalidDocumentVersion documented as "The document version isn't valid or doesn't exist."). The message AWS returns for this operation is, as recorded, `Document with name <name> with version <version> does not exist.` — the fixed text is `Document with name `, ` with version ` and the trailing ` does not exist.`; the two variable parts are the Name and the DocumentVersion taken verbatim from the request. This is a DIFFERENT sentence from the one GetDocument uses for a nonexistent numeric version (`Document version: <n> is invalid, latest version is <m>`), which the same check asserts separately and which the emulator gets right.

Emulator: the error code is correct (InvalidDocumentVersion) but the message is GetDocument's sentence — `Document version: 99 is invalid, latest version is 2`, i.e. the version from the request plus the document's latest version number from state, with none of AWS's fixed text and no mention of the document name. The handler builds that string literally in services/service-com-amazonaws-ssm-amazonssm/src/ops/update_document_default_version.rs (the InvalidDocumentVersion arm around lines 196-214, `format!("Document version: {requested} is invalid, latest version is {latest_version}")`).

Why: a client that reads or matches the refusal message to learn WHICH document rejected the default-version switch gets a sentence that never names the document, and a client that distinguishes a failed default-version switch from a failed version read by message text cannot tell them apart — both operations now emit the same sentence. Operators and CloudFormation/CDK-style tooling that surface the raw message to users show text AWS never produces for this call.

Fix: the UpdateDocumentDefaultVersion handler's version-resolution refusal path (services/service-com-amazonaws-ssm-amazonssm/src/ops/update_document_default_version.rs) — when the requested version number resolves to no version of the record, raise InvalidDocumentVersion with AWS's `Document with name <name> with version <version> does not exist.` sentence, templated from the request's Name and DocumentVersion, and leave GetDocument's distinct `Document version: ... is invalid, latest version is ...` wording where it belongs. The surrounding behavior is already correct and must stay: the refusal does not move the default (the check's follow-up DescribeDocument assertion passes).

Related but NOT covering this: open issue 20261104T121102Z-ssm-10-net-new-root-causes-across-getdocument.md covers GetDocument's version/VersionName refusal classes and messages; it does not carry this check id and does not touch UpdateDocumentDefaultVersion.

Traces: ssm.document-get-resolves-default-not-latest.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout. Check ssm.document-get-resolves-default-not-latest is listed under '### ssm' and is reported [mismatch].

Failure site (suite report): check_ssm_document_get_resolves_default_not_latest.py line 191, `assert bad_default["Message"] == (f"Document with name {name} with version 99 does not exist.")`, AssertionError whose rendered value is the observed message: `Document version: 99 is invalid, latest version is 2`.

Sequence exercised by the check before the failing assertion, all of which PASSED: CreateDocument (v1) -> UpdateDocument (v2) -> unpinned GetDocument resolves v1 -> $LATEST resolves v2 -> $DEFAULT resolves v1 -> DescribeDocument DefaultVersion=1/LatestVersion=2 -> ListDocumentVersions IsDefaultVersion flags -> UpdateDocumentDefaultVersion(DocumentVersion="2") succeeds and every surface flips -> GetDocument(DocumentVersion="99") refused InvalidDocumentVersion with `Document version: 99 is invalid, latest version is 2` (correct) -> UpdateDocumentDefaultVersion(DocumentVersion="99") refused with code InvalidDocumentVersion (correct) but message `Document version: 99 is invalid, latest version is 2`.

Expected: error code InvalidDocumentVersion, HTTP 400, message `Document with name <name> with version <version> does not exist.` (fixed text `Document with name `, ` with version `, ` does not exist.`; <name> and <version> echoed from the request — `99` and the generated document name in this run are only this run's fill-ins, not part of the requirement).
Actual: error code InvalidDocumentVersion, HTTP 400, message `Document version: <version> is invalid, latest version is <latest>` — wrong fixed text, document name absent, and a state-derived latest-version number AWS does not include for this operation.

Smithy model consulted: services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json (UpdateDocumentDefaultVersion input/output and the four modeled errors). Emulator source: services/service-com-amazonaws-ssm-amazonssm/src/ops/update_document_default_version.rs.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. ssm.create_document(Name=<name>, DocumentType='Command', DocumentFormat='JSON', Content=<a schemaVersion 2.2 aws:runShellScript document>); poll DescribeDocument until Status == 'Active'.
3. ssm.update_document(Name=<name>, Content=<a second revision>, DocumentVersion='$LATEST'); poll DescribeDocument(DocumentVersion='2') until Active.
4. ssm.update_document_default_version(Name=<name>, DocumentVersion='2') — succeeds.
5. ssm.update_document_default_version(Name=<name>, DocumentVersion='99') and read Error.Code and Error.Message from the ClientError. Expected code InvalidDocumentVersion with message 'Document with name <name> with version 99 does not exist.'; observed the same code with message 'Document version: 99 is invalid, latest version is 2'.
6. Re-replay check ssm.document-get-resolves-default-not-latest to confirm the fix.
