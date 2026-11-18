Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sesv2__sns__sqs.templated-send-with-missing-variable-publishes-rendering-failure
Issue-Key: sesv2__sns__sqs.templated-send-with-missing-variable-publishes-rendering-failure|_drain#29|Timeout|for m in sqs.receive_message(
Issue-Commit: 0129204ced37f63133d2cc688ac6df5b5ea216be

# sesv2__sns__sqs: a templated send whose TemplateData omits a referenced variable renders the reference as empty text and publishes no RENDERING_FAILURE event, so the configuration set's SNS destination never receives a 'Rendering Failure'

> _Note: this issue was identified against the code base at commit `0129204ced37f63133d2cc688ac6df5b5ea216be`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendEmail

## Description
One net-new root cause, exactly as the one surviving sesv2__sns__sqs check grounds it. Related open issues named rather than duplicated: 20261027T161051Z-sesv2-sns-sqs-sesv2-sendemail-with-content-template-is-refused-501.md and 20261114T220956Z-sesv2-sendemail-with-content-template-answers-501-notimplemented.md both record the templated SendEmail path as a hard 501 NotImplemented; the checkout no longer behaves that way (the Template arm of send_email.rs is implemented and renders through crate::state::template_rendering), and neither issue states anything about the RENDERING_FAILURE event type, which is what this check exercises. This check was NOT marked owned by either, so it is filed here.

=== Cause 1 of 1: a missing message variable is silently rendered as nothing, and no RENDERING_FAILURE notification is ever produced ===

AWS: Smithy (services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json) models SendEmail's Content as EmailContent{Simple,Raw,Template} and documents the templated form as a first-class message type ("Templated - A message that contains personalization tags. When you send this type of email, Amazon SES API v2 automatically replaces the tags with values that you specify"). The enum EventType is [SEND, REJECT, BOUNCE, COMPLAINT, DELIVERY, OPEN, CLICK, RENDERING_FAILURE, DELIVERY_DELAY, SUBSCRIPTION], so RENDERING_FAILURE is a modelled event type an event destination's MatchingEventTypes may name, and SES's event-publishing documentation states it is produced only for templated sends, when the template data is missing a variable the template references. The notification DOCUMENT itself is not in the model, so for its shape this is recorded behavior only: the send is ACCEPTED (200, a fresh MessageId), a Send event is published for it, and then a second notification arrives on every Enabled destination whose MatchingEventTypes names RENDERING_FAILURE, carrying eventType 'Rendering Failure', mail.messageId equal to the MessageId SendEmail returned, and a failure object with templateName naming the template the send used and errorMessage naming the variable that had no value (AWS words that sentence "Attribute '<name>' is not present in the rendering data." — fixed text "Attribute '" ... "' is not present in the rendering data." with the missing variable's name taken from the template/request; the template name and message id in the document are likewise values of the request and of the emulator's state, never literals of one run). The same send with the variable supplied produces a Send event and NO Rendering Failure.

Emulator: the templated arm renders a reference the data does not supply as the empty string and then treats the message as a perfectly good send. services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state/template_rendering.rs states the rule it implements as "a name the data does not give renders as nothing", and send_email.rs's (ContentForm::Templated, ..) arm of message_parts() refuses a render only for RenderRefusal::Unsupported (501) and RenderRefusal::TooLarge (400) — never for a reference with no value. Consequently the notification path produces no rendering event at all: services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state/event_publication/notification.rs defines exactly four event triples — SEND ('SEND','Send','Email Sent'), DELIVERY, BOUNCE and COMPLAINT — and nothing in that module can emit RENDERING_FAILURE / 'Rendering Failure'. The check therefore drains the subscribed queue forever waiting for {'Send','Rendering Failure'}, receives only the Send events, and burns its whole budget: the recorded verdict is [mismatch] with "Check timed out after 60.0s".

Why: a client that uses SES templated sending has no way to learn that its personalization data was wrong. On AWS the send is accepted and the defect is reported asynchronously as a Rendering Failure naming the template and the missing attribute — that event is the ONLY signal, and mail-pipeline code (alarms on RENDERING_FAILURE, dead-letter handling, per-template dashboards, operator runbooks) is written against it. Against the emulator the message is instead delivered with the personalization tag rendered as empty text, so a broken template/data pair looks like a complete success, every RENDERING_FAILURE subscriber stays silent forever, and any test or workflow that waits for that event hangs instead of failing.

Fix: the templated arm of the operation's handler plus the service's event state — have message_parts()/handle() in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_email.rs detect a template reference the supplied variables give no value for (the substitution pass in crate::state::template_rendering must report it rather than resolving it to nothing), keep the send ACCEPTED with its MessageId, and add a RENDERING_FAILURE event type to crate::state::event_publication::notification alongside SEND/DELIVERY/BOUNCE/COMPLAINT so it is published, with the failure{templateName,errorMessage} member, to every Enabled destination whose MatchingEventTypes names it (and suppress the recipient delivery for that message, since nothing was rendered). SendBulkEmail renders through the same module and needs the same treatment.

Traces: sesv2__sns__sqs.templated-send-with-missing-variable-publishes-rendering-failure.

## Evidence
Behavioral suite replay, the only surviving failure of the run (not marked ALREADY FILED, not marked OWNED):

### sesv2__sns__sqs
[mismatch] sesv2__sns__sqs.templated-send-with-missing-variable-publishes-rendering-failure
  Error: Check timed out after 60.0s

The check's own drain states what it is waiting for: _drain(sqs, queue_url, {"Send", "Rendering Failure"}, timeout_s=120) with the assertion "events {sorted(missing)} never arrived in {timeout_s}s; got {sorted(seen)}"; because the drain's 120 s deadline is longer than the 60 s check budget, the unreached Rendering Failure surfaces as the timeout rather than as that assertion.

Expected (AWS), with a configuration set whose SNS destination has MatchingEventTypes=['SEND','RENDERING_FAILURE'] and a raw-delivery SQS subscriber on the topic:
  - SendEmail(Content={'Template': {'TemplateName': <tpl>, 'TemplateData': '{}'}}) over a template whose Subject/Text reference {{name}} -> 200 with a MessageId distinct from the control send's;
  - the queue receives a 'Send' event whose mail.messageId is that MessageId, and then a 'Rendering Failure' event with the same mail.messageId, failure.templateName = <tpl> and failure.errorMessage naming the missing variable ('name'), AWS wording "Attribute 'name' is not present in the rendering data.";
  - the positive control (TemplateData='{"name": "control"}') produces the 'Send' event and NO 'Rendering Failure'.
Actual (emulator): the probe SendEmail is accepted and the missing reference is rendered as the empty string, so the send is indistinguishable from the control; only 'Send' events ever arrive and no 'Rendering Failure' document is ever published, so the check never completes.

Emulator source:
  - services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state/template_rendering.rs — module rule: "every {{ name }} a part writes is replaced by the value the data gives that name, a name the data does not give renders as nothing"; RenderRefusal has only Unsupported and TooLarge.
  - services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_email.rs — the (ContentForm::Templated, _, _, Some(template)) arm of message_parts() calls template_rendering::render() and maps only Unsupported -> 501 and TooLarge -> 400 bad_request.
  - services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/state/event_publication/notification.rs — the only event triples defined are SEND, DELIVERY, BOUNCE, COMPLAINT; there is no RENDERING_FAILURE / 'Rendering Failure' spelling anywhere in the publication path.

Smithy: com.amazonaws.sesv2#EventType = [SEND, REJECT, BOUNCE, COMPLAINT, DELIVERY, OPEN, CLICK, RENDERING_FAILURE, DELIVERY_DELAY, SUBSCRIPTION].

## Steps to Reproduce
1. sns CreateTopic; sqs CreateQueue, set its Policy to allow sns.amazonaws.com sqs:SendMessage from that topic, and sns Subscribe the queue with Attributes={'RawMessageDelivery':'true'}.
2. sesv2 CreateConfigurationSet(<set>); sesv2 CreateConfigurationSetEventDestination(ConfigurationSetName=<set>, EventDestinationName='render', EventDestination={'Enabled': True, 'MatchingEventTypes': ['SEND','RENDERING_FAILURE'], 'SnsDestination': {'TopicArn': <topic>}}).
3. sesv2 CreateEmailTemplate(TemplateName=<tpl>, TemplateContent={'Subject': 'probe {{name}}', 'Text': 'Hello {{name}}, this is a rendering probe.'}).
4. Positive control: SendEmail(FromEmailAddress=<verified sender>, Destination={'ToAddresses': ['success@simulator.amazonses.com']}, Content={'Template': {'TemplateName': <tpl>, 'TemplateData': '{"name": "control"}'}}, ConfigurationSetName=<set>) and drain the queue until a 'Send' event whose mail.messageId equals the returned MessageId arrives. No 'Rendering Failure' must appear for this send.
5. The probe: the SAME call with TemplateData='{}' (the referenced variable omitted). Expect 200 with a new MessageId, then a 'Send' event and a 'Rendering Failure' event on the queue with mail.messageId = that MessageId, failure.templateName = <tpl> and failure.errorMessage naming 'name' ("Attribute 'name' is not present in the rendering data.").
6. Observe instead: the send is accepted and rendered with {{name}} replaced by nothing, only a 'Send' event is ever published, and no 'Rendering Failure' document arrives — the drain never completes and the check times out at 60 s.

Replay check: sesv2__sns__sqs.templated-send-with-missing-variable-publishes-rendering-failure.
