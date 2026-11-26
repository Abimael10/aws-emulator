Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__sns.sns-topic-handler-permission-role-deploys-then-denies-without-create-permission
Issue-Key: cloudformation__iam__sns.sns-topic-handler-permission-role-deploys-then-denies-without-create-permission|run#28|AssertionError|assert re.fullmatch(pattern, failed[0]), f"the topic failed with {failed[0]!r}"
Issue-Commit: c3c8deee388d115b7b5e547e320a19e176e4acbd

# CloudFormation AWS::SNS::Topic AccessDenied CREATE_FAILED reason omits the SDK diagnostic suffix and lower-cases the action's service prefix

> _Note: this issue was identified against the code base at commit `c3c8deee388d115b7b5e547e320a19e176e4acbd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Cause 1 of 1: when the stack's execution role is denied sns:CreateTopic, CloudFormation now does fail the AWS::SNS::Topic resource (the enforcement gap tracked by the open issue 20261109T163411Z-cloudformation-iam-sns-the-aws-sns-topic-resource-handler-performs-no.md is no longer what this check trips on), but the ResourceStatusReason it records on the CREATE_FAILED event is not rendered in AWS's form. Failing check: cloudformation__iam__sns.sns-topic-handler-permission-role-deploys-then-denies-without-create-permission.

AWS: the CREATE_FAILED event's ResourceStatusReason wraps the service exception the resource handler received, in the fixed template `Resource handler returned message: "<service exception message> (Service: <sdk service id>, Status Code: <http status>, Request ID: <request id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: AccessDenied)`. For this denial the inner exception message is AWS's standard no-identity-policy wording, `User: <principal arn> is not authorized to perform: <Action> on resource: <resource arn> because no identity-based policy allows the <Action> action`, and the action is rendered with the SDK's service-id casing — `SNS:CreateTopic`, not `sns:CreateTopic`. Smithy: sns#CreateTopic declares AuthorizationErrorException, errorKind client, httpError 403, awsQueryError code AuthorizationError / httpResponseCode 403 — which is what supplies `Status Code: 403` and `Service: Sns` in the suffix. The CFN event wrapper itself is recorded behavior only.

Fixed text of the requirement: `Resource handler returned message: "`, ` is not authorized to perform: `, ` on resource: `, ` because no identity-based policy allows the `, ` action`, ` (Service: `, `, Status Code: `, `, Request ID: `, `) (SDK Attempt Count: `, `)" (RequestToken: `, `, HandlerErrorCode: AccessDenied)`. Variable parts taken from the request and emulator state: the assumed-role principal ARN (account + role name + session name), the action name (service-id-cased `SNS:CreateTopic`), the target topic ARN built from region/account/TopicName, the service id (`Sns`), the status code (`403`), the per-call request id, the SDK attempt count, and the stack operation's request token. None of the concrete values in the recording below are part of the template.

Emulator: the reason omits the entire SDK diagnostic parenthetical and lower-cases the action's service prefix, producing `Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role>/AWSCloudFormation is not authorized to perform: sns:CreateTopic on resource: arn:aws:sns:<region>:<account>:<topic> because no identity-based policy allows the sns:CreateTopic action" (RequestToken: <uuid>, HandlerErrorCode: AccessDenied)` — i.e. `... allows the sns:CreateTopic action"` closes the quoted message with no `(Service: Sns, Status Code: 403, Request ID: <id>) (SDK Attempt Count: <n>)` before the closing quote, and both occurrences of the action read `sns:CreateTopic` instead of `SNS:CreateTopic`.

Why: clients parse ResourceStatusReason to classify deploy failures — scraping the service id, the HTTP status code and the request id out of the parenthetical to decide whether a failure is an authorization problem, which service refused, and which request to quote in a support case. Against the emulator those fields are absent, so such parsing yields nothing and the failure cannot be attributed; least-privilege test suites that assert on the denied action string also miss, because the action's casing differs from the one AWS emits.

Fix: the CloudFormation resource-provisioning path's rendering of a handler-returned service exception into the event's ResourceStatusReason — the component that formats the wrapper must append the `(Service: …, Status Code: …, Request ID: …) (SDK Attempt Count: …)` segment from the refused call's service id, HTTP status and request id, and the SNS authorization denial that feeds it must name the action with the SDK service-id casing.

Traces: cloudformation__iam__sns.sns-topic-handler-permission-role-deploys-then-denies-without-create-permission.

## Evidence
Expected (regex the check applies to the single Topic CREATE_FAILED event's ResourceStatusReason):
  Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role-name>/[\w+=,.@-]+ is not authorized to perform: SNS:CreateTopic on resource: <topic-arn> because no identity-based policy allows the SNS:CreateTopic action (Service: Sns, Status Code: 403, Request ID: [0-9a-f-]{36}) (SDK Attempt Count: \d+)" (RequestToken: [0-9a-f-]{36}, HandlerErrorCode: AccessDenied)

Actual (emulator, from the replay):
  AssertionError: the topic failed with 'Resource handler returned message: "User: arn:aws:sts::000000000000:assumed-role/sim2real-xsvc-snshp-d1fb90c86db5/AWSCloudFormation is not authorized to perform: sns:CreateTopic on resource: arn:aws:sns:us-west-2:000000000000:sim2real-xsvc-snshp-b068bb65001c because no identity-based policy allows the sns:CreateTopic action" (RequestToken: d5f642f9-5929-ad71-df56-74150337f975, HandlerErrorCode: AccessDenied)'

(The account, region, role name, topic name and request token above are this run's values filling the template; the divergences are the missing `(Service: Sns, Status Code: 403, Request ID: …) (SDK Attempt Count: …)` segment and the `sns:` vs `SNS:` action prefix.)

## Steps to Reproduce
1. CreateRole trusted by cloudformation.amazonaws.com, and PutRolePolicy granting every AWS::SNS::Topic handler permission EXCEPT sns:CreateTopic (sns:TagResource, sns:Subscribe, sns:GetTopicAttributes, sns:PutDataProtectionPolicy, sns:SetTopicAttributes, sns:UntagResource, sns:Unsubscribe, sns:ListTagsForResource, sns:ListSubscriptionsByTopic, sns:GetDataProtectionPolicy, sns:DeleteTopic, iam:GetRole, iam:PassRole on the role itself).
2. CreateStack with RoleARN set to that role, DisableRollback=true, and a template containing a single AWS::SNS::Topic with an explicit TopicName.
3. Poll DescribeStacks until CREATE_FAILED, then page DescribeStackEvents and take the event with LogicalResourceId=Topic and ResourceStatus=CREATE_FAILED.
4. Inspect its ResourceStatusReason: the quoted handler message ends at `... action"` with no `(Service: Sns, Status Code: 403, Request ID: <uuid>) (SDK Attempt Count: <n>)`, and the action is spelled `sns:CreateTopic` rather than `SNS:CreateTopic`.
