Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ssm.deploy-ssm-document-invalid-content-rejected
Issue-Key: cloudformation__ssm.deploy-ssm-document-invalid-content-rejected|run#19|AssertionError|assert reasons and _create_failed_reason().match(reasons[0]), (
Issue-Commit: 043cf7bbddd41f9e9269741e32e8c49c8ecdcab2

# cloudformation: a failed resource-provider call's CREATE_FAILED reason is re-worded ("Error occurred during operation 'CreateDocument'") instead of AWS's "Invalid request provided: ... (Service: Ssm, Status Code: 400, Request ID: ...) (SDK Attempt Count: 1)"

> _Note: this issue was identified against the code base at commit `043cf7bbddd41f9e9269741e32e8c49c8ecdcab2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
One net-new root cause in how CloudFormation renders a downstream service refusal into a stack event's ResourceStatusReason.

Cause 1 of 1: the CREATE_FAILED ResourceStatusReason for a resource whose handler call was refused by the underlying service uses the emulator's own wording and drops the SDK/service attribution.

AWS: a stack declaring AWS::SSM::Document whose Content lacks the required `mainSteps` rolls back (ROLLBACK_COMPLETE) and the resource's CREATE_FAILED event carries exactly:
`Resource handler returned message: "Invalid request provided: <service message> (Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: 1)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)`
The fixed text is `Resource handler returned message: "Invalid request provided: `, then ` (Service: <service sdk id>, Status Code: <the service's HTTP status>, Request ID: `, `) (SDK Attempt Count: 1)" (RequestToken: `, `, HandlerErrorCode: InvalidRequest)`. The variable parts are the downstream service's own error message (here `Missing "mainSteps" in the document.`, from SSM CreateDocument's InvalidDocumentContent — Smithy: CreateDocument errors include InvalidDocumentContent, errorKind client, httpError 400), the service's status code, and the two per-run UUIDs (Request ID, RequestToken), which are graded by shape only.

Emulator: the same event reads
`Resource handler returned message: "Error occurred during operation 'CreateDocument': Missing "mainSteps" in the document." (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)`
— it substitutes a `Error occurred during operation '<ApiName>': ` prefix of its own for AWS's `Invalid request provided: ` prefix, and omits the whole `(Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: 1)` suffix. The outer `Resource handler returned message: "..." (RequestToken: ..., HandlerErrorCode: InvalidRequest)` envelope is correct, and the stack does reach ROLLBACK_COMPLETE.

Why: the reason string is the only machine-readable account of why a resource failed. A client (and the CDK/toolkit diagnostics, which parse it) that matches AWS's wording to attribute the failure to the downstream service and its HTTP status gets no `Service:`/`Status Code:` fields at all, and any match on `Invalid request provided:` fails — so a 400 from the resource's service is indistinguishable from any other handler error.

Fix: the CloudFormation resource-provider error-to-ResourceStatusReason mapping used when emitting the CREATE_FAILED/UPDATE_FAILED stack event (the handler-exception formatter), which must render the downstream service's sdk id, HTTP status and request id in AWS's fixed template rather than its own `Error occurred during operation '<op>'` text.

Traces: cloudformation__ssm.deploy-ssm-document-invalid-content-rejected.

## Evidence
Check cloudformation__ssm.deploy-ssm-document-invalid-content-rejected, mismatch:

  File "check_cloudformation__ssm_deploy_ssm_document_invalid_content_rejected.py", line 99, in run
    assert reasons and _create_failed_reason().match(reasons[0])
  AssertionError: expected the Doc resource's failure to name the missing mainSteps, got ['Resource handler returned message: "Error occurred during operation \'CreateDocument\': Missing "mainSteps" in the document." (RequestToken: 4938bde3-0fc4-64a8-d26f-d9c5fb32433d, HandlerErrorCode: InvalidRequest)']

Expected (real AWS, regex in the check, UUIDs graded by shape):
  Resource handler returned message: "Invalid request provided: Missing "mainSteps" in the document. (Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: 1)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)
Actual (emulator):
  Resource handler returned message: "Error occurred during operation 'CreateDocument': Missing "mainSteps" in the document." (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)

## Steps to Reproduce
1. SSM: CreateDocument a valid Command document (schemaVersion 2.2 with mainSteps) as a control; GetDocument reports Status=Active.
2. CloudFormation: CreateStack with a template whose only resource is `Doc` of Type AWS::SSM::Document, DocumentType=Command and Content={"schemaVersion": "2.2", "description": "no mainSteps here"} (no mainSteps).
3. Poll DescribeStacks until terminal: ROLLBACK_COMPLETE (the emulator matches here).
4. DescribeStackEvents(StackName=<stack>) and read the ResourceStatusReason of the `Doc` CREATE_FAILED event: it must match AWS's `Resource handler returned message: "Invalid request provided: ... (Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: 1)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)` template.
5. Or replay the check id in trace_ids.
