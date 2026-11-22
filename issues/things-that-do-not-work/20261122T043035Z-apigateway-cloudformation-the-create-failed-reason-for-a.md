Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected
Issue-Key: apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected|run#26|AssertionError|assert reason.startswith(prefix) and reason.endswith(suffix), (
Issue-Commit: 622c214bd0a84106899ec4445dc3396f8892f16d

# apigateway__cloudformation: the CREATE_FAILED reason for a colliding AWS::ApiGateway::Stage forwards API Gateway's raw 409 instead of CloudFormation's AlreadyExists wording

> _Note: this issue was identified against the code base at commit `622c214bd0a84106899ec4445dc3396f8892f16d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStage

## Description
One net-new root cause in CloudFormation's AWS::ApiGateway::Stage resource handler.

Cause 1 of 1: a Stage whose StageName already exists (because a sibling AWS::ApiGateway::Deployment with its own StageName already minted it) is refused, but the ResourceStatusReason CloudFormation records is the API Gateway service exception passed straight through, not the registry handler's already-exists reason. Failing check: apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected

AWS:      Smithy (apigateway): CreateStage input restApiId/stageName/deploymentId all required; modeled error ConflictException with httpError 409. A custom-named resource whose identifier already exists is reported by CloudFormation's resource provider as an AlreadyExists failure, so the CREATE_FAILED event's ResourceStatusReason reads exactly:
          Resource handler returned message: "Resource of type 'AWS::ApiGateway::Stage' with identifier 'RestApiId: <rest-api-id> StageName: <stage-name>' already exists." (RequestToken: <cfn request token>, HandlerErrorCode: AlreadyExists)
          Fixed text: `Resource handler returned message: "Resource of type 'AWS::ApiGateway::Stage' with identifier '`, `' already exists." (RequestToken: `, `, HandlerErrorCode: AlreadyExists)` and the literal identifier labels `RestApiId: ` / ` StageName: `. Variable parts: the RestApi id and the stage name taken from the request, and CloudFormation's own request token.
Emulator: records `Resource handler returned message: "Stage already exists (Service: ApiGateway, Status Code: 409, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: ResourceConflict)` — the raw service message plus a Service/Status Code/Request ID suffix, and HandlerErrorCode ResourceConflict instead of AlreadyExists. Neither the resource type nor the colliding identifier appears.
Why:      a client that reads CREATE_FAILED reasons (CDK, the CLI's deploy output, any rollback triage) cannot tell WHICH resource identifier collided, because the emulator's message names neither the type nor the RestApiId/StageName pair. Tooling that branches on HandlerErrorCode to distinguish a name collision (retry with a new name) from a generic conflict (retry the same request) takes the wrong branch, since AlreadyExists never appears.
Fix:      the AWS::ApiGateway::Stage create path of CloudFormation's resource-provider handler — the already-exists precondition must be detected by the handler and reported as its own AlreadyExists failure with the identifier it checked, instead of surfacing the apigateway ConflictException verbatim.
Traces:   apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected (the stack reaches ROLLBACK_COMPLETE as expected; only the reason text and HandlerErrorCode diverge).

## Evidence
Check apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected [mismatch]

Template: RestApi -> root GET MOCK Method -> Deployment (StageName = S) -> Stage (StageName = S), so the Stage resource collides with the stage its sibling Deployment already created.

expected ResourceStatusReason (CREATE_FAILED on the Stage resource):
  Resource handler returned message: "Resource of type 'AWS::ApiGateway::Stage' with identifier 'RestApiId: <rest-api-id> StageName: <stage-name>' already exists." (RequestToken: <token>, HandlerErrorCode: AlreadyExists)

actual:
  Resource handler returned message: "Stage already exists (Service: ApiGateway, Status Code: 409, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: ResourceConflict)

AssertionError raised at check line 191: reason did not start with the expected prefix nor end with ', HandlerErrorCode: AlreadyExists)'. The rest-api id, stage name, request id and request token in any single run are per-run values that fill the template; they are not part of the requirement.

## Steps to Reproduce
1. CreateStack with a template containing: AWS::ApiGateway::RestApi; a root GET MOCK AWS::ApiGateway::Method; AWS::ApiGateway::Deployment with Properties.StageName = "dup"; AWS::ApiGateway::Stage with RestApiId/DeploymentId refs and StageName = "dup".
2. Poll DescribeStacks until the stack settles — it reaches ROLLBACK_COMPLETE (correct).
3. Read DescribeStackEvents and take the single CREATE_FAILED row for the Stage logical id.
4. Its ResourceStatusReason is the raw apigateway 409 with HandlerErrorCode ResourceConflict; AWS words it as the AlreadyExists resource-handler failure naming 'AWS::ApiGateway::Stage' and the 'RestApiId: <id> StageName: <name>' identifier.
