Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__rds__sns.deploy-rds-event-subscription-undefined-category-rejected-and-rolled-back
Issue-Key: cloudformation__rds__sns.deploy-rds-event-subscription-undefined-category-rejected-and-rolled-back|run#10|AssertionError|assert got == _REJECTED, f"the EventSub CREATE_FAILED reason was {got!r}, expected {_REJECTED!r}"
Issue-Commit: 4f6404fbd80fb4416b5a7d1c3745210509011655

# cloudformation__rds__sns: the AWS::RDS::EventSubscription CREATE_FAILED reason is classified HandlerErrorCode: NotFound instead of InternalFailure

> _Note: this issue was identified against the code base at commit `4f6404fbd80fb4416b5a7d1c3745210509011655`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSubscription

## Description
1 net-new root cause in the AWS::RDS::EventSubscription resource provider's downstream-error classification.

Cause 1 of 1: a CreateEventSubscription refusal for an undefined event category is mapped to HandlerErrorCode `NotFound`, where AWS's registry handler reports `InternalFailure`.

AWS: RDS CreateEventSubscription validates each requested EventCategory against the categories DescribeEventCategories defines for the given SourceType and refuses an undefined one. Smithy (rds): CreateEventSubscription error SubscriptionCategoryNotFoundFault, errorKind client, httpError 404, awsQueryError code SubscriptionCategoryNotFound, doc "The supplied category does not exist." The CloudFormation AWS::RDS::EventSubscription provider surfaces that refusal as a CREATE_FAILED ResourceStatusReason in the registry-handler envelope `Resource handler returned message: "<message>" (RequestToken: <token>, HandlerErrorCode: <code>)`. Recorded behavior only for the classification itself (HandlerErrorCode is a resource-provider construct, not modeled in the CFN Smithy model): the recording shows the provider does NOT translate this downstream 404 into a client-side handler code — it reports `HandlerErrorCode: InternalFailure`, carrying the wrapped message `Category : <category> not found for source type <source-type>. (Service: Rds, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)`. In that message the fixed text is `Category : `, ` not found for source type `, `. (Service: Rds, Status Code: 404, Request ID: `, `) (SDK Attempt Count: `, `)`; the variable parts are the rejected category and the SourceType taken from the template's EventCategories/SourceType properties, plus the per-run request id and attempt count (which are masked by the check and are not requirements).

Emulator: the message is now correct — the emulator emits exactly `Category : backup not found for source type db-parameter-group. (Service: Rds, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)` and the stack does settle ROLLBACK_COMPLETE — but it classifies the reason `HandlerErrorCode: NotFound` instead of `InternalFailure`. The replayed reason parsed to the pair ('Category : backup not found for source type db-parameter-group. (Service: Rds, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)', 'NotFound') where AWS gives (<same message>, 'InternalFailure').

Why: HandlerErrorCode is the machine-readable half of a CloudFormation resource failure, and it is what clients branch on. A client that distinguishes a retryable/opaque provider fault (InternalFailure) from a missing-resource fault (NotFound) takes the wrong branch: tooling that treats NotFound as "the referenced thing is gone, recreate or skip it" will retry or re-resolve a dependency that never existed, while a genuine provider fault goes unreported. Any negative test or drift/rollback report that asserts on HandlerErrorCode for this resource type cannot pass.

Fix: the AWS::RDS::EventSubscription resource provider's downstream-error-to-HandlerErrorCode classification in the CloudFormation service — the mapping that turns an RDS error into the registry-handler envelope's HandlerErrorCode. The emulator appears to derive the handler code from the downstream HTTP status (404 -> NotFound); for this provider AWS does not, so the status-driven default must not be applied to a SubscriptionCategoryNotFound refusal. Note this is the same class of defect as the already-open cloudformation__events issue 20261111T230358Z-cloudformation-events-a-resource-provider-s-create-failed.md and the cloudformation__secretsmanager issue 20261117T143604Z-cloudformation-secretsmanager-2-net-new-causes-the-colliding.md, but in a different service and provider, so neither covers it.

Related history: the fixed issue 20261109T135215Z-cloudformation-rds-sns-aws-rds-eventsubscription-ref-is.md made this refusal happen at all (the stack previously reached CREATE_COMPLETE). The refusal and its message are now right; only the HandlerErrorCode classification remains divergent, which that issue never described.

Traces: cloudformation__rds__sns.deploy-rds-event-subscription-undefined-category-rejected-and-rolled-back.

## Evidence
Check cloudformation__rds__sns.deploy-rds-event-subscription-undefined-category-rejected-and-rolled-back, assertion at line 243 of the check:

  AssertionError: the EventSub CREATE_FAILED reason was ('Category : backup not found for source type db-parameter-group. (Service: Rds, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)', 'NotFound'), expected ('Category : backup not found for source type db-parameter-group. (Service: Rds, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)', 'InternalFailure')

Expected (AWS): the DescribeStackEvents CREATE_FAILED row for the EventSub logical resource carries ResourceStatusReason matching `Resource handler returned message: "Category : <category> not found for source type <source-type>. (Service: Rds, Status Code: 404, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: InternalFailure)`.

Actual (emulator): the identical wrapped message, but `HandlerErrorCode: NotFound`.

Everything else in the scenario matched: the stack settled ROLLBACK_COMPLETE, the rolled-back subscription was absent from DescribeEventSubscriptions, and the rolled-back topic was absent from ListTopics. The only divergence is the handler error code.

## Steps to Reproduce
1. CreateStack with a template containing an AWS::SNS::Topic and an AWS::RDS::EventSubscription whose Properties set SubscriptionName, SnsTopicArn: {"Ref": "Topic"}, SourceType: "db-parameter-group", Enabled: true, and EventCategories: ["backup"] — a category DescribeEventCategories does not define for the db-parameter-group source type.
2. Poll DescribeStacks by StackId until a terminal status; it settles ROLLBACK_COMPLETE (correct).
3. Read DescribeStackEvents for the stack and take the single CREATE_FAILED row for the EventSub logical resource.
4. Parse its ResourceStatusReason with the registry-handler envelope pattern `^Resource handler returned message: "(?<message>.*)" \(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?<code>[A-Za-z]+)\)$`.
5. Observe the captured code is `NotFound`; AWS reports `InternalFailure`. The captured message already matches AWS.

Control: the same template with a defined category ("configuration change") reaches CREATE_COMPLETE and reads back correctly, so the divergence is confined to the failure-reason classification.
