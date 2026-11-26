Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces
Issue-Key: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces|run#28|AssertionError|assert len(payload["successful"]) == 1 and len(payload["failed"]) == 1, payload
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# appsync__cloudformation: an Event API publish never runs the channel namespace's codeHandlers onPublish, so every event is reported successful and no per-event CustomError failure is ever returned

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannelNamespace

## Description
One net-new appsync__cloudformation cause, grounded in the only failing behavioral check that is neither stub-blocked-and-auto-filed nor OWNED by an open issue.

Cause 1 of 1: the Event API HTTP publish path (POST /event on the Api's HTTP dns endpoint) ignores the target channel namespace's codeHandlers, so the onPublish handler is never evaluated and every event in the batch is reported as successful. Failing check: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces.

AWS: Smithy (com.amazonaws.appsync#AWSDeepdishControlPlaneService): CreateChannelNamespace = POST /v2/apis/{apiId}/channelNamespaces, code 200, input name (Namespace, @length 1..50, @pattern ^([A-Za-z0-9](?:[A-Za-z0-9\-]{0,48}[A-Za-z0-9])?)$), publishAuthModes/subscribeAuthModes (AuthModes), codeHandlers (Code, @length 1..32768); GetChannelNamespace = GET /v2/apis/{apiId}/channelNamespaces/{name}, output channelNamespace (ChannelNamespace). The model documents ChannelNamespace$codeHandlers as "The event handler functions that run custom business logic to process published events and subscribe requests." The publish wire shape itself is recorded behavior only: a POST to the Event API's HTTP endpoint path /event with body {"channel": "<namespace>/<segment>", "events": ["<json-encoded event>", ...]} and an accepted auth mode (here x-api-key) answers HTTP 200 whose body partitions the batch — AppSync runs the namespace's onPublish over ctx.events, an event the handler returns unchanged appears in "successful" carrying an identifier and its 0-based "index", and an event the handler returns carrying an error field appears in "failed" with code "CustomError" and message equal to the handler-supplied error string. The status stays 200 even when entries fail. In that per-entry failure the fixed text is the code "CustomError"; the message is a value produced by the deployed handler for that event (here the handler's own string, e.g. 'payload.ok must be true' under handler v1 and 'payload.ready must be true' after the in-place CFN handler update) and the "index" is the event's position in the request batch — none of those values are part of the template.

Emulator: the namespace's handler is stored and readable — the check's prior GetChannelNamespace assertions on codeHandlers, publishAuthModes and channelNamespaceArn all passed — but the publish does not apply it. A two-event publish in which exactly one event must be rejected by the deployed onPublish came back 200 with the whole batch accepted: {'failed': [], 'successful': [{'identifier': '021caa78-77c0-4432-b2a9-c200a05827bc', 'index': 0}, {'identifier': 'bfc30567-3a74-4674-800e-d6a6bcd04b10', 'index': 1}]} — i.e. len(successful)==2, len(failed)==0 where AWS returns one of each, and no failed entry with code "CustomError" and the handler's message exists at all. Because the first publish already diverges, everything the check verifies after it (that a CFN in-place update swapping codeHandlers v1->v2 changes which events are rejected, and that the renamed namespace's publish succeeds under v2) is unverified.

Why: a client of an AppSync Event API relies on its namespace handler for authorization, validation, filtering and enrichment of published events. Against this emulator the handler is dead code: every event is accepted and fanned out, including events the deployed onPublish explicitly rejects, and a client that reads the response's failed[] to report or retry per-event rejections sees an empty list and concludes the whole batch was valid. Changing the handler (a normal deploy-time operation, here through an AWS::AppSync::ChannelNamespace in-place update) has no observable effect on the data plane, so handler rollouts cannot be tested at all.

Fix: the Event API data-plane publish handler for POST /event (the Event API endpoint path in the appsync crate, not the GraphQL data plane) plus its response serializer — resolve the channel's namespace from the request's "channel" prefix, evaluate that namespace's stored codeHandlers onPublish over the decoded batch as ctx.events, and build the 200 response from the handler's return value: events returned unchanged into successful (identifier + original index) and events returned with an error field into failed with code "CustomError" and the handler-supplied message, preserving batch order/arity. A namespace with no codeHandlers keeps today's pass-through behavior.

No open issue covers this: the open appsync__cloudformation issues are about unregistered AWS::AppSync::{ApiCache,SourceApiAssociation,Type} types (20261109T123922Z-appsync-cloudformation-3-net-new-causes-aws-appsync.md), in-place updates of AWS::AppSync::{DataSource,GraphQLSchema,ApiKey} answering NotImplemented (20261114T191235Z-appsync-cloudformation-4-net-new-causes-in-place.md), CodeS3Location not being read from S3 (20261117T120125Z-appsync-cloudformation-s3-codes3location-is-never-read-from.md) and DataSource/Resolver delete ordering (20261121T235951Z-appsync-cloudformation-a-stack-delete-removes-aws-appsync.md); the Event-API-adjacent appsync issue 20261108T213015Z-appsync-2-net-new-causes-updatetype-is-an.md Cause 2 is about Api$dns publishing bare host:port records, i.e. endpoint discovery, not handler execution on publish. The AppSync tagging issue 20261118T153103Z is likewise unrelated (this check's list_tags_for_resource assertions passed).

Traces: appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: 363/455 passed (79.8%); 79 mismatched, 13 errored. Of the 92 failing checks, 91 are marked OWNED by an open issue; this is the only unowned, non-stub-filed failure.

[mismatch] appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces
  File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
  File "check_appsync__cloudformation_deploy_event_api_channel_namespace_cfn_handler_update_and_rename_replaces.py", line 199, in run
    assert len(payload["successful"]) == 1 and len(payload["failed"]) == 1, payload
  AssertionError: {'failed': [], 'successful': [{'identifier': '021caa78-77c0-4432-b2a9-c200a05827bc', 'index': 0}, {'identifier': 'bfc30567-3a74-4674-800e-d6a6bcd04b10', 'index': 1}]}

What succeeded before the divergence (so the cause is isolated to the publish path): the CFN stack carrying AWS::AppSync::Api (EventConfig with API_KEY auth/connection/publish/subscribe modes) and AWS::AppSync::ChannelNamespace (Name 'default', CodeHandlers = handler v1, Tags) reached CREATE_COMPLETE; GetApi returned the expected ownerContact and apiArn; GetChannelNamespace returned codeHandlers exactly equal to the deployed handler v1 source, subscribeAuthModes == publishAuthModes == ['API_KEY'], and channelNamespaceArn equal to the stack's NamespaceArn output; ListTagsForResource on both ARNs returned the deployed tags; CreateApiKey succeeded.

The diverging call: POST <http endpoint>/event with headers {x-api-key: <key>} and body {"channel": "default/room", "events": ["{\"ok\": true, \"turn\": 1}", "{\"ok\": false, \"turn\": 2}"]}. The deployed handler v1 is:
  export function onPublish(ctx) {
    return ctx.events.map((event) => {
      if (event.payload && event.payload.ok === true) { return event; }
      return { ...event, error: 'payload.ok must be true' };
    });
  }
so AWS answers 200 with exactly one successful entry (index 0) and exactly one failed entry whose (code, message) == ("CustomError", "payload.ok must be true"). The emulator answered 200 with both events successful and failed == [], i.e. the handler was not run.

Model evidence (smithy_query): CreateChannelNamespace input codeHandlers : Code, @length 1..32768; ChannelNamespace$codeHandlers doc = "The event handler functions that run custom business logic to process published events and subscribe requests."; GetChannelNamespace = GET /v2/apis/{apiId}/channelNamespaces/{name} -> channelNamespace.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1.
2. Replay check appsync__cloudformation.deploy-event-api-channel-namespace-cfn-handler-update-and-rename-replaces.
3. Equivalently, by hand: CreateStack with a template containing AWS::AppSync::Api (Name=<any>, OwnerContact='Owner-Team-v1', EventConfig={AuthProviders:[{AuthType:API_KEY}], ConnectionAuthModes:[{AuthType:API_KEY}], DefaultPublishAuthModes:[{AuthType:API_KEY}], DefaultSubscribeAuthModes:[{AuthType:API_KEY}]}) and AWS::AppSync::ChannelNamespace (ApiId=GetAtt EvtApi.ApiId, Name='default', PublishAuthModes/SubscribeAuthModes=[{AuthType:API_KEY}], CodeHandlers = an onPublish that returns each event unchanged when event.payload.ok === true and otherwise returns {...event, error: 'payload.ok must be true'}); wait for CREATE_COMPLETE.
4. CreateApiKey(apiId) and POST to the Api's HTTP endpoint path /event with header x-api-key:<key> and body {"channel":"default/room","events":["{\"ok\": true, \"turn\": 1}","{\"ok\": false, \"turn\": 2}"]}.
   Expect HTTP 200 with exactly one entry in successful (index 0, carrying an identifier) and exactly one entry in failed whose code is "CustomError" and whose message is the handler-supplied string 'payload.ok must be true'. The emulator returns 200 with both events in successful and failed == [].
5. After fixing, re-run the whole check so the rest of the scenario is also exercised: a CFN in-place update swapping CodeHandlers to a v2 handler requiring payload.ready === true (OwnerContact/Tags also updated) must keep ApiId/ApiArn/NamespaceArn identical and must make a publish of [{"ok":true,"turn":3},{"ready":true,"turn":4}] return one successful and one failed entry with ("CustomError", 'payload.ready must be true'); renaming the namespace to 'renamed' must replace the ChannelNamespace (new ARN, old name NotFoundException "Channel namespace not found."), a publish on 'renamed/room' with {"ready":true} must return failed == [] and successful indexes [0], and the stack delete must leave neither the namespace nor the Api readable (NotFoundException "API not found.").
