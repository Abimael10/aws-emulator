Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__ec2__elbv2.deploy-apigateway-vpclink-invalid-target-arn-rejected-and-rolled-back
Issue-Key: apigateway__cloudformation__ec2__elbv2.deploy-apigateway-vpclink-invalid-target-arn-rejected-and-rolled-back|handler_reason#18|AssertionError|assert m is not None, f"not a resource-handler failure reason: {reason!r}"
Issue-Commit: 8ff3b21e7fb9cd785286bd581017af494993989c

# apigateway__cloudformation__ec2__elbv2: a failed AWS::ApiGateway::VpcLink stabilization writes emulator prose into ResourceStatusReason instead of AWS's resource-handler envelope (no HandlerErrorCode)

> _Note: this issue was identified against the code base at commit `8ff3b21e7fb9cd785286bd581017af494993989c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Cause 1 of 1: the CREATE_FAILED stack event for an AWS::ApiGateway::VpcLink that fails stabilization carries an emulator-authored sentence instead of CloudFormation's resource-handler failure envelope, so the event exposes no HandlerErrorCode at all.

AWS: Smithy (apigateway) CreateVpcLink returns a VpcLink whose status is asynchronous (PENDING -> AVAILABLE | FAILED | DELETING) with statusMessage carrying the reason, and models no synchronous error for an unresolvable target; the CloudFormation side is recorded behavior only. CloudFormation surfaces a registry resource provider's failure in the stack event's ResourceStatusReason in exactly one envelope: Resource handler returned message: "<handler message>" (RequestToken: <uuid>, HandlerErrorCode: <code>). For this resource the handler message is `Failed to stabilize Vpc Link with id <vpcLinkId> Status Message NLB ARN not found.` and the code is `GeneralServiceException`. Fixed text, which is the requirement: `Resource handler returned message: "`, `" (RequestToken: `, `, HandlerErrorCode: `, the trailing `)`, plus `Failed to stabilize Vpc Link with id `, ` Status Message NLB ARN not found.` and `GeneralServiceException`. Variable parts, taken from the request and the emulator's own state: the physical VPC-link id the create returned and the per-run request token (the token's value is per-run and is itself never a requirement).

Emulator: the stack does end ROLLBACK_COMPLETE and does emit exactly one VpcLink CREATE_FAILED event, but its ResourceStatusReason is the bare sentence `API Gateway reports VPC link <vpcLinkId> in status FAILED: NLB ARN not found` — unwrapped, with no RequestToken and no HandlerErrorCode member in the text.

Why: a client reading a failed stack parses ResourceStatusReason for that envelope to recover the handler message and, above all, HandlerErrorCode — the machine-readable classification (GeneralServiceException vs InvalidRequest vs NotFound) it branches on to decide retry-vs-fix-the-template. With the envelope absent there is no code to extract, every such parse fails, and IaC tooling and drift/guardrail reporting built on it cannot tell a service-side stabilization failure from a bad-request failure.

Fix: the AWS::ApiGateway::VpcLink resource provider's stabilization-failure path in CloudFormation's resource-provisioning layer — specifically the component that composes ResourceStatusReason for a failed resource, which must wrap every handler failure message in the envelope above with the request token and the handler error code.

Relation to what is already filed: open issue 20261111T095650Z-apigateway-cloudformation-ec2-elbv2-an-aws-apigateway-vpclink.md carries this check's earlier replay key only (`assert status == "ROLLBACK_COMPLETE"` — the stack used to reach CREATE_COMPLETE). The stack now fails and rolls back, so the replay advances past that assertion and the suite does not mark this failure OWNED; the reason-envelope divergence above is the remaining net-new cause and is not covered by that issue's key.

Traces: apigateway__cloudformation__ec2__elbv2.deploy-apigateway-vpclink-invalid-target-arn-rejected-and-rolled-back (the check's handler_reason helper matches ResourceStatusReason against the envelope regex before comparing message and code).

## Evidence
Failing check (suite heading `### apigateway__cloudformation__ec2__elbv2`, marked neither ALREADY FILED nor OWNED):

[mismatch] apigateway__cloudformation__ec2__elbv2.deploy-apigateway-vpclink-invalid-target-arn-rejected-and-rolled-back
  run -> got = handler_reason(rows[0].get("ResourceStatusReason"), ctx.account)
  handler_reason -> assert m is not None, f"not a resource-handler failure reason: {reason!r}"
  AssertionError: not a resource-handler failure reason: 'API Gateway reports VPC link 3azatzv4mr in status FAILED: NLB ARN not found'

Expected (AWS), as the check states it: handler_reason(...) == ("Failed to stabilize Vpc Link with id <link-id> Status Message NLB ARN not found.", "GeneralServiceException"), i.e. ResourceStatusReason == 'Resource handler returned message: "Failed to stabilize Vpc Link with id <link-id> Status Message NLB ARN not found." (RequestToken: <uuid>, HandlerErrorCode: GeneralServiceException)'. The id `3azatzv4mr` and the request token are per-run values that fill the template, not part of it.

Actual (emulator): ResourceStatusReason == 'API Gateway reports VPC link 3azatzv4mr in status FAILED: NLB ARN not found' — no `Resource handler returned message:` prefix, no RequestToken, no HandlerErrorCode.

The rest of the check passes: the control stack (VpcLink targeting a real internal NLB) reaches CREATE_COMPLETE, the bad stack reaches ROLLBACK_COMPLETE, and exactly one VpcLink CREATE_FAILED event is present — only the reason's representation diverges.

Model evidence: apigateway CreateVpcLink's status/statusMessage are the asynchronous provisioning channel; no synchronous error is modeled for an unresolvable targetArns entry, so the CloudFormation event text is recorded behavior.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite.
2. Standalone repro: CreateStack with a single resource AWS::ApiGateway::VpcLink, Properties Name=<unique>, TargetArns=["arn:aws:elasticloadbalancing:<region>:<account>:loadbalancer/net/<nlb-that-does-not-exist>/0123456789abcdef"].
3. Poll DescribeStacks by StackId to a terminal status; it reaches ROLLBACK_COMPLETE (correct).
4. DescribeStackEvents for that stack and take the event with LogicalResourceId=VpcLink and ResourceStatus=CREATE_FAILED.
5. Read ResourceStatusReason: the emulator returns 'API Gateway reports VPC link <id> in status FAILED: NLB ARN not found'. AWS returns 'Resource handler returned message: "Failed to stabilize Vpc Link with id <id> Status Message NLB ARN not found." (RequestToken: <uuid>, HandlerErrorCode: GeneralServiceException)'.
6. Re-replay apigateway__cloudformation__ec2__elbv2.deploy-apigateway-vpclink-invalid-target-arn-rejected-and-rolled-back to confirm a fix.
