Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudformation.cfn-appsync-data-source-in-place-update-retained-replacement-then-not-found appsync__cloudformation.cfn-appsync-graphql-api-in-place-update-serves-query-then-not-found appsync__cloudformation.cfn-appsync-api-key-in-place-update-retained-replacement-then-not-found appsync__cloudformation.cfn-appsync-graphql-schema-in-place-update-serves-new-fields-then-not-found appsync__cloudformation.cfn-appsync-resolver-in-place-update-retained-replacement-then-not-found
Issue-Key: appsync__cloudformation.cfn-appsync-data-source-in-place-update-retained-replacement-then-not-found|_expect#2|AssertionError|raise AssertionError(
Issue-Key: appsync__cloudformation.cfn-appsync-graphql-api-in-place-update-serves-query-then-not-found|_expect#2|AssertionError|raise AssertionError(
Issue-Key: appsync__cloudformation.cfn-appsync-api-key-in-place-update-retained-replacement-then-not-found|run#2|AssertionError|raise _redacted_error(exc, clean) from None
Issue-Key: appsync__cloudformation.cfn-appsync-graphql-schema-in-place-update-serves-new-fields-then-not-found|run#2|AssertionError|assert _physical_ids(cfn, ref["stack"])["Schema"] == schema_id, (
Issue-Key: appsync__cloudformation.cfn-appsync-resolver-in-place-update-retained-replacement-then-not-found|_assert_resolver#2|AssertionError|assert result.get("error") is None, f"EvaluateCode of Query.{field}: {result.get('error')!r}"
Issue-Commit: 91290edf36da0a835077a90e85db4b27bf00fabc

# appsync__cloudformation: 4 net-new causes — in-place property updates of AWS::AppSync::{DataSource,GraphQLSchema,ApiKey} answer NotImplemented and roll the stack back, ApiKey Ref is the key value instead of the key ARN, GraphQLSchema's physical id is not <apiId>GraphQLSchema, and EvaluateCode binds no ctx.args

> _Note: this issue was identified against the code base at commit `91290edf36da0a835077a90e85db4b27bf00fabc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Four distinct root causes found replaying the five AWS::AppSync::* CloudFormation in-place/replacement update checks. These checks now get past the Outputs defect described in open issue 20261111T103331Z-appsync-cloudformation-describestacks-returns-outputs-rows-with-outputkey.md and fail further along; none of them is marked ALREADY FILED or OWNED in the replay report.

Cause 1 of 4: an UpdateStack that changes a mutable property of an existing AWS::AppSync::DataSource, AWS::AppSync::GraphQLSchema or AWS::AppSync::ApiKey is refused as not implemented, and the stack rolls back.
AWS: recorded behavior only (CloudFormation resource reference for aws-resource-appsync-datasource / -graphqlschema / -apikey): none of the changed properties is create-only — a Description change on an ApiKey, a Definition change on a GraphQLSchema and a property change on a DataSource are all UPDATE_IN_PROGRESS -> UPDATE_COMPLETE in place, keeping the same PhysicalResourceId, and the stack reaches UPDATE_COMPLETE. AppSync's own UpdateDataSource/StartSchemaCreation/UpdateApiKey back those updates.
Emulator: each resource emits UPDATE_FAILED with the emulator-authored reason "NotImplemented: this emulator does not change the properties of an existing AWS::AppSync::<Type> in place; the resource was left as it is." and the stack ends UPDATE_ROLLBACK_COMPLETE with "The following resource(s) failed to update: [...]". AWS emits no such reason and no rollback.
Why: no AppSync stack can be evolved — the ordinary deploy loop (edit the schema/data source/key description, redeploy) fails and rolls back, so every subsequent assertion about the updated API is unreachable and a client's CI reads a healthy change as a failed deployment.
Fix: the AWS::AppSync::DataSource, AWS::AppSync::GraphQLSchema and AWS::AppSync::ApiKey resource providers in the CloudFormation emulator's resource registry — implement the update handler by calling the corresponding appsync control-plane operation instead of returning NotImplemented.
Traces: cfn-appsync-data-source-in-place-update-retained-replacement-then-not-found, cfn-appsync-graphql-api-in-place-update-serves-query-then-not-found.

Cause 2 of 4: Ref / PhysicalResourceId of AWS::AppSync::ApiKey is the API key value, not the key ARN.
AWS: recorded behavior only (aws-resource-appsync-apikey return values): Ref on AWS::AppSync::ApiKey returns the API key ARN, of the form arn:<partition>:appsync:<region>:<account-id>:apis/<apiId>/apikeys/<apiKeyId>, and that ARN is the resource's PhysicalResourceId; GetAtt ApiKeyId / ApiKey return the key id and value.
Emulator: the Ref output (and the stack resource's physical id) is the raw API key value — the assertion reports it as `ARN '<api-key>'` after redaction, where an ARN was required. The partition/region/account/apiId/apiKeyId in the expected ARN are all values from the request and the emulator's own state; only the shape (an appsync apis/<apiId>/apikeys/<keyId> ARN) is the requirement.
Why: a template that passes Ref Key to anything expecting an ARN (IAM policy resource, cross-stack export, another resource's property) gets a secret key value instead, and a stack that tracks replacement by physical id cannot tell the retained key from the new one. It also leaks the key into CloudFormation's resource listing.
Fix: the AWS::AppSync::ApiKey resource provider in the CloudFormation emulator's resource registry — return the key ARN as the physical id / Ref value.
Traces: cfn-appsync-api-key-in-place-update-retained-replacement-then-not-found.

Cause 3 of 4: Ref / PhysicalResourceId of AWS::AppSync::GraphQLSchema is not the documented composite id.
AWS: recorded behavior only (aws-resource-appsync-graphqlschema return values): the schema resource's physical id — and Ref — is the API id with the literal "GraphQLSchema" appended, i.e. `<apiId>GraphQLSchema` (fixed suffix; the apiId is the value from state), and it is stable across in-place Definition updates.
Emulator: DescribeStackResources reports a different physical id for the Schema resource (not the `<apiId>GraphQLSchema` composite), so the identity assertion fails immediately after CREATE_COMPLETE.
Why: a client that correlates a stack resource with its API through the documented composite id, or that asserts a Definition change was in place (same physical id), gets an opaque value it cannot join on.
Fix: the AWS::AppSync::GraphQLSchema resource provider in the CloudFormation emulator's resource registry — derive the physical id as <apiId>GraphQLSchema.
Traces: cfn-appsync-graphql-schema-in-place-update-serves-new-fields-then-not-found.

Cause 4 of 4: EvaluateCode does not bind the supplied context's `arguments` to `ctx.args`, so stored APPSYNC_JS resolver code fails in the sandbox.
AWS: Smithy — EvaluateCode(runtime, code, context, function) returns `evaluationResult` with `error` absent for code that runs. In APPSYNC_JS, `ctx.args` is an alias of `ctx.arguments`, so with context `{"arguments": {"msg": "<m>"}}` and function "response", the code `return "v1:" + ctx.args.msg;` evaluates to the JSON string "v1:<m>" and `error` is null.
Emulator: EvaluateCode answers with an `error` whose message is "cannot read property 'msg' of undefined" — `ctx.args` is undefined in the sandbox — and no evaluationResult value.
Why: the documented way to test a resolver's stored code offline reports a failure for code AWS runs, so a client cannot validate a deployed resolver, and any tooling that gates a deploy on EvaluateCode blocks on a phantom error.
Fix: the appsync EvaluateCode handler's context binding (populate ctx.args as an alias of ctx.arguments, with the rest of the documented ctx members).
Related open issues describing the same sandbox gap from other checks: 20261110T215409Z-appsync-10-net-new-causes-graphql-realtime-endpoints.md and 20261111T092526Z-appsync-evaluatecode-diverges-across-the-board-context-not.md — neither carries this check.
Traces: cfn-appsync-resolver-in-place-update-retained-replacement-then-not-found.

## Evidence
Cause 1 — cfn-appsync-data-source-in-place-update-retained-replacement-then-not-found:
  expected: stack UPDATE_COMPLETE, Source UPDATE_IN_PROGRESS -> UPDATE_COMPLETE, same PhysicalResourceId
  actual:   AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [Source]. '), expected 'UPDATE_COMPLETE'; failed events: [('Source', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::AppSync::DataSource in place; the resource was left as it is.')]
  cfn-appsync-graphql-api-in-place-update-serves-query-then-not-found:
  actual:   stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [Schema, Source, Key]. '); failed events: Schema/Source/Key each UPDATE_FAILED with 'NotImplemented: this emulator does not change the properties of an existing AWS::AppSync::{GraphQLSchema,DataSource,ApiKey} in place; the resource was left as it is.'

Cause 2 — cfn-appsync-api-key-in-place-update-retained-replacement-then-not-found:
  expected: Ref Key == arn:<partition>:appsync:<region>:<account>:apis/<apiId>/apikeys/<apiKeyId>
  actual:   AssertionError: ARN '<api-key>' must be arn:<partition>:appsync:<region>:<account>:apis/<apiId>/apikeys/<api-key>  (the Ref value is the API key value itself, redacted by the check)

Cause 3 — cfn-appsync-graphql-schema-in-place-update-serves-new-fields-then-not-found:
  expected: DescribeStackResources PhysicalResourceId for 'Schema' == '<apiId>GraphQLSchema'
  actual:   AssertionError: the schema's physical id is the API id with GraphQLSchema appended, '<apiId>GraphQLSchema'  (the reported physical id is some other value)

Cause 4 — cfn-appsync-resolver-in-place-update-retained-replacement-then-not-found:
  request: EvaluateCode runtime={APPSYNC_JS,1.0.0}, function='response', context='{"arguments": {"msg": "<m>"}}', code='export function response(ctx) { return "v1:" + ctx.args.msg; }'
  expected: error == null, evaluationResult == '"v1:<m>"'
  actual:   AssertionError: EvaluateCode of Query.first: {'message': "cannot read property 'msg' of undefined"}

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (checks listed under ### appsync__cloudformation).
2. Cause 1: create a stack with AWS::AppSync::GraphQLApi + GraphQLSchema + DataSource(NONE) + ApiKey, then UpdateStack changing the DataSource's properties (and/or the Schema Definition, and/or the ApiKey Description). The resources answer UPDATE_FAILED 'NotImplemented ...' and the stack ends UPDATE_ROLLBACK_COMPLETE.
3. Cause 2: declare Outputs {KeyArn: {Ref: Key}} for an AWS::AppSync::ApiKey and compare the output (and DescribeStackResources PhysicalResourceId) with arn:<partition>:appsync:<region>:<account>:apis/<apiId>/apikeys/<keyId>.
4. Cause 3: declare Outputs {SchemaId: {Ref: Schema}} for AWS::AppSync::GraphQLSchema and compare with '<apiId>GraphQLSchema'.
5. Cause 4: deploy an AWS::AppSync::Resolver with Runtime APPSYNC_JS and Code 'export function request(ctx){return {};} export function response(ctx){return "v1:" + ctx.args.msg;}', then call EvaluateCode with that code, function='response' and context='{"arguments":{"msg":"hello"}}'.
