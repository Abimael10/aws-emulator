Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation.cfn-apigateway-deployment-in-place-description-retained-api-move-then-not-found apigateway__cloudformation.cfn-apigateway-method-mock-in-place-update-retained-verb-replacement-then-not-found apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found apigateway__cloudformation.cfn-apigateway-resource-retained-path-replacement-then-not-found apigateway__cloudformation.cfn-apigateway-restapi-mock-in-place-update-retained-stage-replacement-then-not-found
Issue-Key: apigateway__cloudformation.cfn-apigateway-deployment-in-place-description-retained-api-move-then-not-found|_expect#16|AssertionError|raise AssertionError(
Issue-Key: apigateway__cloudformation.cfn-apigateway-method-mock-in-place-update-retained-verb-replacement-then-not-found|_expect#20|AssertionError|raise AssertionError(
Issue-Key: apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found|_expect#18|AssertionError|raise AssertionError(
Issue-Key: apigateway__cloudformation.cfn-apigateway-resource-retained-path-replacement-then-not-found|run#18|AssertionError|assert ids["Api"] == api_id and new_res != old_res, f"only the resource is replaced: {ids!r}"
Issue-Key: apigateway__cloudformation.cfn-apigateway-restapi-mock-in-place-update-retained-stage-replacement-then-not-found|_expect#37|AssertionError|raise AssertionError(
Issue-Commit: d3cdd55e8b72332e5c71fd34997111c995fed5da

# apigateway__cloudformation: 3 net-new causes — in-place updates of Deployment/Method/Stage are refused NotImplemented, a Resource PathPart change is not a replacement, and stack delete leaves the Deployment DELETE_FAILED

> _Note: this issue was identified against the code base at commit `d3cdd55e8b72332e5c71fd34997111c995fed5da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Three net-new root causes in the CloudFormation resource handlers for API Gateway. The open issue 20261111T111216Z-apigateway-cloudformation-in-place-updates-of-aws-apigateway-deployment-method.md describes the same refusal class for Deployment/Method/Resource/Stage but does not carry these failing checks' replay keys (the Resource check now fails on a different assertion and the RestApi/stage-replacement check is not listed there at all); the open issue 20261110T214424Z-apigateway-cloudformation-an-in-place-update-of-aws.md records the same refusal for AWS::ApiGateway::RestApi.

Cause 1 of 3: a stack update that changes only a MUTABLE property of an API Gateway resource is refused instead of applied.
AWS: recorded behavior only (CloudFormation resource handlers are not in the Smithy model). Changing AWS::ApiGateway::Deployment.Description, an AWS::ApiGateway::Method integration/mock setting, or AWS::ApiGateway::Stage.Variables is an in-place update: the logical resource goes UPDATE_IN_PROGRESS -> UPDATE_COMPLETE with its physical id unchanged and the stack reaches UPDATE_COMPLETE; API Gateway applies it through PatchOperations on UpdateDeployment / UpdateMethod / UpdateStage.
Emulator: the resource is UPDATE_FAILED and the stack rolls back to UPDATE_ROLLBACK_COMPLETE with StackStatusReason 'The following resource(s) failed to update: [<LogicalId>]. '. The resource event reason is emulator-authored prose: fixed text 'NotImplemented: this emulator does not change the properties of an existing ' + '<resource type>' + ' in place; the resource was left as it is.' — the only variable part is the resource type name. AWS never answers 'NotImplemented' for a supported property change, and no AWS error code has that shape.
Why: no client (console, `aws cloudformation deploy`, CDK, SAM, Serverless) can change a deployment description, a method integration or stage variables through a template at all — the stack rolls back where AWS returns UPDATE_COMPLETE.
Fix: the update path of the AWS::ApiGateway::Deployment / Method / Stage resource handlers in the CloudFormation provisioner — classify a mutable-property change as an in-place update and call the corresponding API Gateway update operation instead of returning the stub reason.
Failing checks: apigateway__cloudformation.cfn-apigateway-deployment-in-place-description-retained-api-move-then-not-found, apigateway__cloudformation.cfn-apigateway-method-mock-in-place-update-retained-verb-replacement-then-not-found, apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found.

Cause 2 of 3: changing AWS::ApiGateway::Resource.PathPart does not replace the resource.
AWS: recorded behavior only. Every property of AWS::ApiGateway::Resource is create-only, so a PathPart change is a REPLACEMENT: CloudFormation creates a new API Gateway resource under the same parent, the logical resource's PhysicalResourceId becomes the NEW resource id, and with UpdateReplacePolicy Retain the old resource id survives in the API. The RestApi is untouched, so its physical id is unchanged.
Emulator: after the update the stack still reports the SAME physical resource id for the Resource (the check reads {'Api': <api id>, 'Res': <unchanged resource id>, ...}), so the create-only change was absorbed in place and no replacement happened. The AWS::ApiGateway::Method physical id in the same stack is also a stack-derived logical name ('<stack>-ResMethod-<suffix>') rather than an API Gateway identifier, which is the shape AWS uses for the Method resource's physical id.
Why: a client that renames a path expects a new resource (and, under Retain, the old one still reachable); here the template and the live API silently disagree and any Ref/GetAtt consumer keeps pointing at the old resource.
Fix: the AWS::ApiGateway::Resource handler's update classification in the CloudFormation provisioner — mark PathPart (and every other property) create-only so a change forces replacement and the new physical id is recorded.
Failing check: apigateway__cloudformation.cfn-apigateway-resource-retained-path-replacement-then-not-found.

Cause 3 of 3: DeleteStack leaves AWS::ApiGateway::Deployment in DELETE_FAILED because a stage still points at it.
AWS: recorded behavior only. DeleteStack removes the stack's API Gateway resources in dependency order — stages (and the RestApi itself, which drops every stage) before the deployments they reference — so a stack holding a RestApi + Deployment + Stage, including a stage retained by UpdateReplacePolicy, reaches DELETE_COMPLETE.
Emulator: the Deployment delete is attempted while a stage still references it and the handler refuses, so the stack ends DELETE_FAILED with StackStatusReason 'The following resource(s) failed to delete: [Deployment]. ' and the resource event reason 'Resource handler returned message: "Active stages pointing to this deployment must be moved or deleted" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'. The fixed text is API Gateway's BadRequestException wording 'Active stages pointing to this deployment must be moved or deleted'; the request token is a per-run value.
Why: the stack cannot be deleted at all — the account is left with an undeletable stack and leaked API Gateway resources, and every subsequent 'is it gone?' assertion is unreachable.
Fix: the delete ordering/dependency resolution for AWS::ApiGateway::* in the CloudFormation provisioner (delete stages, including retained ones inside the stack's API, before the deployments they reference).
Failing check: apigateway__cloudformation.cfn-apigateway-restapi-mock-in-place-update-retained-stage-replacement-then-not-found.

## Evidence
Cause 1 — expected stack status UPDATE_COMPLETE with the resource UPDATE_IN_PROGRESS -> UPDATE_COMPLETE.
Actual (cfn-apigateway-deployment-in-place-description-retained-api-move-then-not-found): AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [Deployment]. '), expected 'UPDATE_COMPLETE'; failed events: [('Deployment', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::ApiGateway::Deployment in place; the resource was left as it is.')]
Same reason with [Method] ('...existing AWS::ApiGateway::Method in place...') and [Stage] ('...existing AWS::ApiGateway::Stage in place...').

Cause 2 — expected: after a PathPart change the Api physical id is unchanged AND the Resource physical id is new.
Actual (cfn-apigateway-resource-retained-path-replacement-then-not-found): AssertionError: only the resource is replaced: {'Api': 'j43xbguwzu', 'Res': '10wir4', 'ResMethod': 'sim2real-cfnres-stack-830ac61edc13-ResMethod-EE01E4118909'} — the Resource id is identical to the pre-update one, so no replacement occurred.

Cause 3 — expected stack status DELETE_COMPLETE.
Actual (cfn-apigateway-restapi-mock-in-place-update-retained-stage-replacement-then-not-found): AssertionError: stack ended 'DELETE_FAILED' ('The following resource(s) failed to delete: [Deployment]. '), expected 'DELETE_COMPLETE'; failed events: [('Deployment', 'DELETE_FAILED', 'Resource handler returned message: "Active stages pointing to this deployment must be moved or deleted" (RequestToken: 8c552678-4d7a-1a8a-060f-32d4351a97d6, HandlerErrorCode: InvalidRequest)'), ('<stack name>', 'DELETE_FAILED', 'The following resource(s) failed to delete: [Deployment]. ')]

## Steps to Reproduce
1. Replay the behavioral suite and re-run the five checks listed in trace_ids.
2. Cause 1: CreateStack a template with AWS::ApiGateway::RestApi + Deployment + Method + Stage; wait for CREATE_COMPLETE; UpdateStack changing only Deployment.Description (then only the Method's Integration, then only Stage.Variables); DescribeStacks / DescribeStackEvents.
3. Cause 2: CreateStack a RestApi + Resource(PathPart='first') + Method; note the Resource's PhysicalResourceId; UpdateStack with PathPart='second' and UpdateReplacePolicy Retain on the Resource; read DescribeStackResources again — the Resource id must differ.
4. Cause 3: after a stack holding RestApi + Deployment + Stage has had its Stage replaced under UpdateReplacePolicy Retain, DeleteStack and poll DescribeStacks to a terminal delete status.
