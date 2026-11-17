Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.describe-patch-group-state-aggregates-counts ssm.deploy-ssm-document-via-cloudformation-then-send-command ssm.global-infrastructure-region-public-parameters-name-us-east-1 ssm.parameter-tier-upgrade-is-one-way-and-intelligent-tiering-derives-the-tier ssm.deploy-document-via-cloudformation-then-associable
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|SendCommand|InvalidDocument
Issue-Key: ssm.deploy-ssm-document-via-cloudformation-then-send-command|run#16|AssertionError|assert missing_doc_message == "", missing_doc_message
Issue-Key: ssm|GetParametersByPath|AccessDeniedException
Issue-Key: ssm.parameter-tier-upgrade-is-one-way-and-intelligent-tiering-derives-the-tier|run#30|AssertionError|assert downgrade_message == (
Issue-Key: ssm.deploy-document-via-cloudformation-then-associable|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Commit: 09a4de48f58372094bfcb9a0bb2c4d192074b72e

# ssm: AWS-RunPatchBaseline missing from the document catalog, InvalidDocument carries a message AWS leaves empty, global-infrastructure public parameters refused, advanced-tier downgrade message wrong, association Overview never settles

> _Note: this issue was identified against the code base at commit `09a4de48f58372094bfcb9a0bb2c4d192074b72e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendCommand

## Description
Five net-new root causes for this service.

Cause 1 of 5: The AWS-owned Command document AWS-RunPatchBaseline is absent from the document catalog.
AWS: AWS-RunPatchBaseline is an AWS-owned SSM Command document present in every account and region; SendCommand(DocumentName='AWS-RunPatchBaseline') is accepted and is the documented way to scan or install patches, after which DescribeInstancePatchStates/DescribePatchGroupState report the Installed/Missing/Failed counts. Smithy: SendCommand.DocumentName is required (DocumentARN, pattern ^[a-zA-Z0-9_\-.:/]{3,128}$) and InvalidDocument ("The specified SSM document doesn't exist", HTTP 400) is the refusal for a document that does not exist.
Emulator: SendCommand(DocumentName='AWS-RunPatchBaseline') is refused with HTTP 400 InvalidDocument and the message "Document AWS-RunPatchBaseline doesn't exist", so no patch scan can ever be started and nothing downstream of it can be observed.
Why: the entire Patch Manager workflow (scan, compliance, patch-group state) is unreachable; a client that relies on an AWS-owned document present in every real account gets a hard refusal.
Fix: seed the AWS-owned document catalog (AWS-RunPatchBaseline and its siblings) in the SSM document state that services/service-com-amazonaws-ssm-amazonssm/src/ops/send_command.rs and the GetDocument/DescribeDocument handlers resolve against.
Traces: ssm.describe-patch-group-state-aggregates-counts.

Cause 2 of 5: SendCommand's InvalidDocument for a nonexistent DocumentName carries a message where AWS sends none.
AWS: SendCommand with a DocumentName that does not exist is refused HTTP 400 with error code InvalidDocument and an EMPTY message — the error code alone identifies the document-specific failure (this is what distinguishes it from the InvalidInstanceId refusal, whose message is the fixed text "Instances not in a valid state for account"). Smithy: InvalidDocument has a single optional Message member; the recorded AWS response leaves it empty.
Emulator: the refusal is InvalidDocument but with the emulator-authored message "Document <name> doesn't exist" (the document name is the variable part; the surrounding text is emulator-invented).
Why: clients and test harnesses that assert on the exact refusal payload see a message AWS never sends; error-message-driven branching diverges.
Fix: the InvalidDocument construction in the SendCommand handler (services/service-com-amazonaws-ssm-amazonssm/src/ops/send_command.rs) must emit the code with no message.
Traces: ssm.deploy-ssm-document-via-cloudformation-then-send-command.

Cause 3 of 5: The public /aws/service/global-infrastructure parameters are refused instead of served.
AWS: the public parameter hierarchy /aws/service/global-infrastructure/regions is readable by any caller: GetParametersByPath(Path='/aws/service/global-infrastructure/regions') lists one String parameter per Region whose value is the Region code, and GetParameter('/aws/service/global-infrastructure/regions/us-east-1/longName') returns the Region's long name. Smithy: GetParametersByPath models no AccessDenied error at all — its modeled failures are InternalServerError, InvalidFilterKey, InvalidFilterOption, InvalidFilterValue, InvalidKeyId, InvalidNextToken — so an authorization refusal on a public path is not a modeled outcome.
Emulator: the call fails with AccessDeniedException and the message 'No access to "/aws/" namespace: aws/service/global-infrastructure/regions is not a valid namespace', i.e. the whole /aws/ public namespace is treated as forbidden rather than as public reference data.
Why: Region/service discovery through public parameters — a common bootstrap step for CLIs, CDK apps and installers — fails outright with an error the SDK does not even model for this operation.
Fix: the public-parameter namespace handling and reference data behind services/service-com-amazonaws-ssm-amazonssm/src/ops/get_parameters_by_path.rs and get_parameter.rs. (The open issue 20261107T081213Z-ssm-3-net-new-causes-a-launched-ec2.md covers a different public-parameter check — DescribeParameters not listing public parameters — and does not carry this check's replay key.)
Traces: ssm.global-infrastructure-region-public-parameters-name-us-east-1.

Cause 4 of 5: PutParameter's refusal of an advanced-to-standard tier downgrade uses emulator-authored wording.
AWS: an advanced parameter cannot be reverted to the standard tier; PutParameter(Tier='Standard', Overwrite=true) on it is refused HTTP 400 ValidationException with the fixed text "This parameter uses the advanced-parameter tier. You can't downgrade a parameter from the advanced-parameter tier to the standard-parameter tier. If necessary, you can delete the advanced parameter and recreate it as a standard parameter. Be aware that standard parameters have a value limit of 4096 characters." (all fixed text, no request-derived values). Smithy: PutParameter.Tier -> ParameterTier enum; the refusal class is the service-common ValidationException.
Emulator: the code is right but the message is the emulator-authored "Parameter <name> is an Advanced parameter and can't be reverted to the Standard tier" — it interpolates the parameter name, which AWS's text does not contain, and omits AWS's remediation sentences.
Why: a client surfacing or matching the refusal text shows wording AWS never produced; the guidance AWS gives (delete and recreate, 4096-character limit) is lost.
Fix: the tier-downgrade validation message in services/service-com-amazonaws-ssm-amazonssm/src/ops/put_parameter.rs.
Traces: ssm.parameter-tier-upgrade-is-one-way-and-intelligent-tiering-derives-the-tier.

Cause 5 of 5: A State Manager association created on a CloudFormation-deployed document never settles its Overview status.
AWS: CreateAssociation returns an AssociationDescription whose Overview.Status starts Pending and, with no managed node matching the target tag, settles asynchronously at Overview.Status='Success' with DetailedStatus='Success'; DescribeAssociation reports the settled overview within minutes.
Emulator: the association is created but its Overview never reaches a terminal value, so the check exhausts its budget and aborts ("Check timed out after 60.0s") while polling DescribeAssociation for Success/Failed.
Why: any client that waits for an association to converge after deploying an AWS::SSM::Document waits forever; association health can never be read.
Fix: the association execution/overview state machine behind CreateAssociation/DescribeAssociation in services/service-com-amazonaws-ssm-amazonssm/src/ops/create_association.rs and the association state DescribeAssociation projects.
Traces: ssm.deploy-document-via-cloudformation-then-associable.

## Evidence
Cause 1 — ssm.describe-patch-group-state-aggregates-counts: "botocore.errorfactory.InvalidDocument: An error occurred (InvalidDocument) when calling the SendCommand operation: Document AWS-RunPatchBaseline doesn't exist". Expected: SendCommand accepted, since AWS-RunPatchBaseline is AWS-owned and always present.
Cause 2 — ssm.deploy-ssm-document-via-cloudformation-then-send-command: the check asserts InvalidDocument with an empty message and fails with "AssertionError: Document <missing-doc-name> doesn't exist" (the actual message). Expected code InvalidDocument, message "" (the document name is the only variable part of the emulator's text).
Cause 3 — ssm.global-infrastructure-region-public-parameters-name-us-east-1: "botocore.errorfactory.AccessDeniedException: An error occurred (AccessDeniedException) when calling the GetParametersByPath operation: No access to \"/aws/\" namespace: aws/service/global-infrastructure/regions is not a valid namespace". Expected: a page of String parameters, e.g. /aws/service/global-infrastructure/regions/us-east-1 -> 'us-east-1'.
Cause 4 — ssm.parameter-tier-upgrade-is-one-way-and-intelligent-tiering-derives-the-tier: expected message "This parameter uses the advanced-parameter tier. You can't downgrade a parameter from the advanced-parameter tier to the standard-parameter tier. If necessary, you can delete the advanced parameter and recreate it as a standard parameter. Be aware that standard parameters have a value limit of 4096 characters."; actual "Parameter /<name> is an Advanced parameter and can't be reverted to the Standard tier" (the name is a per-run value).
Cause 5 — ssm.deploy-document-via-cloudformation-then-associable: "Error: Check timed out after 60.0s" while polling DescribeAssociation for Overview.Status in ('Success','Failed'). Expected Overview.Status='Success', DetailedStatus='Success'.

## Steps to Reproduce
Cause 1: ssm SendCommand(DocumentName='AWS-RunPatchBaseline', InstanceIds=[<a managed node>], Parameters={'Operation':['Scan']}) — refused InvalidDocument instead of accepted.
Cause 2: ssm SendCommand(DocumentName='<a name that does not exist>', InstanceIds=['i-0123456789abcdef0']) — the InvalidDocument error carries a non-empty message.
Cause 3: ssm GetParametersByPath(Path='/aws/service/global-infrastructure/regions') — AccessDeniedException instead of the Region parameters; likewise GetParameter('/aws/service/global-infrastructure/regions/us-east-1/longName').
Cause 4: PutParameter(Name=<n>, Value=<5 KB>, Type='String', Tier='Advanced'), then PutParameter(Name=<n>, Value=<1 KB>, Type='String', Tier='Standard', Overwrite=True) — ValidationException with the wrong message text.
Cause 5: deploy an AWS::SSM::Document via CloudFormation, CreateAssociation(Name=<doc>, Targets=[{Key:'tag:sim2real-target',Values:['<tag no node carries>']}]), then poll DescribeAssociation(AssociationId) — Overview.Status never settles.
