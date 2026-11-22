Issue-Source: trace-sweep-divergence
Issue-Trace: ses-inbound-mail-published-by-receipt-rule-sns-action
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ses-inbound-mail-published-by-receipt-rule-sns-action
Issue-Shape-Id: com.amazonaws.ses#SimpleEmailService
Issue-Key: ses|SendEmail|2xx->4xx|$.SendEmailResponse
Issue-Key: ses|SendEmail|2xx->4xx|status
Issue-Commit: 701b7d43696bfd8d52286723c43a23a76a522357

# ses SendEmail is refused 400 with no MessageId on the receipt-rule inbound path

> _Note: this issue was identified against the code base at commit `701b7d43696bfd8d52286723c43a23a76a522357`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendEmail

## Description
One cause, on service token ses, in fixture ses-inbound-mail-published-by-receipt-rule-sns-action.

Cause 1 of 1: a well-formed SES (classic, Query protocol) SendEmail issued after the fixture has established the sending identity and the account's active receipt rule set is refused with HTTP 400 and no SendEmailResponse at all, where AWS accepts the send with HTTP 200 and a minted MessageId.

AWS: Smithy (com.amazonaws.ses#SendEmail) binds Source, Destination and Message as @required and declares output SendEmailResponse with MessageId @required; the operation's success status is 200. Its ONLY modeled refusals are AccountSendingPausedException, ConfigurationSetDoesNotExistException, ConfigurationSetSendingPausedException, MailFromDomainNotVerifiedException and MessageRejected (every one httpError 400, awsQueryError codes AccountSendingPausedException / ConfigurationSetDoesNotExist / ConfigurationSetSendingPausedException / MailFromDomainNotVerifiedException / MessageRejected). The operation's own documentation states the only conditions under which a send is rejected: an unverified Source, a sandbox account sending to a non-verified, non-simulator recipient, a message over 10 MB, no recipient at all, a recipient not in the form UserName@[SubDomain.]Domain.TopLevelDomain, more than 50 recipients across To:/CC:/BCC:, and the 24-hour sending quota. Recorded behavior: in this fixture the preceding steps (which replay as passing) create the SNS topic, the SQS queue and its policy, the subscription, the SES receipt rule set with its SNS receipt-rule action, activate that rule set and establish the sending identity, and the send is then ACCEPTED — HTTP 200 whose body is SendEmailResponse/SendEmailResult/MessageId plus ResponseMetadata/RequestId. MessageId and RequestId are per-run values (the recording asserts only their shape, '<shaped-not-asserted>'), so no literal id is a requirement; the requirement is that a send whose request is well formed, whose Source is a usable identity of the account and which names no missing or paused configuration set is answered 200 with a freshly minted MessageId, not refused.

Emulator: answers HTTP 400 and serializes no SendEmailResponse element whatsoever (replay: 'status: expected 200, got 400' and '$.SendEmailResponse: missing (expected {SendEmailResult: {MessageId: <shaped-not-asserted>}, ResponseMetadata: {RequestId: <shaped-not-asserted>}}, got None)'). The step asserts the status and the response document only, so the replay does not capture the refusal's Error/Code and Error/Message; a fixer reproducing this MUST record the ErrorResponse body, because whichever refusal is raised here is raised on a request AWS accepts. No modeled refusal applies: the identity exists and is verified by earlier steps of this same fixture, there is at least one syntactically valid recipient, the message is tiny, and no configuration set is involved on this path.

Hypothesis (neither the model nor the recording states this, so it is only a starting point): the gates in services/service-com-amazonaws-ses-simpleemailservice/src/ops/send_email.rs that can produce a 400 are the MessageRejected family (empty recipient set, >50 recipients, an address is_email_address() rejects, is_verified_sender() false, the 24-hour quota) plus the two configuration-set errors and the account-wide pause; of these, the sender gate is the likeliest, i.e. state.settle_identities()/is_verified_sender() not treating the identity the fixture established as verified at send time. The other two SES-identity divergences in this same replay (SetIdentityDkimEnabled reporting an identity 'not verified', GetIdentityDkimAttributes reporting DkimEnabled true for an identity that should read false) point at the same identity-state bookkeeping.

Why: SendEmail is the entry point for the whole SES receive path. A client that verifies an identity, creates and activates a receipt rule set with an SNS action and then sends a message to a recipient covered by that rule set gets a hard 400 and no message id: it cannot send mail at all on this path, has nothing to correlate by MessageId, and no received-message notification can ever be produced. It also masks the next leg of the same fixture — the SES receipt-rule SNS publication landing in SQS (task#10 GetQueueAttributes, task#11 ReceiveMessage) — which cannot be observed while the send that must trigger it is refused; those two steps are already owned by 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md.

Fix: the SendEmail handler and the order/content of its refusal gates (services/service-com-amazonaws-ses-simpleemailservice/src/ops/send_email.rs) and the identity-verification state it reads (services/service-com-amazonaws-ses-simpleemailservice/src/state.rs: settle_identities, is_verified_sender, sender_identity_keys). A send that is well formed, whose Source is covered by a verified identity of the account and which names no missing or sending-paused configuration set must reach the acceptance path and return 200 with a minted MessageId; only the modeled conditions above may refuse it, each with its own modeled error code rather than a blanket 400.

Related open issues that do NOT carry this step's replay key, so none closes it (the replay report lists task#9 as unowned, stub_path empty): 20261121T085411Z-sesv2-sendemail-through-a-configuration-set-with-an-sns.md (the same 200->400 shape on the sesv2 surface, different service token and fixture), 20261109T155522Z-ses-sesv2-sendemail-to-an-unverified-non-simulator.md (a send that is wrongly ACCEPTED, the opposite direction), 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md (the downstream delivery leg, which owns task#10 and task#11 of this fixture).

Traces: ses-inbound-mail-published-by-receipt-rule-sns-action#task#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; 202 steps, 196 passed, 6 mismatched), fixture ses-inbound-mail-published-by-receipt-rule-sns-action, step task#9, service ses, operation SendEmail, verdict mismatch, divergence_key 'service=ses|SendEmail|status=200->400|keys=$.SendEmailResponse,status', owned_by empty, stub_path empty, affected_count 1:
  status: expected 200, got 400
  $.SendEmailResponse: missing (expected {'SendEmailResult': {'MessageId': '<shaped-not-asserted>'}, 'ResponseMetadata': {'RequestId': '<shaped-not-asserted>'}}, got None)

Expected (AWS, recorded): HTTP 200 with SendEmailResponse/SendEmailResult/MessageId and ResponseMetadata/RequestId, both values per-run and shape-asserted only.
Actual (emulator): HTTP 400 with no SendEmailResponse element. The refusal's Error/Code and Error/Message are not asserted by this step and therefore not reported by the replay.

Model evidence (smithy_query operation=SendEmail, services/service-com-amazonaws-ses-simpleemailservice/smithy-model/com-amazonaws-ses-simpleemailservice.json): input Source/Destination/Message required; output SendEmailResponse with MessageId required; errors AccountSendingPausedException, ConfigurationSetDoesNotExistException, ConfigurationSetSendingPausedException, MailFromDomainNotVerifiedException, MessageRejected, all httpError 400.

Code evidence: services/service-com-amazonaws-ses-simpleemailservice/src/ops/send_email.rs declares SUCCESS_STATUS = 200 and raises 400 only from the recipient-count gates (lines ~239-252), the invalid-recipient gate (~278-283), the account/configuration-set pause and missing-set gates (~302-341), the unverified-sender gate (~346-351) and the 24-hour quota gate (~357-364); the IAM gate at ~262 refuses with 403, not 400, so the observed 400 originates in one of those gates or in framework-level input validation ahead of handle().

Surrounding context: the fixture's earlier steps (SNS CreateTopic, SQS CreateQueue/GetQueueAttributes/SetQueueAttributes, Subscribe, the SES receipt rule set + SNS receipt-rule action + rule-set activation, identity establishment) all replay as passing, so every resource this send depends on exists when task#9 runs. The two steps after it (task#10 sqs GetQueueAttributes expecting ApproximateNumberOfMessages '1' but getting '0', task#11 sqs ReceiveMessage returning an empty body) are already owned by 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md and are not re-filed here.

## Steps to Reproduce
1. git_clone() the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus()).
3. Let fixture ses-inbound-mail-published-by-receipt-rule-sns-action run its setup: sns CreateTopic; sqs CreateQueue, GetQueueAttributes(QueueArn), SetQueueAttributes with a policy allowing sns.amazonaws.com to sqs:SendMessage from that topic; sns Subscribe(Protocol='sqs', Endpoint=<queue arn>); ses CreateReceiptRuleSet / CreateReceiptRule with an SNSAction on that topic and SetActiveReceiptRuleSet; establish the sending identity. All of these answer as recorded.
4. Step task#9: call ses SendEmail (Query protocol, Action=SendEmail) with the established Source, a Destination.ToAddresses entry covered by the active receipt rule set, and a well-formed Message (Subject + Body.Text).
5. Observe HTTP 400 with no SendEmailResponse element, where the recording has HTTP 200 carrying SendEmailResult/MessageId and ResponseMetadata/RequestId.
6. Capture the 400's ErrorResponse/Error/Code and Error/Message to identify which gate in src/ops/send_email.rs refuses the request (start with the unverified-sender gate and state.settle_identities()/is_verified_sender()).
7. After the fix, re-replay ses-inbound-mail-published-by-receipt-rule-sns-action#task#9 and confirm HTTP 200 with a minted MessageId. The downstream task#10/task#11 sqs steps remain owned by their own issue.
