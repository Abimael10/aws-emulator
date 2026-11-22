Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudformation.deploy-channel-namespace-cfn-invalid-code-handler-rolls-back appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces
Issue-Key: appsync__cloudformation.deploy-channel-namespace-cfn-invalid-code-handler-rolls-back|run#10|AssertionError|assert status == "ROLLBACK_COMPLETE", f"broken stack ended {status!r}: {_fail_reasons(cfn, bad_id)}"
Issue-Key: appsync__cloudformation|ListTagsForResource|BadRequestException
Issue-Commit: 316bc8d4d27618b04e0a3739e3ce26f595ac855b

# appsync__cloudformation: 2 net-new causes — an AWS::AppSync::ChannelNamespace with invalid APPSYNC_JS CodeHandlers deploys CREATE_COMPLETE instead of failing the stack, and ListTagsForResource refuses a ChannelNamespace ARN

> _Note: this issue was identified against the code base at commit `316bc8d4d27618b04e0a3739e3ce26f595ac855b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannelNamespace

## Description
Two distinct, net-new root causes in the appsync/CloudFormation surface. Both are grounded in failing behavioral checks that are neither ALREADY FILED nor OWNED by an open issue. A fixer must close both before submitting.

Cause 1 of 2: AWS::AppSync::ChannelNamespace accepts CodeHandlers that are not valid APPSYNC_JS, so the stack reaches CREATE_COMPLETE where AWS fails the resource and rolls the stack back.
Failing check id: appsync__cloudformation.deploy-channel-namespace-cfn-invalid-code-handler-rolls-back
AWS: Smithy: CreateChannelNamespace.codeHandlers targets Code (@length 1..32768) and the operation models BadRequestException(400); the CFN resource AWS::AppSync::ChannelNamespace maps CodeHandlers onto it. AWS compiles the handler module, so a value that is not a valid APPSYNC_JS module (no exported onPublish/onSubscribe — e.g. a plain sentence) is refused by the service. CloudFormation therefore records CREATE_FAILED for that logical resource with a resource-handler reason of the fixed form `Resource handler returned message: "<service message> (Service: AppSync, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: <code>)` whose variable parts are the ids/counts of the run and whose service message and handler error code are "Error processing APPSYNC_JS code." and InvalidRequest; the stack settles ROLLBACK_COMPLETE, publishes no Outputs, and the AWS::AppSync::Api created alongside it is rolled back too (its name absent from ListApis). The identical template with a valid handler deploys CREATE_COMPLETE and GetChannelNamespace reads the handler back verbatim.
Emulator: the broken stack settles CREATE_COMPLETE with no failure event at all (DescribeStackEvents reports ResourceStatusReason None for the stack, Namespace and EvtApi), so the namespace and the Api both survive, the ApiId output is published, and none of the rollback contract can be observed. The underlying CreateChannelNamespace never validates codeHandlers as APPSYNC_JS.
Why: a deployment carrying a broken event handler is reported as successful. The user gets a live Event API whose namespace cannot work instead of a fast, diagnosable rollback, and CI that gates on stack status (and on the handler-error reason) passes on a template AWS rejects.
Fix: the CreateChannelNamespace handler's codeHandlers compile/validation in the appsync service crate, plus the AWS::AppSync::ChannelNamespace CFN resource handler's failure propagation — a service refusal must become CREATE_FAILED with the resource-handler reason and roll the stack back.
Traces: appsync__cloudformation.deploy-channel-namespace-cfn-invalid-code-handler-rolls-back (mismatch: broken stack ended 'CREATE_COMPLETE').

Cause 2 of 2: ListTagsForResource refuses a ChannelNamespace ARN with BadRequestException "Invalid input resource arn", so the CFN handler-update/rename scenario cannot read a namespace's tags.
Failing check id: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces
AWS: Smithy: ListTagsForResource (GET /v1/tags/{resourceArn}) types resourceArn as ResourceArn (@length 70..75, @pattern ^arn:aws:appsync:[A-Za-z0-9_/.-]{0,63}:\d{12}:apis/[0-9A-Za-z_-]{26}$) and models BadRequestException(400) / NotFoundException(404). That modeled pattern describes only the api ARN, but AWS accepts the ChannelNamespace ARN the service itself returns in ChannelNamespace$channelNamespaceArn (recorded behavior) and answers 200 with that namespace's tag map — including the tags CloudFormation propagated from the stack. A refusal is reserved for an ARN that is not an appsync resource ARN at all.
Emulator: ListTagsForResource(resourceArn=<channelNamespaceArn>) answers HTTP 400 BadRequestException "Invalid input resource arn" — the handler validates the ARN against the api-only shape pattern, so no channel-namespace tag can be read and the CFN update/rename scenario aborts on the call.
Why: tags on CloudFormation-managed channel namespaces are unreadable, so stack-tag propagation, cost allocation and tag-based discovery all break, and the client is told the ARN is invalid even though the emulator minted it.
Fix: the ListTagsForResource (and TagResource / UntagResource) handlers' resourceArn parsing and the tag state they read in the appsync service crate — accept every appsync resource ARN the service issues, including the channel-namespace form.
Traces: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces (error: BadRequestException "Invalid input resource arn" on ListTagsForResource).

## Evidence
Cause 1 — appsync__cloudformation.deploy-channel-namespace-cfn-invalid-code-handler-rolls-back
Expected (AWS): a stack of {AWS::AppSync::Api (EventConfig all API_KEY), AWS::AppSync::ChannelNamespace with CodeHandlers="this is not a valid APPSYNC_JS module"} settles ROLLBACK_COMPLETE; the Namespace resource has exactly one CREATE_FAILED event whose reason matches `Resource handler returned message: "<msg> (Service: AppSync, Status Code: <n>, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <id>, HandlerErrorCode: <code>)` with <msg> == "Error processing APPSYNC_JS code." and <code> == InvalidRequest; no ApiId output is published and the Api name is absent from ListApis. The sibling stack with the valid handler (`export function onPublish(ctx){ return ctx.events; } export function onSubscribe(ctx){}`) reaches CREATE_COMPLETE and GetChannelNamespace returns that exact codeHandlers text.
Actual (emulator): the broken stack ended 'CREATE_COMPLETE' and DescribeStackEvents carries no failure reason at all: {'sim2real-appsync-nsbad-bad-<id>': None, 'Namespace': None, 'EvtApi': None}.

Cause 2 — appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces
Expected (AWS): ListTagsForResource(resourceArn=<the deployed ChannelNamespace's channelNamespaceArn>) -> 200 with the namespace's tag map.
Actual (emulator): botocore.errorfactory.BadRequestException: An error occurred (BadRequestException) when calling the ListTagsForResource operation: Invalid input resource arn.

## Steps to Reproduce
Cause 1:
1. cloudformation.CreateStack with Resources {EvtApi: AWS::AppSync::Api {Name, EventConfig {AuthProviders/ConnectionAuthModes/DefaultPublishAuthModes/DefaultSubscribeAuthModes = [{AuthType: API_KEY}]}}, Namespace: AWS::AppSync::ChannelNamespace {ApiId: !GetAtt EvtApi.ApiId, Name: "default", CodeHandlers: "this is not a valid APPSYNC_JS module"}} and Outputs {ApiId}.
2. Poll DescribeStacks until terminal — AWS: ROLLBACK_COMPLETE; emulator: CREATE_COMPLETE.
3. DescribeStackEvents and look for the single CREATE_FAILED row for 'Namespace' with the resource-handler reason carrying "Error processing APPSYNC_JS code." / HandlerErrorCode InvalidRequest — the emulator records none.
4. Confirm the rolled-back Api is absent from appsync.ListApis (the emulator still lists it) and that the control stack with valid CodeHandlers reaches CREATE_COMPLETE with GetChannelNamespace echoing the handler.

Cause 2:
1. Deploy the same template with a valid CodeHandlers handler and read the created namespace's channelNamespaceArn (GetChannelNamespace or the CFN resource).
2. appsync.ListTagsForResource(resourceArn=<that channelNamespaceArn>) -> expect 200 with the namespace's tags; the emulator answers 400 BadRequestException "Invalid input resource arn".
