Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent__cloudformation.deploy-flow-node-type-enum-rejected
Issue-Key: bedrock-agent__cloudformation|ListFlows|NotImplemented
Issue-Commit: 959acf6fdc1b71cfc1ed24eda8e7e03da38a36d2

# bedrock-agent ListFlows is an unimplemented scaffold: HTTP 501 NotImplemented instead of a 200 page of flowSummaries, so a CloudFormation-deployed AWS::Bedrock::Flow can never be enumerated

> _Note: this issue was identified against the code base at commit `959acf6fdc1b71cfc1ed24eda8e7e03da38a36d2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListFlows

## Description
Cause 1 of 1: bedrock-agent `ListFlows` is still a scaffold stub and refuses every call with HTTP 501 `NotImplemented`, so no caller can enumerate flows — including the flows CloudFormation itself has just provisioned from `AWS::Bedrock::Flow`.

AWS: `ListFlows` is a read-only, paginated GET on `/flows/` (Smithy: ListFlows, http {method: GET, uri: "/flows/", code: 200}; paginated {inputToken: nextToken, outputToken: nextToken, pageSize: maxResults, items: flowSummaries}). Input is `maxResults` (@range 1..1000, httpQuery) and `nextToken` (@length 1..2048, @pattern ^\S*$, httpQuery). On success it answers 200 with `flowSummaries` (REQUIRED, @length max 10 per page — each member a FlowSummary carrying the flow's id, arn, name and status) and, when more results remain, `nextToken`. The only failures the model declares are `ValidationException` (400), `AccessDeniedException` (403), `ThrottlingException` (429) and `InternalServerException` (500). `NotImplemented` is not a modeled error of this operation, and 501 is not a status AWS ever returns for it. An empty account answers 200 with an empty `flowSummaries` list, never an error.

Emulator: every ListFlows call is refused. Replay shows `botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the ListFlows operation: operation ListFlows is scaffolded but not implemented in this emulator`. The handler is the untouched scaffold — its `handle` body ends in `Err(crate::scaffolded::Scaffolded::not_implemented("ListFlows"))` and the file still carries the `SCAFFOLDED, NOT IMPLEMENTED` marker, so the refusal is unconditional and independent of the request. Note the surrounding behaviour already works: in the failing check the control stack declaring `AWS::Bedrock::Flow` reached CREATE_COMPLETE and published its FlowArn/FlowId outputs, so the flow exists in the emulator's state — only the enumeration surface is missing. This is a different defect from the open issue 20261109T123922Z-bedrock-agent-cloudformation-aws-bedrock-prompt-promptversion-flow.md (unregistered `AWS::Bedrock::*` resource types), which does not cover ListFlows, and from the open per-operation stub issues filed for other bedrock-agent operations (e.g. 20261107T230358Z-bedrock-agent-deleteflowalias-stub.md, 20261108T203853Z-bedrock-agent-listingestionjobs-stub.md); no open issue carries ListFlows.

Why: a client that lists flows to discover or reconcile what exists — exactly what the AWS console, the CLI `aws bedrock-agent list-flows` paginator and any CloudFormation-plus-SDK verification flow do — receives a 501 the SDK surfaces as a non-retryable client error instead of a page of summaries. Both outcomes a caller depends on are unreachable: it cannot confirm a flow it just created is present (presence), and it cannot confirm a rolled-back or deleted flow is gone (absence), because the error carries no information either way. Pagination is equally unusable, so nothing built on the `ListFlows` paginator can run against the emulator.

Fix: implement the `ListFlows` handler in services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/list_flows.rs — read the flows the service state holds, project each into a FlowSummary, honour `maxResults`/`nextToken` paging against the modeled 10-item page cap, return 200 with the (possibly empty) required `flowSummaries` list, and refuse a malformed `nextToken` with the modeled `ValidationException` rather than 501. Delete the `SCAFFOLDED, NOT IMPLEMENTED` marker line once it is implemented.

Traces: bedrock-agent__cloudformation.deploy-flow-node-type-enum-rejected — the check deploys a valid-node-type control stack (CREATE_COMPLETE), then calls the ListFlows paginator to confirm the control flow is listed with the stack's FlowId/FlowArn and later to confirm the rolled-back bad flow name is absent; the first ListFlows call is refused 501, so neither the positive nor the negative half of the check is ever reached.

## Evidence
Expected (AWS): HTTP 200 with a JSON body containing the required `flowSummaries` array (each entry a FlowSummary with the flow's id/arn/name/status) and, when further pages exist, `nextToken`; an account with no flows still answers 200 with `flowSummaries: []`. Modeled failures are limited to ValidationException (400), AccessDeniedException (403), ThrottlingException (429) and InternalServerException (500).

Actual (emulator): HTTP 501, error code `NotImplemented`, message `operation ListFlows is scaffolded but not implemented in this emulator`. In that message the fixed emulator-authored text is `operation … is scaffolded but not implemented in this emulator` and the only variable part is the operation name `ListFlows`; AWS has no corresponding status, code or message for this operation at all.

Replay output: `botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the ListFlows operation: operation ListFlows is scaffolded but not implemented in this emulator` (raised from botocore client.py `_make_api_call`).

Handler state in the checkout: services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/src/ops/list_flows.rs line 4 `//! SCAFFOLDED, NOT IMPLEMENTED: this operation answers HTTP 501 NotImplemented` and line 170 `Err(crate::scaffolded::Scaffolded::not_implemented("ListFlows"))`.

## Steps to Reproduce
1. Boot the emulator and deploy a CloudFormation stack whose template declares an `AWS::Bedrock::Flow` with a valid node `Type` (e.g. `Output`) plus the IAM execution role it needs, with Capabilities=[CAPABILITY_IAM]; wait for CREATE_COMPLETE and read the stack Outputs (Ref -> flow ARN, Fn::GetAtt Id -> flow id).
2. With a bedrock-agent client, page the flow list: `for page in client.get_paginator("list_flows").paginate(): page["flowSummaries"]` (equivalently `aws bedrock-agent list-flows`).
3. Observe the first request is answered HTTP 501 `NotImplemented` (`operation ListFlows is scaffolded but not implemented in this emulator`) instead of HTTP 200 with a `flowSummaries` entry whose id and arn equal the stack's FlowId and FlowArn.
4. Calling ListFlows on an account with no flows at all shows the same 501, where AWS answers 200 with an empty `flowSummaries` list.

Behavioral check: bedrock-agent__cloudformation.deploy-flow-node-type-enum-rejected
