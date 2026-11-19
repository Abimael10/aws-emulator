Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sesv2.send-email-template-resolution-precedes-the-sender-verification-gate
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2.send-email-template-resolution-precedes-the-sender-verification-gate|run#11|AssertionError|assert good_template_error == not_verified, (
Issue-Commit: 66e0b44d5c5b54c153b466beead5d8bb9530c165

# sesv2: SendEmail with Content.Template answers 501 NotImplemented, so the documented refusal ordering (template-data validation, then the sender-identity gate, then template-name resolution) is unobservable

> _Note: this issue was identified against the code base at commit `66e0b44d5c5b54c153b466beead5d8bb9530c165`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendEmail

## Description
One net-new cause as this check grounds it. Related open issue: 20261027T161051Z-sesv2-sns-sqs-sesv2-sendemail-with-content-template-is-refused-501.md records the same 501 on the templated SendEmail path, but under the sesv2__sns__sqs suite and keyed to a Rendering-Failure event flow; this replay's sesv2 check was NOT marked owned by it, and it adds a requirement that issue does not state — the ORDER in which SendEmail's three refusals fire — so it is filed here and that issue is named rather than duplicated. Confirming this requires both.

=== Cause 1 of 1: the templated SendEmail path is a hard-coded 501, so none of SendEmail's modeled refusals can be reached ===

AWS: Smithy (services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json) binds SendEmail to POST /v2/email/outbound-emails -> 200 {MessageId}; its required Content member targets EmailContent {Simple, Raw, Template}, and the operation's documentation names the templated form a first-class message type ("Templated - A message that contains personalization tags"). SendEmail's modeled error set is exactly AccountSuspendedException (400), BadRequestException (400), LimitExceededException (400), MailFromDomainNotVerifiedException (400), MessageRejected (400), NotFoundException (404), SendingPausedException (400), TooManyRequestsException (429) — there is no 501 and no 'NotImplemented' code in the contract. Recorded behavior for the stage ordering, with the sender held unverified and the mailbox-simulator recipient (success@simulator.amazonses.com) as destination: (a) a VALID stored template is refused MessageRejected (400) with the message 'Email address is not verified. The following identities failed the check in region REGION: ADDRESS' — fixed text 'Email address is not verified. The following identities failed the check in region ' and ': ', with the upper-cased region of the endpoint and the unverified FromEmailAddress from the request; (b) a template name that does NOT exist is refused with the SAME MessageRejected/message — the identity gate masks template-name resolution, so a caller cannot learn the template is missing until the sender is verified; (c) MALFORMED TemplateData is refused with TestRenderEmailTemplate's own verdict on that data (BadRequestException, 400) — a code distinct from MessageRejected, so template-data validation precedes the identity gate.

Emulator: the templated arm never reaches any of that. services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_email.rs dispatches Content in message_parts() and returns crate::scaffolded::Scaffolded::not_implemented("SendEmail with templated content") for the Template arm — HTTP 501, error code 'NotImplemented', message 'operation SendEmail with templated content is scaffolded but not implemented in this emulator' (the fixed text is 'operation ' ... ' is scaffolded but not implemented in this emulator' around the scaffold label; none of it is AWS wording). The Simple and Raw arms are implemented. The check's TestRenderEmailTemplate positive control on the same stored template succeeded, and GetEmailTemplate correctly reported the absent template as NotFoundException 'Template NAME does not exist.', so only the send path diverges.

Why: a client cannot send a templated email at all, and the status/code it gets back is outside the operation's modeled error set, so SDK error handling cannot classify it (not throttling, not validation, not retryable) and it surfaces as an opaque failure. Worse for correctness work: because every templated call is refused identically, the three distinct conditions AWS distinguishes — unverified sender (MessageRejected, which deliberately masks a bad template name) versus malformed TemplateData (BadRequestException, resolved before the identity gate) — collapse into one code, so a caller cannot tell a request it must fix from an identity it must verify.

Fix: the operation's handler and the template state it reads — the Template arm of message_parts() and the handle() body that follows it in services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_email.rs, over the email-template store in crate::state: validate Content.Template.TemplateData with the same reader TestRenderEmailTemplate uses and refuse malformed data BadRequestException FIRST, then apply the FromEmailAddress verification gate (MessageRejected, with AWS's not-verified sentence) BEFORE resolving the template name, then resolve the name against the store and send, minting a MessageId exactly as the Simple arm does.

Traces: sesv2.send-email-template-resolution-precedes-the-sender-verification-gate.

## Evidence
Behavioral suite replay, surviving sesv2 failure (not marked ALREADY FILED, not marked OWNED):

[mismatch] sesv2.send-email-template-resolution-precedes-the-sender-verification-gate
  AssertionError: SendEmail with a valid stored template and an unverified sender returned 'NotImplemented', expected MessageRejected (the identity gate)

Expected (AWS), sender unverified, recipient success@simulator.amazonses.com:
  - Content={'Template': {'TemplateName': <existing>, 'TemplateData': '{"name":"Ada"}'}} -> 400 MessageRejected, 'Email address is not verified. The following identities failed the check in region <REGION>: <unverified address>'
  - the same call naming a template that does not exist -> the identical 400 MessageRejected/message (the identity gate masks name resolution)
  - Content.Template.TemplateData='{"name": ' (malformed) -> the code TestRenderEmailTemplate gives that data, 400 BadRequestException, i.e. a code distinct from MessageRejected
Actual (emulator): HTTP 501, code 'NotImplemented', message 'operation SendEmail with templated content is scaffolded but not implemented in this emulator' for all three legs. 501/'NotImplemented' is outside SendEmail's modeled error set (AccountSuspendedException, BadRequestException, LimitExceededException, MailFromDomainNotVerifiedException, MessageRejected, NotFoundException, SendingPausedException, TooManyRequestsException).

Emulator source: services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_email.rs — message_parts(), the (None, None, Some(template)) arm returns Scaffolded::not_implemented("SendEmail with templated content"); the Simple and Raw arms are implemented.

Already-filed neighbour named for the fixer: 20261027T161051Z-sesv2-sns-sqs-sesv2-sendemail-with-content-template-is-refused-501.md (same 501, recorded under sesv2__sns__sqs for the asynchronous Rendering-Failure flow; this check adds the refusal-ordering requirement and was not marked owned).

## Steps to Reproduce
1. sesv2 CreateEmailTemplate with TemplateContent={'Subject': 'Hello {{name}}', 'Text': 'Hi {{name}}, this is a probe.', 'Html': '<p>Hi {{name}}</p>'}.
2. Confirm the positive control: TestRenderEmailTemplate(TemplateName=<that template>, TemplateData='{"name":"Ada"}') returns a render containing 'Ada' and no '{{name}}' (this passes today).
3. Leaving an address such as <unique>@example.com UNVERIFIED, call SendEmail(FromEmailAddress=<that address>, Destination={'ToAddresses': ['success@simulator.amazonses.com']}, Content={'Template': {'TemplateName': <that template>, 'TemplateData': '{"name":"Ada"}'}}). Expect 400 MessageRejected 'Email address is not verified. The following identities failed the check in region <REGION>: <that address>'; observe 501 'NotImplemented'.
4. Repeat step 3 with a TemplateName that does not exist. Expect the identical 400 MessageRejected and message; observe the same 501. (GetEmailTemplate on that name correctly answers NotFoundException 'Template NAME does not exist.')
5. Repeat step 3 with TemplateData='{"name": ' (malformed). Expect the code TestRenderEmailTemplate returns for that data (400 BadRequestException), different from the MessageRejected of steps 3-4; observe the same 501.

Replay check: sesv2.send-email-template-resolution-precedes-the-sender-verification-gate.
