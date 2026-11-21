Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-configuration-set-publishes-delivery-event-to-sns
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-configuration-set-publishes-delivery-event-to-sns
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|SendEmail|2xx->4xx|$.MessageId
Issue-Key: sesv2|SendEmail|2xx->4xx|status
Issue-Commit: 229c05c4a85b849588e172b17631c1308ea16fa2

# SendEmail through a configuration set with an SNS event destination is refused 400 and mints no MessageId

> _Note: this issue was identified against the code base at commit `229c05c4a85b849588e172b17631c1308ea16fa2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendEmail

## Description
One cause, on service token sesv2, in fixture sesv2-configuration-set-publishes-delivery-event-to-sns.

Cause 1 of 1: a SendEmail issued through an existing configuration set (one that carries an enabled SNS event destination) is refused with HTTP 400 and no MessageId, where AWS accepts the send.

AWS: Smithy (sesv2): SendEmail is POST /v2/email/outbound-emails with success code 200; its only output member is MessageId (OutboundMessageId). Input Content is @required; ConfigurationSetName is an optional member (ConfigurationSetName) naming an existing configuration set, and the modeled refusals are AccountSuspendedException/BadRequestException/LimitExceededException/MailFromDomainNotVerifiedException/MessageRejected (all httpError 400), NotFoundException (404) and TooManyRequestsException (429). Recorded behavior: with the sending identity established and the configuration set plus its enabled SnsDestination event destination created in the preceding steps of the same fixture, the send is ACCEPTED — HTTP 200 carrying a MessageId. The MessageId is a per-run value (the recording asserts its shape only, '<shaped-not-asserted>'), so no literal id is a requirement; the requirement is that a send whose request is well formed, whose sending identity is usable and whose named configuration set exists is accepted with 200 and a minted MessageId, rather than refused.

Emulator: answers HTTP 400 and emits no MessageId at all (replay: 'status: expected 200, got 400' and '$.MessageId: missing (expected <shaped-not-asserted>, got None)'). The step's assertions cover only the status and MessageId, so the refusal's error code and message are not captured by this divergence: a fixer reproducing it should record the x-amzn-errortype and message the handler returns, because whichever modeled refusal is being raised here is raised on a request AWS accepts. No modeled refusal applies to this request — the identity, the configuration set and its event destination all exist, having been created by earlier steps of the same fixture that replay as passing.

Why: the configuration-set send is the entry point for every SES event-publication pattern. A client that creates a configuration set, attaches an SNS event destination and then sends through it gets a hard 400 and no message id, so it cannot send mail at all on that path, cannot correlate anything by MessageId, and no delivery/bounce/complaint event can ever be produced downstream. It also masks the next leg: the SES -> SNS -> SQS delivery filed as 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md cannot be observed while the send that must trigger it is refused.

Fix: the SendEmail handler and its gating order (services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_email.rs) — a send naming an existing configuration set with an enabled event destination must pass the handler's gates and return 200 with a freshly minted MessageId; only the modeled conditions (unverified identity, malformed request/content, missing configuration set, suspended/paused account, limits) may refuse it.

Related open issues that do NOT carry this step's replay key, so none closes it (the replay report marks task#8 unowned): 20261118T045426Z-sesv2-send-path-skips-request-and-content-validation.md (send-path validation wording/status on MALFORMED requests, keys sesv2|SendEmail|4xx->4xx), 20261109T155522Z-ses-sesv2-sendemail-to-an-unverified-non-simulator.md (a send that is wrongly ACCEPTED), 20261026T065823Z-sesv2-sns-sqs-ses-publishes-no-events-to.md and 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md (the publication/delivery legs AFTER a successful send), 20261117T190411Z-sesv2-sts-sesv2-sendemail-with-a-tenantname-answers-notimplemented-so.md (TenantName sends). An earlier issue cited by the sqs issue as owning this step (20261109T232952Z-sesv2-sendemail-refuses-the-configuration-set-send-with.md) does not exist in either the open or the resolved folder, which is consistent with the replay report listing task#8 as unowned.

Traces: sesv2-configuration-set-publishes-delivery-event-to-sns#task#8.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture sesv2-configuration-set-publishes-delivery-event-to-sns, step task#8 (sesv2 SendEmail), verdict mismatch, owned_by empty, stub_path empty, affected_count 1:
  status: expected 200, got 400
  $.MessageId: missing (expected '<shaped-not-asserted>', got None)

Expected (AWS, recorded): HTTP 200 with a MessageId whose value is per-run (shape-asserted only).
Actual (emulator): HTTP 400 with no MessageId member. The error code and message of the refusal are not asserted by this step and therefore not reported by the replay.

Surrounding context in the same fixture: the setup and preceding task steps (CreateTopic, CreateQueue, GetQueueAttributes, SetQueueAttributes, Subscribe, sesv2 CreateConfigurationSet, CreateConfigurationSetEventDestination, identity establishment) replay as passing, so the configuration set, its enabled SNS event destination and the sending identity all exist when task#8 runs. The two steps that follow (task#9 sqs GetQueueAttributes, task#10 sqs ReceiveMessage) diverge on the empty queue and are already owned by 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md.

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
3. Replay fixture sesv2-configuration-set-publishes-delivery-event-to-sns and let its setup run: sns CreateTopic; sqs CreateQueue; GetQueueAttributes(QueueArn); SetQueueAttributes with a policy allowing sns.amazonaws.com to sqs:SendMessage from that topic; Subscribe(Protocol='sqs', Endpoint=<queue arn>); sesv2 CreateConfigurationSet and CreateConfigurationSetEventDestination with an enabled SnsDestination on that topic; establish the sending identity. All of these answer as recorded.
4. Step task#8: call sesv2 SendEmail with ConfigurationSetName set to that configuration set and well-formed Content/Destination from the established identity.
5. Observe HTTP 400 with no MessageId, where the recording has HTTP 200 and a MessageId. Capture the response's x-amzn-errortype and message to see which gate in src/ops/send_email.rs refuses the request.
6. After the fix, re-replay sesv2-configuration-set-publishes-delivery-event-to-sns#task#8 and confirm 200 with a minted MessageId (the downstream sqs steps task#9/task#10 remain owned by their own issue).
