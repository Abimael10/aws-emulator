Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow
Issue-Key: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow|_deploy_cfn#5|AssertionError|raise AssertionError(
Issue-Commit: 4cf7211f237a5c930178a3131e9243c3c3eb5dd6

# appsync__cloudwatch__cognito-idp__dynamodb__logs: a CloudFormation AWS::Logs::MetricFilter is refused NotFound "The specified log group does not exist." against a log group created by the same stack, rolling the whole stack back

> _Note: this issue was identified against the code base at commit `4cf7211f237a5c930178a3131e9243c3c3eb5dd6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricFilter

## Description
One net-new root cause.

Cause 1 of 1: when a stack declares an AWS::Logs::LogGroup and an AWS::Logs::MetricFilter whose LogGroupName is a Ref to that log group, the metric-filter resource fails with HandlerErrorCode NotFound and the handler message "The specified log group does not exist.", so the stack never reaches CREATE_COMPLETE and ends ROLLBACK_COMPLETE. Failing check: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow.
AWS: Smithy (services/service-com-amazonaws-cloudwatchlogs-logs-20140328): PutMetricFilter takes logGroupName (required, @length 1..512, @pattern ^[\.\-_/#A-Za-z0-9]+$), filterName, filterPattern and metricTransformations, returns an EMPTY output (no body) on success, and models ResourceNotFoundException (client, HTTP 400) for a log group that does not exist — a name beginning with '/aws/...' is ordinary and matches the pattern. CloudFormation orders a resource after the one it Refs, so the log group is created (CreateLogGroup, then DescribeLogGroups lists it) before PutMetricFilter is called; the call therefore succeeds and the stack reaches CREATE_COMPLETE with every declared output resolvable. CloudFormation wraps a handler refusal as 'Resource handler returned message: "<the service error message>" (RequestToken: <uuid>, HandlerErrorCode: <code>)' — the fixed text is the wrapper and the service's own message; the request token and the logical id are per-run values.
Emulator: the metric-filter resource is refused: event ResourceStatusReason 'Resource handler returned message: "The specified log group does not exist." (RequestToken: <uuid>, HandlerErrorCode: NotFound)' on the logical id of the MetricFilter, and the stack ends ROLLBACK_COMPLETE instead of CREATE_COMPLETE. The log group named in that same template (an AWS::Logs::LogGroup with an explicit LogGroupName of the form /aws/appsync/apis/<apiId>, resolved from a Ref/GetAtt on the API in the same stack, plus RetentionInDays) is declared immediately before it, so the group either is not created by its own resource handler or is not visible under the name the metric-filter handler looks up. Hypothesis: the AWS::Logs::LogGroup resource handler does not register the group under its LogGroupName (e.g. it returns a different physical id, or the Ref the MetricFilter resolves is not that name), or the two resources are not ordered by the Ref, so PutMetricFilter runs against a name the logs state does not hold.
Why: any stack (including every CDK app using logs.MetricFilter over a logs.LogGroup, the canonical pattern) that pairs a log group with a metric filter, subscription filter or alarm over its metric cannot be deployed at all — the stack rolls back, so no resource in it exists and every downstream leg (the AppSync API, its Cognito pool, its DynamoDB table, its metric and alarm) is unreachable. A client sees a NotFound for a resource its own template creates.
Fix: the CloudFormation resource handlers for AWS::Logs::LogGroup and AWS::Logs::MetricFilter and their dependency resolution — the log group's create must register it under the template's LogGroupName so DescribeLogGroups/PutMetricFilter find it, the MetricFilter's Ref to it must resolve to that name, and the metric filter must be created only after the group it references; PutMetricFilter must then return its empty success response rather than ResourceNotFoundException.
Traces: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow (the stack is rolled back before any GraphQL, metric or alarm assertion is reached).

## Evidence
Verbatim from the replay:
  appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow
  AssertionError: stack <stackName> ended in ROLLBACK_COMPLETE: <MetricFilterLogicalId>=Resource handler returned message: "The specified log group does not exist." (RequestToken: <uuid>, HandlerErrorCode: NotFound)
  (recorded instance: stack sim2real-xsvc-stk-9a1b6573af38, logical id RequestFilterCFC61D7D, RequestToken 9417da8d-7139-62c2-22eb-d62f39cd3c61 — the stack name, logical id and request token are per-run values; the requirement is the status and the absence of this refusal.)
The template declares, in this order: an AWS::Logs::LogGroup with LogGroupName '/aws/appsync/apis/<apiId>' (apiId from GetAtt on the AWS::AppSync::GraphQLApi in the same stack) and RetentionInDays 1, then an AWS::Logs::MetricFilter over it (FilterPattern a literal term match, MetricTransformations -> one metric named GraphQLRequests in the stack's namespace).
Expected (AWS): PutMetricFilter against the just-created group succeeds with an empty response; the stack reaches CREATE_COMPLETE and resolves its ApiId, GraphQLUrl, UserPoolId, UserPoolClientId, TableName, LogGroupName, MetricNamespace and AlarmName outputs.
Actual (emulator): the MetricFilter resource is CREATE_FAILED with HandlerErrorCode NotFound and the message above, and the stack status is ROLLBACK_COMPLETE.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite (service heading: appsync__cloudwatch__cognito-idp__dynamodb__logs).
2. CreateStack with a template that contains an AWS::Logs::LogGroup (explicit LogGroupName, e.g. '/aws/appsync/apis/<apiId>' built from a GetAtt on an AWS::AppSync::GraphQLApi in the same stack, RetentionInDays 1) and an AWS::Logs::MetricFilter whose LogGroupName is a Ref to that log group, with FilterPattern '"<apiId>"' and one MetricTransformation (MetricName GraphQLRequests, MetricValue "1", a namespace of your choice).
3. Poll DescribeStacks. Observed: ROLLBACK_COMPLETE, with a DescribeStackEvents entry for the metric-filter logical id whose ResourceStatusReason is 'Resource handler returned message: "The specified log group does not exist." (RequestToken: <uuid>, HandlerErrorCode: NotFound)'. Expected: CREATE_COMPLETE.
4. Cross-check directly: CreateLogGroup(logGroupName='/aws/appsync/apis/x') then PutMetricFilter(logGroupName='/aws/appsync/apis/x', filterName='f', filterPattern='"x"', metricTransformations=[...]) and DescribeLogGroups(logGroupNamePrefix='/aws/appsync/apis/x') to see whether the group the stack's own resource created is registered under that name.
