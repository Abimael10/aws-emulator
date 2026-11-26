Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces
Issue-Key: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces|run#38|AssertionError|assert api_tags.get("sim2real-api-tag") == "v2", api_tags
Issue-Commit: d9a6c5d40fe177efc4b980c5614fe1327ed8dfb9

# appsync__cloudformation: an in-place UpdateStack does not reconcile the Tags of AWS::AppSync::Api (and its ChannelNamespace) — ListTagsForResource still reads the pre-update tag values

> _Note: this issue was identified against the code base at commit `d9a6c5d40fe177efc4b980c5614fe1327ed8dfb9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
One net-new root cause behind the one remaining unowned appsync__cloudformation failure.

Cause 1 of 1: the CloudFormation resource handler for AWS::AppSync::Api performs an in-place UPDATE of the scalar properties but never reconciles the resource's `Tags` property against AppSync, so the tags on the live API keep the values the CREATE wrote.

AWS: `AWS::AppSync::Api` carries a `Tags` property as a list of {Key,Value}. On an UpdateStack that changes only non-create-only properties, CloudFormation updates the resource in place (same ApiId/ApiArn) and reconciles the declared tag set onto the live resource, so `appsync:ListTagsForResource(resourceArn=<ApiArn>)` immediately reflects the NEW template values. The AppSync model gives the handler the exact tools for this: `UpdateApi` (POST /v2/apis/{apiId}) has NO `tags` input member at all — its members are apiId, name, ownerContact, eventConfig — so tags are necessarily reconciled through the separate tagging surface, `TagResource` (POST /v1/tags/{resourceArn}, input resourceArn + tags: TagMap) and `UntagResource`, and read back through `ListTagsForResource` (GET /v1/tags/{resourceArn}, output tags: TagMap). The same holds for the sibling `AWS::AppSync::ChannelNamespace` `Tags` property, whose ARN is the namespace ARN. (Smithy, appsync: UpdateApi input = {apiId, name, ownerContact, eventConfig} — no tags member; TagResource.tags TagMap @length 1..50; ListTagsForResource.tags TagMap.) Beyond the tag reconciliation itself, this is recorded behavior only.

Emulator: UpdateStack settles UPDATE_COMPLETE and the in-place scalar change IS applied — `GetApi(apiId).ownerContact` correctly advances from the v1 to the v2 value, and ApiId/ApiArn/the ChannelNamespace ARN are all preserved as an in-place update requires. But `ListTagsForResource(resourceArn=<ApiArn>)` still returns the tag map the create produced: the stack-level tag plus the resource tag at its ORIGINAL value. Replay observed `{'sim2real': '<stack-name>', 'sim2real-api-tag': 'v1'}` where the updated template declares `sim2real-api-tag` = the new value; the assertion `api_tags.get("sim2real-api-tag") == "v2"` fails on the stale 'v1'. The emulator returns no error and no failed resource event — it reports success while leaving the tag set unreconciled, so the divergence is silent. Because the check stops here, the identical ChannelNamespace `Tags` leg (`list_tags_for_resource(resourceArn=<namespace ARN>)` expected to advance to the new value) and the subsequent legs of the scenario are never reached and are unverified for the same cause.

Why: tags are the control surface for cost allocation, attribute-based access control and resource discovery. A client that re-tags an AppSync API by changing the `Tags` property of its template and sees UPDATE_COMPLETE believes the new tag set is live; in fact the API still carries the old one. Anything keyed off those tags — an IAM policy with an `aws:ResourceTag` condition, a cost-allocation report, a Resource Groups / tag-based query, a drift check — then reads a tag set that disagrees with the template that is supposedly deployed, and CloudFormation reports no drift or failure to point at it. Retagging via CloudFormation is simply inert, and the only way to fix a tag is out-of-band through the AppSync tagging API, which immediately puts the stack out of sync with its own template.

Fix: the UPDATE path of the AWS::AppSync::Api CloudFormation resource handler (and the same path of the AWS::AppSync::ChannelNamespace handler), which must diff the previous and new `Tags` property and reconcile it onto the live resource via the appsync tagging surface — TagResource for added/changed keys and UntagResource for removed keys, against the resource's ARN — in addition to the UpdateApi call it already makes for the scalar properties. The handler's CREATE path already applies Tags, so the gap is specifically the update-time diff; no change to UpdateApi itself is appropriate, since the model exposes no tags member there.

Traces: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces

## Evidence
Check appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces, verdict [mismatch].

Failing assertion (after the in-place UpdateStack leg reached UPDATE_COMPLETE and identities were confirmed unchanged):
  assert api_tags.get("sim2real-api-tag") == "v2", api_tags
  AssertionError: {'sim2real': 'sim2real-appsync-evtns-0a92084a2fbb', 'sim2real-api-tag': 'v1'}

Expected (AWS): appsync.list_tags_for_resource(resourceArn=<ApiArn from the stack Output>)['tags'] carries the tag value declared by the UPDATED template — i.e. 'sim2real-api-tag' advanced to the new value. The map's other entry is the stack-level tag; its value is a per-run generated stack name and is not part of the requirement.
Actual (emulator): the same tag key still holds the value the CREATE wrote (the pre-update value), with no error raised and no failed resource event recorded.

Supporting observations from the same replay, all consistent with 'scalars updated, tags not reconciled':
- (out2["ApiId"], out2["ApiArn"], out2["NamespaceArn"]) == the create-time triple — the update was correctly in place, not a replacement.
- get_api(apiId)['ownerContact'] DID advance to the updated template value, so the handler's UpdateApi call ran and succeeded.
- DescribeStacks StackStatus == 'UPDATE_COMPLETE'; the divergence is silent.

Model basis (appsync Smithy): UpdateApi input members are apiId, name (required), ownerContact, eventConfig (required) — there is NO tags member, so update-time tag reconciliation must go through TagResource (POST /v1/tags/{resourceArn}, tags: TagMap @length 1..50) / UntagResource, and is read back via ListTagsForResource (GET /v1/tags/{resourceArn} -> tags: TagMap).

## Steps to Reproduce
1. CreateStack with a template declaring an AWS::AppSync::Api (EventConfig with API_KEY auth providers and connection/publish/subscribe auth modes) whose Properties include OwnerContact and Tags=[{Key: 'x-tag', Value: 'v1'}], plus an AWS::AppSync::ChannelNamespace on that API with Tags=[{Key: 'ns-tag', Value: 'v1'}], and Outputs publishing the Api's ApiId and ApiArn (Fn::GetAtt) and the namespace Ref.
2. Poll DescribeStacks to CREATE_COMPLETE and read the ApiArn Output.
3. appsync ListTagsForResource(resourceArn=<ApiArn>) -> 'x-tag' == 'v1'. Correct.
4. UpdateStack with the SAME template except OwnerContact and both Tags values changed to 'v2' (Name and every create-only property unchanged, so the update is in place).
5. Poll DescribeStacks to UPDATE_COMPLETE and confirm ApiId / ApiArn / the namespace ARN are unchanged.
6. appsync GetApi(apiId=<ApiId>) -> ownerContact is the new 'v2' value, showing the in-place update ran.
7. appsync ListTagsForResource(resourceArn=<ApiArn>) -> BUG: 'x-tag' is still 'v1' instead of 'v2'.
8. Same for the namespace: ListTagsForResource(resourceArn=<namespace ARN>) -> 'ns-tag' still 'v1'.
9. Re-replay the check in trace_ids to confirm the fix; note that legs after this assertion (the handler-v2 publish, the create-only Name rename replacement and the delete-cleanup legs) are gated behind it and must also pass.
