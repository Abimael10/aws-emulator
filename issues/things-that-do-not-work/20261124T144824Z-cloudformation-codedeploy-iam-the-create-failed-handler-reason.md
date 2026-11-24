Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__codedeploy.deploy-cfn-codedeploy-deploymentgroup-invalid-role-rejected
Issue-Key: cloudformation__codedeploy.deploy-cfn-codedeploy-deploymentgroup-invalid-role-rejected|run#23|AssertionError|assert got == want, f"handler reason {got!r}, expected {want!r}"
Issue-Commit: 4e68013acb17007a4b50a9cc9c0932294de10e0a

# cloudformation__codedeploy__iam: the CREATE_FAILED handler reason for an AWS::CodeDeploy::DeploymentGroup with a non-assumable service role omits the AWS SDK diagnostic envelope "(Service: CodeDeploy, Status Code: 400, Request ID: ...) (SDK Attempt Count: ...)" (1 cause)

> _Note: this issue was identified against the code base at commit `4e68013acb17007a4b50a9cc9c0932294de10e0a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Cause 1 of 1: when a CloudFormation resource provider surfaces a downstream service refusal, the quoted message inside the handler-reason wrapper carries only the service's own sentence; AWS's quoted message also carries the SDK client-side diagnostic envelope the provider's SDK exception appended.

AWS: recorded behavior only for the envelope's composition — the Smithy model for CloudFormation does not describe ResourceStatusReason text. What AWS produces for a resource whose provider caught a service exception is a CREATE_FAILED event on the logical resource whose ResourceStatusReason is
  Resource handler returned message: "<service message> (Service: <ServiceId>, Status Code: <http status>, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: <code>)
Fixed text: the literal frames `Resource handler returned message: "`, `" (RequestToken: `, `, HandlerErrorCode: `, `)`, and inside the quotes the literal frames ` (Service: `, `, Status Code: `, `, Request ID: `, `) (SDK Attempt Count: `, `)`. Variable parts: `<service message>` is the downstream refusal verbatim — here CodeDeploy's `AWS CodeDeploy does not have the permissions required to assume the role <role-arn>.` with the role ARN taken from the template's ServiceRoleArn; `<ServiceId>` is the refusing service's SDK service id (`CodeDeploy`); `<http status>` is that refusal's HTTP status (400 for CodeDeploy's InvalidRoleException); `<id>`, `<n>` and `<uuid>` are per-run and are masked by the check; `<code>` is the HandlerErrorCode (`InvalidRequest`). The stack then reaches ROLLBACK_COMPLETE and the AWS::CodeDeploy::Application the stack created is deleted.

Emulator: the stack does roll back and the HandlerErrorCode is `InvalidRequest`, but the quoted message stops after the CodeDeploy sentence — `AWS CodeDeploy does not have the permissions required to assume the role <role-arn>.` — with the whole ` (Service: CodeDeploy, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)` suffix absent. The check masks Request ID and SDK Attempt Count before comparing, so this is not a per-run-value difference: the fixed frames `(Service: `, `Status Code: ` and `(SDK Attempt Count: ` never appear.

Why: the handler reason is the only machine-readable account of why a stack rolled back. Clients and CI gates parse it to recover the refusing service, the HTTP status class (4xx caller error vs 5xx retryable) and the request id to quote in a support case. With the envelope stripped, a consumer cannot tell which service refused or whether the failure is retryable, and any log/alarm rule or regression test matching AWS's reason text fails against the emulator.

Fix: the resource-handler error-to-event formatting in the CloudFormation emulator — the path that builds ResourceStatusReason from a provider error must render the downstream service error with its SDK envelope (service id, HTTP status, request id, attempt count) inside the quoted message, not just the service's message string. This applies to every provider that wraps a downstream refusal, not only AWS::CodeDeploy::DeploymentGroup.

Related but not covering: open issue 20261109T135826Z-cloudformation-codedeploy-iam-cloudformation-stack-with-an-aws-codedeploy-deploymentgroup-naming.md carries this check id but is keyed on the stack ending CREATE_COMPLETE instead of ROLLBACK_COMPLETE; that part now behaves and the check proceeds past it, so the surviving reason-text divergence is not gated by that issue's replay key.

Traces: cloudformation__codedeploy.deploy-cfn-codedeploy-deploymentgroup-invalid-role-rejected.

## Evidence
Expected (AWS) — masked handler reason of the CDDeploymentGroup CREATE_FAILED event:
  ('AWS CodeDeploy does not have the permissions required to assume the role arn:aws:iam::<account>:role/<name>. (Service: CodeDeploy, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')

Actual (emulator):
  ('AWS CodeDeploy does not have the permissions required to assume the role arn:aws:iam::<account>:role/<name>.', 'InvalidRequest')

AssertionError at check_cloudformation__codedeploy_deploy_cfn_codedeploy_deploymentgroup_invalid_role_rejected.py line 255: "handler reason (...) , expected (...)". The surrounding assertions passed: the invalid-role stack settled at ROLLBACK_COMPLETE, the reason matched the `Resource handler returned message: "..." (RequestToken: ..., HandlerErrorCode: ...)` wrapper regex, and the HandlerErrorCode was InvalidRequest. `<account>`, `<name>`, `<id>` and `<n>` are masked by the check's own normalizer, so only the missing `(Service: ..., Status Code: ..., Request ID: ...) (SDK Attempt Count: ...)` fixed text differs.

## Steps to Reproduce
1. iam:CreateRole a role trusting codedeploy.amazonaws.com, attach AWSCodeDeployRole, and deploy the control template (AWS::CodeDeploy::Application + AWS::CodeDeploy::DeploymentGroup with one Ec2TagFilter) with it — the stack reaches CREATE_COMPLETE.
2. Build an ARN for a role that was never created: arn:aws:iam::<account>:role/<nonexistent-name>.
3. cloudformation:CreateStack with the same template, passing that ARN as the ServiceRoleArn parameter.
4. Poll DescribeStacks to a terminal status — ROLLBACK_COMPLETE (correct).
5. cloudformation:DescribeStackEvents and read the single CREATE_FAILED row for logical id CDDeploymentGroup; parse its ResourceStatusReason with /^Resource handler returned message: "(?<message>.*)" \(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?<code>[A-Za-z]+)\)$/.
6. Observe <message> ends at "...assume the role <arn>." with no " (Service: CodeDeploy, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" suffix.
