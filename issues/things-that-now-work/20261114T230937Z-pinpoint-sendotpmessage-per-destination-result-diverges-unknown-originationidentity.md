Issue-Source: trace-sweep-divergence
Issue-Trace: pinpoint-send-otp-and-verify-otp
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: pinpoint-send-otp-and-verify-otp
Issue-Shape-Id: com.amazonaws.pinpoint#Pinpoint
Issue-Key: pinpoint|SendOTPMessage|2xx->2xx|$.Result.+12065550100.MessageId
Issue-Key: pinpoint|SendOTPMessage|2xx->2xx|$.Result.+12065550100.StatusCode
Issue-Key: pinpoint|SendOTPMessage|2xx->2xx|$.Result.+12065550100.StatusMessage
Issue-Commit: fbf0662e1538f4e751c1de54648fe720583c1972

# pinpoint SendOTPMessage per-destination result diverges: unknown OriginationIdentity reported as a missing SMS channel (404) instead of the service's 400 origination-identity refusal, and MessageId omitted

> _Note: this issue was identified against the code base at commit `fbf0662e1538f4e751c1de54648fe720583c1972`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendOTPMessage

## Description
One net-new root cause in the Pinpoint emulator: SendOTPMessage computes the per-destination MessageResult from the APPLICATION's SMS-channel state rather than from the request's OriginationIdentity, so every OTP send on an application whose SMS channel is not enabled is reported with the wrong StatusCode, the wrong StatusMessage, and no MessageId. The operation is implemented and answers the right overall HTTP status (200); the divergence is entirely inside MessageResponse.Result.

Cause 1 of 1: SendOTPMessage's per-destination verdict is derived from the app's SMS channel instead of the OriginationIdentity, and MessageId is minted only on success

AWS: Smithy: SendOTPMessage -> POST /v1/apps/{ApplicationId}/otp, success 200 with MessageResponse (httpPayload, required); SendOTPMessageRequestParameters requires BrandName, Channel ("Channel type for the OTP message. Supported values: [SMS]"), DestinationIdentity ("The destination identity to send OTP to"), OriginationIdentity ("The origination identity used to send OTP from") and ReferenceId. MessageResponse.Result is "a map that contains a multipart response for each address ... that the message was sent to", keyed by the address; MessageResult members are DeliveryStatus (required), StatusCode (required, "the downstream service status code for delivering the message"), StatusMessage ("the status message for delivering the message"), MessageId ("the unique identifier for the message that was sent") and UpdatedToken. The model does not state the text of StatusMessage, so the text below is recorded behavior only. For an OTP send whose OriginationIdentity is not a phone number / pool / sender id that exists in the calling account and whose resource policy does not let Pinpoint use it, the service answers 200 overall and reports that one destination with StatusCode 400 and StatusMessage "The provided number does not exist or does not belong to the account, or its resource policy does not allow Pinpoint to use it" — that entire sentence is FIXED text with NO variable part: the service interpolates neither the origination number, nor the destination, nor the ApplicationId into it. The recorded response also carries a MessageId for that destination (its value is a per-run generated id and is not itself a requirement; what is required is that the member is PRESENT even though the destination failed). Note that OTP delivery in AWS is gated on the ORIGINATION IDENTITY named in the request, not on the Pinpoint application's SMS channel configuration, which is why the refusal is a 400 about the provided number rather than a 404 about a channel.

Emulator: the overall status is 200 (match) but the single Result entry is StatusCode 404 (expected 400), StatusMessage "SMS channel not found. Please enable the SMS channel for this application <ApplicationId>." (expected the fixed origination-identity sentence above), and MessageId is ABSENT (expected present). The emulator reaches that verdict by asking the application for a deliverable SMS channel and, when there is none, reusing the shared channel-not-found template that the direct-send path uses — so it reports an application-configuration fault for a request fault, and it mints the MessageId only on the success branch. OriginationIdentity is checked for blankness only and otherwise never validated. The hex application id that appears in the emulator's message is this fixture's generated id, a per-run value, and is not part of the requirement; the defects are the status code, the wrong message class/template, and the omitted member.

Why: a client cannot tell from the emulator why an OTP send failed. The per-destination StatusCode/StatusMessage pair is the only failure report an OTP send produces (the HTTP call succeeds), so SDK callers and operator tooling branch on it: AWS's 400 plus "does not exist or does not belong to the account" tells the caller to fix the OriginationIdentity it passed, while the emulator's 404 "please enable the SMS channel for this application" sends them to change application configuration that is not the problem — and a test or classifier pinned to AWS's status or wording never matches. The missing MessageId additionally breaks callers that read Result[address].MessageId unconditionally to correlate the send (a required-looking correlation handle in the recording), and it makes a failed OTP send indistinguishable in shape from a malformed response.

Fix: the SendOTPMessage handler's per-destination outcome computation — the branch that selects the Outcome from the application's deliverable SMS channel and formats the shared channel-not-found text, plus the conditional that mints the message id only for an accepted delivery. The verdict for an OTP send belongs to validation of the request's OriginationIdentity (and should emit this error class's own fixed text), not to the direct-send channel-state rule; the MessageId must be populated for the destination regardless of the per-destination failure. Related open issue 20261112T053500Z-pinpoint-2-net-new-root-causes-getapp-s.md covers the direct-send SendMessages StatusMessage template for an unconfigured push channel; it does NOT cover SendOTPMessage, and fixing it by correcting the shared channel-not-found template would still leave this operation answering the wrong error class, status code and a missing MessageId.

Traces: pinpoint-send-otp-and-verify-otp#task#2 (SendOTPMessage, verdict mismatch, expected 200 / actual 200 with the three diverging Result members).

## Evidence
Corpus simparity-wire-aws-v1, fixture pinpoint-send-otp-and-verify-otp, step task#2, operation SendOTPMessage, service pinpoint, verdict mismatch, expected_status 200, actual_status 200.

Replay detail (verbatim):
  $.Result.+12065550100.StatusCode: value_mismatch (expected 400, got 404)
  $.Result.+12065550100.StatusMessage: value_mismatch (expected 'The provided number does not exist or does not belong to the account, or its resource policy does not allow Pinpoint to use it', got 'SMS channel not found. Please enable the SMS channel for this application 432de04823eb8593ae57b065a62d661b.')
  $.Result.+12065550100.MessageId: missing (expected '<shaped-not-asserted>', got None)

Expected (AWS), MessageResponse.Result[<DestinationIdentity>] for an OriginationIdentity the account does not own:
  StatusCode: 400
  StatusMessage: 'The provided number does not exist or does not belong to the account, or its resource policy does not allow Pinpoint to use it'  (entirely fixed text — no request or state value is interpolated)
  MessageId: present (value is a per-run generated id, not a requirement)
  DeliveryStatus: as recorded (not reported as diverging)

Actual (emulator):
  StatusCode: 404
  StatusMessage: 'SMS channel not found. Please enable the SMS channel for this application <ApplicationId>.'  — fixed parts '<CHANNEL> channel not found. Please enable the ', ' channel for this application ' and the trailing '.'; the variable part is the ApplicationId taken from the request path (the hex id in the replay output is this fixture's per-run value). Wrong error class for this condition.
  MessageId: absent

Code basis: services/service-com-amazonaws-pinpoint-pinpoint/src/ops/send_otp_message.rs:276-298 — the Outcome is selected from app.deliverable_channel(CHANNEL_TYPE_SMS) and the None arm uses crate::state::channel_not_found_message(CHANNEL_TYPE_SMS, &app.id) with STATUS_NOT_FOUND; line 298 mints message_id only when delivered. OriginationIdentity is read at lines 201-211 for blankness only and never validated against account-owned origination identities.
Model basis: com.amazonaws.pinpoint#SendOTPMessage (POST /v1/apps/{ApplicationId}/otp, success 200, output MessageResponse httpPayload); com.amazonaws.pinpoint#SendOTPMessageRequestParameters.OriginationIdentity (required, 'The origination identity used to send OTP from'); com.amazonaws.pinpoint#MessageResult {DeliveryStatus required, StatusCode required, StatusMessage, MessageId, UpdatedToken}.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1 (fixture pinpoint-send-otp-and-verify-otp, step task#2).
2. Equivalently by hand: CreateApp with any name and note ApplicationResponse.Id.
3. POST /v1/apps/{ApplicationId}/otp with SendOTPMessageRequestParameters = {BrandName: any, Channel: 'SMS', DestinationIdentity: an E.164 number, OriginationIdentity: a phone number the account does not own, ReferenceId: any, CodeLength/AllowedAttempts/ValidityPeriod default}. Do not configure the application's SMS channel (UpdateSmsChannel is still a scaffold, so it cannot be configured over the wire anyway).
4. Read MessageResponse.Result[<DestinationIdentity>]. The emulator returns StatusCode 404, StatusMessage 'SMS channel not found. Please enable the SMS channel for this application <ApplicationId>.' and no MessageId.
5. AWS returns StatusCode 400, StatusMessage 'The provided number does not exist or does not belong to the account, or its resource policy does not allow Pinpoint to use it', and a MessageId for that destination.
6. Re-replay pinpoint-send-otp-and-verify-otp#task#2 to confirm the fix.
