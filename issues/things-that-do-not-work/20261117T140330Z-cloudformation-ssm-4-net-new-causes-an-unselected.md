Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ssm.cfn-ssm-join-if-importvalue-update-rollback-keeps-old-value cloudformation__ssm.fn-if-unselected-branch-importvalue-of-missing-export-not-resolved cloudformation__ssm.cfn-ssm-association-invalid-schedule-expression-rejected-rolls-back cloudformation__ssm.cfn-ssm-document-reserved-name-prefix-rejected cloudformation__ssm.cfn-ssm-parameter-securestring-type-update-rejected-value-intact cloudformation__ssm.cfn-ssm-document-update-method-replace-creates-a-new-physical-document
Issue-Key: cloudformation__ssm.cfn-ssm-join-if-importvalue-update-rollback-keeps-old-value|run#10|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Key: cloudformation__ssm.fn-if-unselected-branch-importvalue-of-missing-export-not-resolved|run#10|AssertionError|assert ok_status == "CREATE_COMPLETE", (
Issue-Key: cloudformation__ssm.cfn-ssm-association-invalid-schedule-expression-rejected-rolls-back|run#11|AssertionError|assert got == _REJECTED, (
Issue-Key: cloudformation__ssm.cfn-ssm-document-reserved-name-prefix-rejected|run#8|AssertionError|assert len(reasons) == 1 and _create_failed_reason(reserved_name).match(reasons[0]), (
Issue-Key: cloudformation__ssm.cfn-ssm-parameter-securestring-type-update-rejected-value-intact|run#18|AssertionError|assert failed[0]["ResourceStatusReason"] == _VALIDATION_REASON, failed[0]["ResourceStatusReason"]
Issue-Key: cloudformation__ssm.cfn-ssm-document-update-method-replace-creates-a-new-physical-document|run#19|AssertionError|assert doc_name_after != doc_name_before, (
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# cloudformation__ssm: 4 net-new causes — an unselected Fn::If branch's Fn::ImportValue is resolved and rolls the stack back, SSM resource-handler failure reasons are emulator-authored, a SecureString type change skips template validation, and UpdateMethod Replace never replaces the document

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Four independent net-new root causes under the cloudformation__ssm heading, each grounded in its own failing behavioral check. A fixer must close all four before submitting.

Cause 1 of 4: CloudFormation resolves the UNSELECTED branch of Fn::If, so a template whose dead branch imports a missing export cannot create.
AWS: recorded behavior only (Fn::If/Fn::ImportValue are template-language semantics, not Smithy members). AWS evaluates the condition first and only resolves the selected branch of Fn::If: a template whose false branch contains Fn::ImportValue of an export that does not exist reaches CREATE_COMPLETE, and the unselected Fn::ImportValue is never looked up. The same holds for an Fn::If nested inside Fn::Join.
Emulator: both templates end ROLLBACK_COMPLETE — the emulator resolves Fn::ImportValue in the branch the condition did not select, fails on the missing export and rolls the stack back.
Why: the standard conditional-import idiom (import a value only in the environments that export it) is unusable: a client cannot deploy one template across accounts/regions where the export exists in some and not others, and the failure is reported as a missing export the template never asked for.
Fix: the intrinsic-function evaluator in the cloudformation service crate — evaluate Fn::If's condition before its branches and resolve only the selected branch (including when Fn::If is an argument of Fn::Join).
Traces: cloudformation__ssm.cfn-ssm-join-if-importvalue-update-rollback-keeps-old-value, cloudformation__ssm.fn-if-unselected-branch-importvalue-of-missing-export-not-resolved.

Cause 2 of 4: a failing SSM resource handler reports an emulator-authored ResourceStatusReason instead of AWS's wrapped service error.
AWS: recorded behavior: when the SSM API refuses a resource operation, the CREATE_FAILED event's ResourceStatusReason is the handler wrapper 'Resource handler returned message: "Invalid request provided: <the SSM service message>  (Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'. The fixed text is "Resource handler returned message: \"Invalid request provided: ", the " (Service: Ssm, Status Code: <code>, Request ID: ", ") (SDK Attempt Count: " and ") (RequestToken: ..., HandlerErrorCode: InvalidRequest)" framing; the variable parts are the SSM message, the status code, and the per-request uuids/attempt count. The inner SSM messages recorded are "Invalid document name <Name>" for a reserved document-name prefix and "Only schedule expressions of type rate, cron, and at are supported." for an unsupported AWS::SSM::Association ScheduleExpression.
Emulator: the reason is 'Resource handler returned message: "Error occurred during operation \'<ApiName>\': <emulator message>" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)' — it substitutes its own "Error occurred during operation '<Op>':" wrapper for AWS's "Invalid request provided:" + (Service/Status Code/Request ID/SDK Attempt Count) suffix, and the inner SSM messages are also its own: "Name '<name>' is prefixed with 'aws': the prefixes aws amazon amzn AWSEC2 AWSConfigRemediation AWSSupport are reserved by Amazon Web Services for use as document name prefixes" and "The schedule is invalid. Verify your cron or rate expression and try again.".
Why: the resource status reason is the only diagnosis a CloudFormation client gets for a failed resource; tooling that parses it (CDK, pipelines, the console) cannot extract the service, status code or request id, and the wording does not match AWS for either operation.
Fix: the resource-handler error wrapper in the cloudformation service crate (the text written into the CREATE_FAILED/UPDATE_FAILED event) plus the CreateDocument reserved-prefix and CreateAssociation schedule-expression refusal messages in the ssm service crate.
Traces: cloudformation__ssm.cfn-ssm-association-invalid-schedule-expression-rejected-rolls-back, cloudformation__ssm.cfn-ssm-document-reserved-name-prefix-rejected.

Cause 3 of 4: changing an AWS::SSM::Parameter's Type to SecureString is rejected by calling PutParameter instead of failing template validation.
AWS: recorded behavior: AWS::SSM::Parameter does not support SecureString, so a stack update that changes Type from String to SecureString fails CloudFormation's own property validation and the failing resource's ResourceStatusReason is exactly "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation." (entirely fixed text) — the SSM API is never called.
Emulator: the update instead calls PutParameter and surfaces its refusal: 'Resource handler returned message: "Error occurred during operation \'PutParameter\': Parameter <name> is a String parameter: Parameter Store doesn\'t support changing a parameter type to SecureString" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'.
Why: a client sees a service-level error for a property its template was never allowed to declare, and the resource-schema validation that AWS performs before touching any resource is absent — so other unsupported property values will likewise reach the service.
Fix: the AWS::SSM::Parameter resource schema/property validation in the cloudformation service crate — reject the unsupported Type during template/property validation and emit AWS's validation reason.
Traces: cloudformation__ssm.cfn-ssm-parameter-securestring-type-update-rejected-value-intact.

Cause 4 of 4: UpdateMethod Replace on AWS::SSM::Document does not create a new physical document.
AWS: recorded behavior: AWS::SSM::Document's UpdateMethod property with value Replace makes a content change a REPLACEMENT — CloudFormation creates a new document under a newly generated physical id, points the logical resource at it and deletes the old one, so the resource's PhysicalResourceId after the update differs from the one before.
Emulator: the update completes but the physical id is unchanged (the same generated document name is reported before and after), i.e. the content was updated in place as if UpdateMethod were NewVersion.
Why: a client that chose Replace precisely because in-place mutation is unsafe (document pinned by associations/automations to a specific name) gets the in-place behavior it asked the service to avoid, and any dependent that captured the old physical id sees no change where AWS shows a new resource.
Fix: the AWS::SSM::Document resource handler's update path in the cloudformation service crate — honour UpdateMethod Replace by classifying the change as a replacement.
Traces: cloudformation__ssm.cfn-ssm-document-update-method-replace-creates-a-new-physical-document.

## Evidence
Behavioral suite, service heading `### cloudformation__ssm`.

Cause 1 — cloudformation__ssm.fn-if-unselected-branch-importvalue-of-missing-export-not-resolved:
AssertionError: a false Condition must leave Fn::If's missing-export branch unselected and unresolved, reaching CREATE_COMPLETE, got 'ROLLBACK_COMPLETE'
cloudformation__ssm.cfn-ssm-join-if-importvalue-update-rollback-keeps-old-value:
AssertionError: a false Condition must leave the missing-export branch unresolved and reach CREATE_COMPLETE, got 'ROLLBACK_COMPLETE'

Cause 2 — cloudformation__ssm.cfn-ssm-document-reserved-name-prefix-rejected:
expected (regex) 'Resource handler returned message: "Invalid request provided: Invalid document name <name>  (Service: Ssm, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'
got ['Resource handler returned message: "Error occurred during operation \'CreateDocument\': Name \'aws-<name>\' is prefixed with \'aws\': the prefixes aws amazon amzn AWSEC2 AWSConfigRemediation AWSSupport are reserved by Amazon Web Services for use as document name prefixes" (RequestToken: 8c9908c0-d12a-fea7-bbe5-ce1bf199f4fc, HandlerErrorCode: InvalidRequest)']
cloudformation__ssm.cfn-ssm-association-invalid-schedule-expression-rejected-rolls-back:
the Association CREATE_FAILED reason was ("Error occurred during operation 'CreateAssociation': The schedule is invalid. Verify your cron or rate expression and try again.", 'InvalidRequest'), expected ('Invalid request provided: Only schedule expressions of type rate, cron, and at are supported. (Service: Ssm, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')

Cause 3 — cloudformation__ssm.cfn-ssm-parameter-securestring-type-update-rejected-value-intact:
expected ResourceStatusReason == "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation."
got 'Resource handler returned message: "Error occurred during operation \'PutParameter\': Parameter <name> is a String parameter: Parameter Store doesn\'t support changing a parameter type to SecureString" (RequestToken: 57981449-2f3e-b71d-6473-55a97719eaf4, HandlerErrorCode: InvalidRequest)'

Cause 4 — cloudformation__ssm.cfn-ssm-document-update-method-replace-creates-a-new-physical-document:
AssertionError: UpdateMethod Replace must create a new physical document, but the physical id stayed '<stack>-Doc-2E3B0FA9BEF1'

All generated stack/document/parameter names, RequestTokens and request ids above are per-run values; the requirements are the statuses and the message templates.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. Cause 1: CreateStack from a template with a Condition that is FALSE and an AWS::SSM::Parameter whose Value is {"Fn::If": [Cond, {"Fn::ImportValue": "<an export that does not exist>"}, "literal"]} (and a variant where the Fn::If sits inside an Fn::Join). Observe ROLLBACK_COMPLETE; AWS reaches CREATE_COMPLETE.
3. Cause 2: CreateStack with an AWS::SSM::Document whose Name begins "aws-", and separately an AWS::SSM::Association with ScheduleExpression that is neither rate/cron/at. Read DescribeStackEvents and compare each CREATE_FAILED ResourceStatusReason with AWS's "Invalid request provided: ... (Service: Ssm, Status Code: 400, Request ID: ...) (SDK Attempt Count: ...)" wrapper.
4. Cause 3: CreateStack with an AWS::SSM::Parameter of Type String, then UpdateStack changing only Type to SecureString. Observe the PutParameter handler error instead of CloudFormation's "Validation failed with 1 error(s). ..." reason.
5. Cause 4: CreateStack with an AWS::SSM::Document carrying UpdateMethod Replace and no explicit Name, note its PhysicalResourceId, then UpdateStack with changed Content. Observe the physical id unchanged.
6. Re-replay the 6 checks listed in trace_ids.
