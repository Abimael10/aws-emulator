Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pinpoint.push-channel-state-transition-changes-send-messages-outcome
Issue-Shape-Id: com.amazonaws.pinpoint#Pinpoint
Issue-Key: pinpoint.push-channel-state-transition-changes-send-messages-outcome|run#17|AssertionError|assert after["StatusCode"] == 400, (
Issue-Commit: 2fdb0bca35dbf2816333afc14f0d0074e1e4571d

# pinpoint SendMessages reports SUCCESSFUL/200 for a direct push to an enabled BAIDU channel with an unusable token — AWS answers the push provider's PERMANENT_FAILURE/400 with a JSON StatusMessage

> _Note: this issue was identified against the code base at commit `2fdb0bca35dbf2816333afc14f0d0074e1e4571d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessages

## Description
One net-new root cause in the Pinpoint emulator, on the SendMessages data plane.

Cause 1 of 1: once a configurable push channel IS enabled for the application, the emulator treats every direct send over it as delivered — it has no provider-rejection outcome at all, so enabling the channel changes only the wording of the per-destination result, never its failure mode.

AWS: Smithy: SendMessages -> POST /v1/apps/{ApplicationId}/messages, success 200 with MessageResponse; MessageResponse.Result is "a multipart response for each address ... the address is the key and the result is the value", and MessageResult has required DeliveryStatus (enum; PERMANENT_FAILURE = "An error occurred when delivering the message to the endpoint address. Amazon Pinpoint won't attempt to send the message again."; SUCCESSFUL = "The message was successfully delivered to the endpoint address.") and required StatusCode ("The downstream service status code for delivering the message"), plus StatusMessage ("The status message for delivering the message"). The model does not state the values for this case, so the values below are recorded behavior only. When the application HAS the BAIDU channel configured and enabled but the destination address is not a token the Baidu Cloud Push service accepts (and the stored ApiKey/SecretKey are not real credentials), the call still answers 200 overall, and that destination's MessageResult carries StatusCode 400 — the downstream provider's status, not 200 — DeliveryStatus PERMANENT_FAILURE, and a StatusMessage that is the push provider's rejection serialized as a JSON document, whose errorMessage member is the fixed text "Invalid notification" and whose channelType member is the destination's channel type taken from the request ("BAIDU" here). This is a DIFFERENT failure mode from the unconfigured-channel case on the same operation, which answers StatusCode 404 with the plain-text "BAIDU channel not found. Please enable the BAIDU channel for this application <ApplicationId>.": enabling the channel must move the destination from the service's own 404 channel-not-found verdict to the provider's 400 rejection, not to success.

Emulator: the overall 200 is right, but the destination's result is StatusCode 200, DeliveryStatus SUCCESSFUL, StatusMessage "Message accepted for delivery" and a generated MessageId. The emulator's per-destination verdict is terminal-success as soon as the channel is deliverable and the MessageConfiguration carries a message for it; there is no branch that models the downstream push provider rejecting the token, so no send over an enabled push channel can ever report a provider failure. (The MessageId hex value and the generated token in the replay output are per-run values and are not part of the requirement; the requirement is the status/DeliveryStatus/StatusMessage class.)

Why: a client gets a positive delivery receipt for a message AWS permanently failed. Code that branches on MessageResult.DeliveryStatus — retry/suppression logic, endpoint-health scoring, dead-token cleanup that removes a push token after PERMANENT_FAILURE, dashboards counting delivery success — takes the success path against the emulator and the failure path against AWS, so exactly the failure handling the emulator is used to develop and test is never exercised. It also makes the channel-state transition untestable: against the emulator the before/after outcomes differ only in text, so a test asserting that enabling a channel changes the failure MODE cannot be written against it. Clients that parse StatusMessage as JSON for the provider's errorMessage/channelType get a plain prose sentence and fail to parse.

Fix: the SendMessages handler's per-destination verdict (Target::verdict), which currently falls through to an unconditional success verdict after the deliverable-channel and carries-message checks; it needs the downstream push-provider outcome for a token the enabled channel cannot deliver to, emitting StatusCode 400, DeliveryStatus PERMANENT_FAILURE and the provider's JSON StatusMessage (errorMessage "Invalid notification", channelType = the destination's ChannelType). File/line citations are under Traces.

Related open/fixed issues (this cause is covered by NEITHER): 20261112T053500Z-pinpoint-2-net-new-root-causes-getapp-s is the open issue the replay reports as OWNING the sibling check pinpoint.delete-app-cascades-channel-configuration; its Cause 2 and the now-fixed 20261112T161408Z-pinpoint-baidu-channel-and-sendmessages-responses-diverge-deleted both concern only the PRE-enable, unconfigured-channel StatusMessage text, which now passes — this check advances past that assertion and fails on the POST-enable half, which both issues explicitly recorded as UNVERIFIED. This is not a refused call: the emulator answered 200 and succeeded where AWS fails the destination, so it is reported here as the net-new cause it is rather than as a reopening.

Traces: pinpoint.push-channel-state-transition-changes-send-messages-outcome.

## Evidence
Suite: simparity-behavioral-aws-v1, service heading '### pinpoint' (2/6 passed, 4 failed). Verdict 'mismatch' — the emulator answered, with the wrong per-destination outcome.

Check pinpoint.push-channel-state-transition-changes-send-messages-outcome, at the post-enable assertion:
  AssertionError: expected 400 once BAIDU is enabled but the push token is invalid, got {'DeliveryStatus': 'SUCCESSFUL', 'MessageId': '5f3060d1b0fe83521d3792f3e3e4a529', 'StatusCode': 200, 'StatusMessage': 'Message accepted for delivery'}

  Expected (AWS), MessageResponse.Result[<address>] after UpdateBaiduChannel(Enabled=true) for an address the provider cannot deliver to:
    StatusCode 400, DeliveryStatus 'PERMANENT_FAILURE', StatusMessage = a JSON document with errorMessage 'Invalid notification' (fixed text) and channelType 'BAIDU' (the destination's ChannelType from the request).
  Actual (emulator): StatusCode 200, DeliveryStatus 'SUCCESSFUL', StatusMessage 'Message accepted for delivery', plus a generated MessageId.
  The check's earlier pre-enable assertions now pass: StatusCode 404, DeliveryStatus PERMANENT_FAILURE and StatusMessage 'BAIDU channel not found. Please enable the BAIDU channel for this application <ApplicationId>.' are produced correctly, so the divergence is isolated to the enabled-channel outcome. The check's final assertion — that the pre- and post-enable StatusCodes DIFFER — therefore also cannot hold.

  Code basis: services/service-com-amazonaws-pinpoint-pinpoint/src/ops/send_messages.rs:527-533 returns the 404 channel-not-found verdict for a non-deliverable configurable channel (correct), and lines 541-545 then return Verdict::new(DELIVERY_SUCCESSFUL, STATUS_OK, "Message accepted for delivery") unconditionally for every deliverable channel that carries a message — no provider-rejection verdict exists.
  Model basis: com.amazonaws.pinpoint#SendMessages POST /v1/apps/{ApplicationId}/messages -> 200 MessageResponse (httpPayload); MessageResponse {ApplicationId required, Result: MapOfMessageResult, EndpointResult, RequestId}; MessageResult {DeliveryStatus required (enum incl. PERMANENT_FAILURE/SUCCESSFUL), StatusCode required (__integer, 'the downstream service status code'), StatusMessage, MessageId, UpdatedToken}; BaiduMessage.Body is the notification body.

## Steps to Reproduce
Boot the emulator from this checkout and run the behavioral suite (simparity-behavioral-aws-v1).

1. CreateApp with any name; note ApplicationResponse.Id.
2. Call SendMessages on that application with MessageRequest.Addresses = {<some push token>: {ChannelType: 'BAIDU'}} and MessageConfiguration.BaiduMessage.Body set. Result[<token>] correctly reports StatusCode 404 / PERMANENT_FAILURE / 'BAIDU channel not found. Please enable the BAIDU channel for this application <that Id>.'
3. Call UpdateBaiduChannel(ApplicationId=<that Id>, BaiduChannelRequest={Enabled: true, ApiKey: <any>, SecretKey: <any>}), and confirm GetBaiduChannel reports Enabled true.
4. Repeat the exact same SendMessages call. The emulator now returns Result[<token>] = StatusCode 200, DeliveryStatus 'SUCCESSFUL', StatusMessage 'Message accepted for delivery'. AWS instead returns StatusCode 400, DeliveryStatus 'PERMANENT_FAILURE' and a JSON StatusMessage whose errorMessage is 'Invalid notification' and whose channelType is 'BAIDU' — enabling the channel changes the failure mode, it does not make the send succeed.

Re-replay check pinpoint.push-channel-state-transition-changes-send-messages-outcome to confirm the cause is closed (it asserts both halves of the transition and that the two StatusCodes differ).
