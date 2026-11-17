Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__lambda.sam-api-event-auth-none-overrides-default-iam apigateway__cloudformation__lambda.sam-api-event-unknown-authorizer-name-rejected
Issue-Key: apigateway__cloudformation__lambda.sam-api-event-auth-none-overrides-default-iam|run#15|AssertionError|assert secured["authorizationType"] == "AWS_IAM", (
Issue-Key: apigateway__cloudformation__lambda.sam-api-event-unknown-authorizer-name-rejected|run#9|AssertionError|assert status == "ROLLBACK_COMPLETE", (
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 2 causes in the SAM transform's Api event Auth handling: DefaultAuthorizer is not inherited, and an event naming an undeclared authorizer is accepted

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two distinct root causes in how the SAM transform turns an AWS::Serverless::Function `Api` event into an API Gateway method's authorization.

Cause 1 of 2: an Api event that declares no `Auth` of its own does not inherit the API's `Auth.DefaultAuthorizer` — the generated method is authorizationType NONE.
AWS: in AWS::Serverless::Api, `Auth.DefaultAuthorizer` names the authorizer applied to every method of the API that does not override it; `AWS_IAM` is one of its legal values. A function's `Api` event with no `Auth` key therefore produces a method whose apigateway GetMethod reports `authorizationType: "AWS_IAM"`. Only an event that sets `Auth.Authorizer: NONE` opts out. (Smithy: apigateway GetMethod output member `authorizationType`; the SAM `Auth.DefaultAuthorizer` inheritance rule is documented SAM behavior, not modelled — so: recorded behavior only for the inheritance rule.)
Emulator: GetMethod on the non-overriding method reports `authorizationType` `'NONE'`, i.e. the default authorizer was never applied, so the method is open.
Why: this is a fail-open security divergence. A template that secures an API once, at `Auth.DefaultAuthorizer`, and relies on inheritance for every route deploys with every one of those routes unauthenticated — a client testing its authorization model sees it pass when the real deployment would refuse, and vice versa.
Fix: the SAM transform's Api-event Auth resolution for AWS::Serverless::Function — the step that merges the API's `Auth.DefaultAuthorizer` into each event before emitting the AWS::ApiGateway::Method `AuthorizationType`.
Traces: apigateway__cloudformation__lambda.sam-api-event-auth-none-overrides-default-iam

Cause 2 of 2: an Api event whose `Auth.Authorizer` names an authorizer the API never declares is accepted — the stack reaches CREATE_COMPLETE instead of failing the transform and rolling back.
AWS: the SAM transform validates that an event's `Auth.Authorizer` is either `NONE`, `AWS_IAM`, or the logical name of an authorizer declared under the API's `Auth.Authorizers`. An undeclared name is a transform error, so CreateStack's stack ends `ROLLBACK_COMPLETE` and no method is created. (Smithy: cloudformation DescribeStacks output member `Stacks[].StackStatus`, enum StackStatus value ROLLBACK_COMPLETE.)
Emulator: the stack status is `'CREATE_COMPLETE'`.
Why: a typo in an authorizer name is silently accepted, so the method deploys with whatever authorization the emulator defaults to rather than being rejected at deploy time. A client gets a green deploy for a template real CloudFormation refuses, and only discovers the gap when the route answers the wrong way.
Fix: the SAM transform's Api-event Auth validation — the check that every referenced authorizer name exists in the API's `Auth.Authorizers` — and the transform-failure path that must roll the stack back.
Traces: apigateway__cloudformation__lambda.sam-api-event-unknown-authorizer-name-rejected

## Evidence
Cause 1 — expected apigateway GetMethod `authorizationType` 'AWS_IAM' on the method whose Api event declares no Auth. Actual, verbatim:
  AssertionError: a method whose event names no Auth of its own must inherit Auth.DefaultAuthorizer; GetMethod reported 'NONE'
Cause 2 — expected DescribeStacks StackStatus 'ROLLBACK_COMPLETE' for an event naming an undeclared authorizer. Actual, verbatim:
  AssertionError: an event naming an undeclared authorizer must fail the transform and roll back; got 'CREATE_COMPLETE'

## Steps to Reproduce
Cause 1: CreateStack with Transform AWS::Serverless-2016-10-31, an AWS::Serverless::Api carrying `Auth: {DefaultAuthorizer: AWS_IAM}`, and an AWS::Serverless::Function with two Api events — one setting `Auth: {Authorizer: NONE}`, one setting no Auth at all. After CREATE_COMPLETE, call apigateway GetMethod on the non-overriding method. Observed authorizationType 'NONE'; expected 'AWS_IAM'.
Cause 2: CreateStack with the same shape but an Api event whose `Auth: {Authorizer: <name not declared in Auth.Authorizers>}`. Wait for a terminal status. Observed 'CREATE_COMPLETE'; expected 'ROLLBACK_COMPLETE'.
