Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow
Issue-Key: appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow|_deploy_cfn#5|AssertionError|raise AssertionError(
Issue-Commit: d68524ddfe39deacc36a1df634022e0b97d0c530

# A CDK stack that declares AppSync's own request log group rolls back: AWS::Logs::LogGroup for /aws/appsync/apis/<apiId> is refused AlreadyExists

> _Note: this issue was identified against the code base at commit `d68524ddfe39deacc36a1df634022e0b97d0c530`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLogGroup

## Description
Cause 1 of 1: a CloudFormation/CDK stack that creates an AppSync GraphQL API with logConfig AND declares the API's own request log group (AWS::Logs::LogGroup, logGroupName = /aws/appsync/apis/<apiId>, resolved from the API's attribute) cannot be created: the log-group resource is refused as already existing and the whole stack reaches ROLLBACK_COMPLETE.

AWS: recorded behavior only (the Smithy models state no cross-service coupling here). On AWS, CreateGraphQLApi with a logConfig does NOT eagerly create /aws/appsync/apis/<apiId> in CloudWatch Logs — the group appears only when AppSync first writes request entries, or when the customer/CloudFormation creates it. Declaring that group in the same template is the documented pattern (a metric filter cannot attach to a group that does not exist yet), so the AWS::Logs::LogGroup resource is created successfully and the stack reaches CREATE_COMPLETE with outputs ApiId, GraphQLUrl, UserPoolId, UserPoolClientId, TableName, LogGroupName, AlarmName, MetricNamespace. Logs CreateLogGroup only answers 400 ResourceAlreadyExistsException ('The specified log group already exists') when the named group genuinely exists already.

Emulator: the stack ends in ROLLBACK_COMPLETE with the resource-level failure 'ApiLogs<logical-id-suffix>=Resource handler returned message: "The specified log group already exists" (RequestToken: <uuid>, HandlerErrorCode: AlreadyExists)'. Fixed text of that message: 'The specified log group already exists'; HandlerErrorCode AlreadyExists; the logical id, request token and stack name are per-run values. Hypothesis: creating the GraphQL API (or its logConfig) pre-creates the /aws/appsync/apis/<apiId> group in the Logs store, so the stack's own LogGroup resource collides with a group the customer never asked any service to create; the alternative is that the CFN AWS::Logs::LogGroup handler issues CreateLogGroup twice for one resource.

Why: any template that owns its AppSync request log group — the only way to attach a metric filter at deploy time and the only way a teardown can assert the group is gone — cannot be deployed at all. Every downstream leg (authenticated mutation, authz refusal, structured log, metric filter datapoint, alarm notification, resolver-denial variant, in-place update, teardown) is unreachable, and the client sees a rolled-back stack for a template AWS accepts.

Fix: the AppSync CreateGraphQLApi handler / its log-config state must not materialize the CloudWatch Logs group for the API (keep it lazy, created on first log write), and the CloudFormation AWS::Logs::LogGroup resource handler must create the declared group exactly once and only fail AlreadyExists when the group pre-exists independently of this stack.

Traces: check appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow (leg 'deploy').

## Evidence
Expected: CreateStack/CDK deploy of the graphql_app_cdk stack reaches CREATE_COMPLETE; the declared log group /aws/appsync/apis/<apiId> exists and is owned by the stack; stack outputs include LogGroupName.
Actual: AssertionError: stack sim2real-xsvc-stk-<id> ended in ROLLBACK_COMPLETE: ApiLogs3D05D88B=Resource handler returned message: "The specified log group already exists" (RequestToken: <uuid>, HandlerErrorCode: AlreadyExists)

## Steps to Reproduce
1. Deploy a stack that creates an AWS::AppSync::GraphQLApi with LogConfig (FieldLogLevel ALL) plus an AWS::Logs::LogGroup whose LogGroupName is /aws/appsync/apis/<the API's ApiId attribute> and a metric filter over that group.
2. Observe the AWS::Logs::LogGroup resource fail with HandlerErrorCode AlreadyExists and the stack roll back.
3. Compare with AWS, where the group does not exist until the stack creates it and the deploy succeeds.
Replay: check appsync__cloudwatch__cognito-idp__dynamodb__logs.cdk-authenticated-graphql-app-workflow
