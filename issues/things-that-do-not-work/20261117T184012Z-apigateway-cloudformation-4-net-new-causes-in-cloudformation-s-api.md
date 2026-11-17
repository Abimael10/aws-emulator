Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected apigateway__cloudformation.deploy-cfn-apigateway-restapi-invalid-apikeysourcetype-rejected apigateway__cloudformation.sam-definitionbody-default-authorizer-covers-uncovered-method apigateway__cloudformation.sam-serverless-api-request-authorizer-missing-identity-rejected
Issue-Key: apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected|run#21|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", (
Issue-Key: apigateway__cloudformation.deploy-cfn-apigateway-restapi-invalid-apikeysourcetype-rejected|run#14|AssertionError|assert _masked(reason) == expected_reason, (
Issue-Key: apigateway__cloudformation.sam-definitionbody-default-authorizer-covers-uncovered-method|run#15|AssertionError|assert method["authorizationType"] == "AWS_IAM", (
Issue-Key: apigateway__cloudformation.sam-serverless-api-request-authorizer-missing-identity-rejected|run#10|AssertionError|assert described["Status"] == "FAILED", (
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# 4 net-new causes in CloudFormation's API Gateway resource handlers: a Stage/Deployment stage collision cannot roll back, the RestApi CREATE_FAILED reason is mis-worded, and the SAM API authorizer rules are not applied

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Four distinct defects in the CloudFormation handling of API Gateway resources, all reached through CreateStack/CreateChangeSet. Each cause has its own failing check ids.

Cause 1 of 4: a stack whose AWS::ApiGateway::Stage declares the StageName its sibling AWS::ApiGateway::Deployment already created cannot roll back.
AWS: recorded behavior only (the collision is an API Gateway ConflictException surfaced by the Stage handler). The create fails and CloudFormation then deletes the resources it created, so the stack settles at ROLLBACK_COMPLETE and nothing is left behind.
Emulator: the stack settles at ROLLBACK_FAILED — at least one resource created before the failure could not be deleted during rollback.
Why: a client that waits on stack_rollback_complete never returns, and the stack is left in a state that only a second, forced delete can clear; the leaked API Gateway resources keep the name reserved.
Fix: the rollback path of the AWS::ApiGateway::Stage / ::Deployment resource handlers (their Delete must succeed for a resource the same stack just created).
Traces: apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected

Cause 2 of 4: the CREATE_FAILED ResourceStatusReason for an AWS::ApiGateway::RestApi with an off-enum ApiKeySourceType diverges from AWS in three ways.
AWS: the reason is `Resource handler returned message: "1 validation error detected: Value '<value>' at 'createRestApiInput.apiKeySource' failed to satisfy constraint: Member must satisfy enum value set: [AUTHORIZER, HEADER] (Service: ApiGateway, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: GeneralServiceException)`. Fixed text: everything outside the angle brackets, including the enum member list rendered in the order `[AUTHORIZER, HEADER]`, the ` (SDK Attempt Count: <n>)` suffix and the handler error code `GeneralServiceException`. Variable parts: the offending value (from the request), the two uuids and the attempt count (per run).
Emulator: it renders the enum list as `[HEADER, AUTHORIZER]`, omits the ` (SDK Attempt Count: <n>)` suffix entirely, and reports `HandlerErrorCode: InvalidRequest`.
Why: tooling that classifies stack failures by HandlerErrorCode sees a client-input error where AWS reports a service exception, and reason-matching assertions on the enum wording fail.
Fix: the AWS::ApiGateway::RestApi resource handler's failure-reason serializer (enum member ordering, SDK wrapper suffix, handler error code mapping).
Traces: apigateway__cloudformation.deploy-cfn-apigateway-restapi-invalid-apikeysourcetype-rejected

Cause 3 of 4: the SAM transform does not apply Auth.DefaultAuthorizer to a DefinitionBody method that names no authorizer.
AWS: recorded behavior only. A SAM AWS::Serverless::Api whose Auth.DefaultAuthorizer is AWS_IAM makes every method the DefinitionBody leaves uncovered inherit it; GetMethod then reports authorizationType AWS_IAM for that method.
Emulator: GetMethod reports authorizationType NONE for the uncovered method — the default authorizer is never propagated.
Why: a SAM API that relies on a default authorizer is deployed wide open; every uncovered route accepts unauthenticated calls.
Fix: the SAM transform's AWS::Serverless::Api Auth expansion (default-authorizer propagation into the generated RestApi methods).
Traces: apigateway__cloudformation.sam-definitionbody-default-authorizer-covers-uncovered-method

Cause 4 of 4: a SAM REQUEST authorizer with no Identity source is accepted.
AWS: recorded behavior only. A SAM AWS::Serverless::Api REQUEST authorizer that declares no Identity source is rejected during the transform, so the change set settles at FAILED.
Emulator: the change set settles at CREATE_COMPLETE and the invalid authorizer is deployed.
Why: a template AWS refuses deploys here, so an authorizer that can never evaluate an identity is created and the divergence only shows up at request time.
Fix: the SAM transform's AWS::Serverless::Api authorizer validation.
Traces: apigateway__cloudformation.sam-serverless-api-request-authorizer-missing-identity-rejected

## Evidence
Cause 1: expected ROLLBACK_COMPLETE, got 'ROLLBACK_FAILED' (AssertionError: "stack whose Stage names its sibling Deployment's own stage settled at 'ROLLBACK_FAILED', expected ROLLBACK_COMPLETE").

Cause 2: expected (ids masked) `Resource handler returned message: "1 validation error detected: Value 'BOGUS' at 'createRestApiInput.apiKeySource' failed to satisfy constraint: Member must satisfy enum value set: [AUTHORIZER, HEADER] (Service: ApiGateway, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <id>, HandlerErrorCode: GeneralServiceException)`; got `Resource handler returned message: "1 validation error detected: Value 'BOGUS' at 'createRestApiInput.apiKeySource' failed to satisfy constraint: Member must satisfy enum value set: [HEADER, AUTHORIZER] (Service: ApiGateway, Status Code: 400, Request ID: <id>)" (RequestToken: <id>, HandlerErrorCode: InvalidRequest)`.

Cause 3: AssertionError: "a method the DefinitionBody names no authorizer for must inherit Auth.DefaultAuthorizer; GetMethod reported 'NONE'" (expected 'AWS_IAM').

Cause 4: AssertionError: "a REQUEST authorizer naming no Identity source must fail the change set; got 'CREATE_COMPLETE'" (expected DescribeChangeSet Status FAILED).

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. apigateway__cloudformation.deploy-apigateway-stage-name-collides-with-deployment-stage-rejected
2. apigateway__cloudformation.deploy-cfn-apigateway-restapi-invalid-apikeysourcetype-rejected
3. apigateway__cloudformation.sam-definitionbody-default-authorizer-covers-uncovered-method
4. apigateway__cloudformation.sam-serverless-api-request-authorizer-missing-identity-rejected
