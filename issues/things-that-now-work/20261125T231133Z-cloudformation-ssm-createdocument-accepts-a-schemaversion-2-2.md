Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ssm.deploy-ssm-document-invalid-content-rejected
Issue-Key: cloudformation__ssm.deploy-ssm-document-invalid-content-rejected|run#16|AssertionError|assert status == "ROLLBACK_COMPLETE", f"expected ROLLBACK_COMPLETE, got {status!r}"
Issue-Commit: 8d1b1b750e0b1340c298c8ebe49c21e166bf4879

# cloudformation__ssm: CreateDocument accepts a schemaVersion 2.2 AWS::SSM::Document Content with no mainSteps, so the stack reaches CREATE_COMPLETE instead of rolling back and the document is created

> _Note: this issue was identified against the code base at commit `8d1b1b750e0b1340c298c8ebe49c21e166bf4879`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDocument

## Description
One net-new root cause under the cloudformation__ssm heading.

Cause 1 of 1: an SSM Command document whose Content declares schemaVersion "2.2" but carries no "mainSteps" member is accepted by the emulator's CreateDocument, so a CloudFormation stack that declares such an AWS::SSM::Document creates successfully and the document exists afterwards.

AWS: Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): CreateDocument(Content required, DocumentContent @length min 1; Name required, @pattern ^[a-zA-Z0-9_\-.]{3,128}$; DocumentType enum) declares the client error InvalidDocumentContent (errorKind client, httpError 400, awsQueryError code InvalidDocumentContent). AWS validates the document body against its schemaVersion: for a Command document on schemaVersion "2.2" the "mainSteps" member is mandatory, and a body carrying only schemaVersion and description is refused HTTP 400 InvalidDocumentContent with the message `Missing "mainSteps" in the document.` (entirely fixed text for this error class — nothing in it is taken from the request). Recorded behavior for the CloudFormation path: because the underlying CreateDocument is refused, the AWS::SSM::Document resource goes CREATE_FAILED, the stack rolls back to ROLLBACK_COMPLETE, and the resource event's ResourceStatusReason is the handler wrapper `Resource handler returned message: "Invalid request provided: Missing "mainSteps" in the document. (Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)` — fixed text is the `Resource handler returned message: "Invalid request provided: ` prefix, the inner SSM message, and the ` (Service: Ssm, Status Code: `/`, Request ID: `/`) (SDK Attempt Count: `/`) (RequestToken: `/`, HandlerErrorCode: InvalidRequest)` framing; the variable parts are the status code, the per-request uuids and the attempt count. Afterwards no document exists under that name, so GetDocument(Name=<that name>) is refused InvalidDocument with `Document with name <Name> does not exist.` (fixed text around the Name copied from the request).

Emulator: CreateStack with an AWS::SSM::Document whose Content is {"schemaVersion": "2.2", "description": ...} and no mainSteps settles at StackStatus CREATE_COMPLETE — no CREATE_FAILED event for the resource and no rollback — which means the emulator's CreateDocument accepted the body and minted the document instead of answering 400 InvalidDocumentContent. The content validation that AWS performs on the document body is absent, so the refusal never reaches the CloudFormation resource handler.

Why: a client relying on AWS's behavior gets the opposite verdict on a malformed document. Against AWS a bad document body fails fast at deploy time with a message naming the missing member; against the emulator the deploy reports success and leaves a document in the account that no SSM execution path can run (an association or Run Command against it would fail later, far from the template that caused it). Negative-path tooling — a pipeline that asserts a rollback, a template linter that round-trips through the service, any test that expects InvalidDocumentContent — reads a passing deploy, and the "document does not exist after rollback" invariant is violated because the document does exist.

Fix: the SSM crate's CreateDocument handler validation — validate Content against its schemaVersion and refuse a Command document that lacks the required "mainSteps" member with InvalidDocumentContent (HTTP 400, message `Missing "mainSteps" in the document.`), creating no document. With that refusal in place the cloudformation crate's AWS::SSM::Document resource handler will fail the resource and roll the stack back (the handler's wrapper-text divergence is a separate, already-open cause: 20261117T140330Z-cloudformation-ssm-4-net-new-causes-an-unselected.md Cause 2 covers the `Error occurred during operation '<Op>':` wrapper and the reserved-prefix/schedule messages, but it does not carry this check's replay key and does not cover content validation at all).

Traces: cloudformation__ssm.deploy-ssm-document-invalid-content-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (478/639 passed, 74.8%; emulator up, no bootstrap signal).

[mismatch] cloudformation__ssm.deploy-ssm-document-invalid-content-rejected
  File "check_cloudformation__ssm_deploy_ssm_document_invalid_content_rejected.py", line 92, in run
    assert status == "ROLLBACK_COMPLETE", f"expected ROLLBACK_COMPLETE, got {status!r}"
  AssertionError: expected ROLLBACK_COMPLETE, got 'CREATE_COMPLETE'

What the check does (from the replayed source):
1. ssm CreateDocument(Name=<unique ctrl name>, DocumentType="Command", Content={"schemaVersion":"2.2","description":...,"mainSteps":[{"action":"aws:runShellScript","name":"runShellScript","inputs":{"runCommand":["echo ..."]}}]}) as a positive control; GetDocument reports Status Active (this part passes).
2. cfn CreateStack with Resources.Doc = {Type: "AWS::SSM::Document", Properties: {Name: <unique>, DocumentType: "Command", Content: {"schemaVersion":"2.2","description":"no mainSteps here"}}} — i.e. NO mainSteps.
3. Poll DescribeStacks to a terminal status.

Expected (AWS): ROLLBACK_COMPLETE, a Doc CREATE_FAILED event whose ResourceStatusReason matches
  ^Resource handler returned message: "Invalid request provided: Missing "mainSteps" in the document\. \(Service: Ssm, Status Code: 400, Request ID: <uuid>\) \(SDK Attempt Count: 1\)" \(RequestToken: <uuid>, HandlerErrorCode: InvalidRequest\)$
and afterwards ssm GetDocument(Name=<the stack's doc name>) refused with code InvalidDocument, message "Document with name <doc name> does not exist."

Actual (emulator): StackStatus CREATE_COMPLETE. The run stops at that assertion, so the two downstream assertions (the CREATE_FAILED reason text and the InvalidDocument not-found read) were never reached; the primary, independently observable divergence is that the invalid document body was accepted instead of refused 400 InvalidDocumentContent.

Model evidence: CreateDocument declares InvalidDocumentContent (client, httpError 400, awsQueryError InvalidDocumentContent) alongside DocumentAlreadyExists / InvalidDocumentSchemaVersion / MaxDocumentSizeExceeded — the content-rejection path is modeled and is simply never taken.

The generated document/stack names, RequestToken and Request ID uuids above are per-run values; the requirements are the terminal stack status, the 400 InvalidDocumentContent classification and the fixed message text `Missing "mainSteps" in the document.`.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator from this checkout.
2. Direct SSM path: ssm create-document --name <unique-name> --document-type Command --content '{"schemaVersion":"2.2","description":"no mainSteps here"}'. AWS refuses HTTP 400 InvalidDocumentContent with message `Missing "mainSteps" in the document.` and creates nothing; the emulator accepts it and creates the document (ssm get-document --name <unique-name> then succeeds instead of being refused InvalidDocument "Document with name <unique-name> does not exist.").
3. CloudFormation path: cfn create-stack --stack-name <unique> --template-body '{"Resources":{"Doc":{"Type":"AWS::SSM::Document","Properties":{"Name":"<unique-doc>","DocumentType":"Command","Content":{"schemaVersion":"2.2","description":"no mainSteps here"}}}}}'.
4. Poll cfn describe-stacks --stack-name <unique>: the emulator settles at CREATE_COMPLETE; AWS settles at ROLLBACK_COMPLETE with a Doc CREATE_FAILED event whose ResourceStatusReason wraps the SSM message as `Resource handler returned message: "Invalid request provided: Missing "mainSteps" in the document. (Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)`.
5. cfn describe-stack-events --stack-name <unique> and ssm get-document --name <unique-doc>: on AWS the document does not exist after rollback; on the emulator it does.
6. Re-replay cloudformation__ssm.deploy-ssm-document-invalid-content-rejected to confirm the fix.
