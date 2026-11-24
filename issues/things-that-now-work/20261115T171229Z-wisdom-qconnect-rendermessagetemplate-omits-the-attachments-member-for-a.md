Issue-Source: trace-sweep-divergence
Issue-Trace: qconnect-message-template-attachment-render
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: qconnect-message-template-attachment-render
Issue-Shape-Id: com.amazonaws.qconnect#WisdomService
Issue-Key: wisdom|RenderMessageTemplate|2xx->2xx|$.attachments
Issue-Commit: 860eb666e989875e1daa85fefb4b7f7b73b30cd3

# qconnect RenderMessageTemplate omits the attachments member for a template that has an attachment

> _Note: this issue was identified against the code base at commit `860eb666e989875e1daa85fefb4b7f7b73b30cd3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RenderMessageTemplate

## Description
Cause 1 of 1: RenderMessageTemplate answers 200 but with NO `attachments` member, so an attachment uploaded onto the message template is invisible on the render path.

AWS: Smithy (services/service-com-amazonaws-qconnect-wisdomservice/smithy-model/com-amazonaws-qconnect-wisdomservice.json) — RenderMessageTemplate, POST /knowledgeBases/{knowledgeBaseId}/messageTemplates/{messageTemplateId}/render, success 200, output shape RenderMessageTemplateResponse with members content, sourceConfigurationSummary, attributesNotInterpolated and `attachments` (MessageTemplateAttachmentList, uniqueItems). Doc for that member: "The message template attachments." Each MessageTemplateAttachment element has every member @required: contentDisposition (ContentDisposition, e.g. ATTACHMENT), name (AttachmentFileName, 1..256, must include a file extension, sensitive), uploadedTime (Timestamp, date-time), url (Url, 1..4096, sensitive pre-signed S3 download URL), urlExpiry (Timestamp, date-time) and attachmentId (Uuid, ^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$). So on AWS, after an attachment has been uploaded onto a message template, every render of that template reports that attachment: one element per uploaded, not-yet-deleted attachment, carrying the attachment's own id, its ATTACHMENT disposition, the uploaded file name, the upload timestamp and a fresh pre-signed URL with its expiry. The recorded response confirms exactly this: a one-element attachments list whose attachmentId is the id returned by the upload, contentDisposition 'ATTACHMENT', name the uploaded file name, and uploadedTime/url/urlExpiry present (shape-asserted only, since they are per-run values).

Emulator: the render of a template that has had an attachment uploaded returns 200 with `attachments` absent entirely (replay detail: "$.attachments: missing (expected [{'attachmentId': '{{attachment_id}}', 'contentDisposition': 'ATTACHMENT', 'name': '<file>.txt', 'uploadedTime': '<shaped-not-asserted>', 'url': '<shaped-not-asserted>', 'urlExpiry': '<shaped-not-asserted>'}], got None)"). The handler in services/service-com-amazonaws-qconnect-wisdomservice/src/ops/render_message_template.rs does project the stored template's attachments, but emits the member only when that list is non-empty, and the list is never populated because the upload path never writes state: CreateMessageTemplateAttachment is still a scaffold stub that answers 501 (see the already-open issue 20261111T073850Z-qconnect-createmessagetemplateattachment-stub.md, filed for the stub itself under the `qconnect` token — it does NOT carry this fixture step's replay key, and this divergence is the wire-visible consequence on RenderMessageTemplate, which the stub issue's own replay never exercises). The emulator therefore has no attachment state for a render to report, and nothing in the render path proves the projection once state exists.

Why: a client that uploads an attachment to a message template and then renders it to fetch the attachment's pre-signed download URL gets a response with no attachments at all. Code that iterates response['attachments'] sees an empty/absent collection and concludes the template has no attachments, so the attachment can never be downloaded, listed on the rendered message, or deleted by id — a silent data loss rather than an error the client can detect and retry.

Fix: the message-template attachment state plus the render projection in this service — attachment upload must persist an attachment record (id, contentDisposition, name, uploadedTime, url, urlExpiry) onto the addressed message template (and onto the snapshot a published version carries), and RenderMessageTemplate's output assembly must emit the `attachments` list from that record for every render of a template that holds one. Values are per-environment (the pre-signed URL host/signature, the timestamps and the generated attachment id) and are not themselves requirements; what is required is that the member is present with one fully populated element per live attachment.

Traces: qconnect-message-template-attachment-render#task#5 (RenderMessageTemplate, expected 200 with a one-element attachments list, got 200 with the member missing).

## Evidence
Fixture qconnect-message-template-attachment-render, step task#5, operation RenderMessageTemplate.
Expected (recorded AWS): HTTP 200 with $.attachments = [{attachmentId: <the id returned by the attachment upload>, contentDisposition: 'ATTACHMENT', name: '<uploaded file name>.txt', uploadedTime: <date-time>, url: <pre-signed S3 URL>, urlExpiry: <date-time>}].
Actual (emulator): HTTP 200, $.attachments missing (None). Replay verdict: mismatch, expected_status 200, actual_status 200, divergence_key service=wisdom|RenderMessageTemplate|status=200->200|keys=$.attachments.
Supporting code: services/service-com-amazonaws-qconnect-wisdomservice/src/ops/render_message_template.rs lines 256-265 emit `attachments` only when the stored template's attachment list is non-empty; services/service-com-amazonaws-qconnect-wisdomservice/src/ops/create_message_template_attachment.rs line 4 still carries the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker, so no attachment is ever persisted for the render to report.
Model citation: Smithy RenderMessageTemplate output member `attachments` -> MessageTemplateAttachmentList of MessageTemplateAttachment{contentDisposition, name, uploadedTime, url, urlExpiry, attachmentId — all @required}.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture qconnect-message-template-attachment-render.
2. Setup creates a knowledge base and an EMAIL message template, then uploads an attachment onto that template (CreateMessageTemplateAttachment / the attachment upload URL).
3. At step task#5, call RenderMessageTemplate on the same knowledgeBaseId/messageTemplateId with the fixture's attributes.
4. Observe HTTP 200 whose body has `content`, `sourceConfigurationSummary` and `attributesNotInterpolated` but no `attachments` member, where AWS returns a one-element attachments list describing the uploaded file (attachmentId, contentDisposition ATTACHMENT, name, uploadedTime, pre-signed url, urlExpiry).
