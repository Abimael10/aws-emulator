Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__lambda.sam-api-event-auth-none-overrides-default-iam
Issue-Key: apigateway__cloudformation__lambda.sam-api-event-auth-none-overrides-default-iam|run#17|AssertionError|assert opened["authorizationType"] == "NONE", (
Issue-Commit: b19562f8ca9de9711e1cde2d426cb1fc8bc6040d

# SAM transform: an Api event setting Auth.Authorizer NONE does not override the API's Auth.DefaultAuthorizer — the method deploys as AWS_IAM

> _Note: this issue was identified against the code base at commit `b19562f8ca9de9711e1cde2d426cb1fc8bc6040d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: in the SAM transform's Api-event Auth resolution, an event that explicitly sets `Auth: {Authorizer: NONE}` is given the API's `Auth.DefaultAuthorizer` anyway, so the generated AWS::ApiGateway::Method is authorizationType AWS_IAM instead of NONE.

AWS: in AWS::Serverless::Api, `Auth.DefaultAuthorizer` names the authorizer applied to every method of the API that does not override it, and an individual Api event opts out by setting `Auth.Authorizer: NONE`. The override wins, so apigateway GetMethod on that method reports `authorizationType: "NONE"` while the sibling method that declares no Auth of its own reports the inherited value (here `"AWS_IAM"`). (Smithy: apigateway GetMethod output member `authorizationType`; the SAM `Auth.DefaultAuthorizer` inheritance/override rule is documented SAM behavior and is not modelled — recorded behavior only for the rule itself.)

Emulator: after the stack reaches CREATE_COMPLETE, GetMethod on the method whose event sets `Auth.Authorizer: NONE` reports `authorizationType` `'AWS_IAM'` — the default authorizer was applied to the method that opts out of it. The failing assertion is verbatim: "a method whose event sets Auth.Authorizer NONE must override the inherited default; GetMethod reported 'AWS_IAM'".

Note: this is NOT the cause carried by open issue 20261117T134237Z-apigateway-cloudformation-lambda-2-causes-in-the-sam-transform-s-api.md, whose Cause 1 is the opposite leg (the inheriting method reported NONE) and whose replay key is the other assertion in the same check. Inheritance is now applied, but it is applied unconditionally: the per-event override is ignored. That issue does not own this check, so this cause must be closed separately.

Why: a template that declares one public route under an otherwise IAM-secured API deploys with that route requiring SigV4. Every unauthenticated client of that route is refused 403 in the emulator where AWS serves it, so an authorization model that is correct on AWS cannot be exercised here — and the inverse (a per-method opt-out silently not taking effect) is exactly the class of mistake the check exists to catch.

Fix: the SAM transform's Api-event Auth resolution for AWS::Serverless::Function — the merge step that combines the API's `Auth.DefaultAuthorizer` with the event's own `Auth.Authorizer` before emitting the AWS::ApiGateway::Method `AuthorizationType`. The event-level value (including the sentinel `NONE`) must take precedence over the API default; only an event with no `Auth` key inherits.

Traces: apigateway__cloudformation__lambda.sam-api-event-auth-none-overrides-default-iam.

## Evidence
Expected: apigateway GetMethod `authorizationType` == 'NONE' for the method whose Api event sets Auth.Authorizer NONE (its sibling, which declares no Auth, inherits 'AWS_IAM').
Actual, verbatim from the replay:
  File "check_apigateway__cloudformation__lambda_sam_api_event_auth_none_overrides_default_iam.py", line 114, in run
  assert opened["authorizationType"] == "NONE", (
  AssertionError: a method whose event sets Auth.Authorizer NONE must override the inherited default; GetMethod reported 'AWS_IAM'
The fixed text of the diverging value is the authorizationType enum itself; 'AWS_IAM'/'NONE' are the two values at issue, and the method/resource ids are per-run values from the deployed stack.

## Steps to Reproduce
1. CreateStack with `Transform: AWS::Serverless-2016-10-31`, an AWS::Serverless::Api carrying `Properties.Auth: {DefaultAuthorizer: AWS_IAM}` and `StageName: test` (no DefinitionBody), and one AWS::Serverless::Function with two Api events on that API: one declaring no `Auth`, one setting `Auth: {Authorizer: NONE}`.
2. Wait for CREATE_COMPLETE.
3. apigateway GetRestApis/GetResources to find both methods, then GetMethod on each.
4. Observed: both report authorizationType 'AWS_IAM'. Expected: 'AWS_IAM' for the event with no Auth, 'NONE' for the event that sets Auth.Authorizer NONE.
