Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: qconnect.message-template-attachment-round-trip
Issue-Shape-Id: com.amazonaws.qconnect#WisdomService
Issue-Key: qconnect|DeleteMessageTemplateAttachment|ConflictException
Issue-Commit: cb633ca166024207bcba2122866495f002551178

# qconnect DeleteMessageTemplateAttachment: ConflictException for an already-deleted attachment carries the wrong message text

> _Note: this issue was identified against the code base at commit `cb633ca166024207bcba2122866495f002551178`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteMessageTemplateAttachment

## Description
One net-new root cause on the Amazon Q in Connect message-template attachment delete path. The 409 classification is already right; the message body of that refusal is not.

Cause 1 of 1: re-deleting a message-template attachment that is already gone is correctly refused with ConflictException (HTTP 409), but the exception's `message` member is the emulator's own not-found wording instead of the conflict wording AWS sends for this operation.

AWS: Smithy (services/service-com-amazonaws-qconnect-wisdomservice/smithy-model/com-amazonaws-qconnect-wisdomservice.json) — DeleteMessageTemplateAttachment, DELETE /knowledgeBases/{knowledgeBaseId}/messageTemplates/{messageTemplateId}/attachments/{attachmentId}, success 204, modelled errors AccessDeniedException(403), ConflictException(409), ResourceNotFoundException(404), ThrottlingException(400), ValidationException(400). ConflictException is a structure with a single `message` member (String) and is documented "The request could not be processed because of conflict in the current state of the resource." The model does not fix the message TEXT, so for the text this is recorded behavior only: the recorded AWS response for a delete of an attachmentId the $LATEST working copy no longer holds is HTTP 409, code ConflictException, message exactly `Unable to delete message template attachment: there is conflict in the current state of resource`. That whole string is FIXED text for this error class — it interpolates no identifier, name, id, limit or member name from the request or from state, which is why the behavioral check compares it for equality rather than by substring.

Emulator: HTTP 409 with code ConflictException (correct) but message `Message template attachment does not exist.` — the constant crate::state::MESSAGE_MESSAGE_TEMPLATE_ATTACHMENT_NOT_FOUND, which is worded as a not-found sentence for the attachment resource and reused here for the conflict class. Replay detail: `AssertionError: unexpected message 'Message template attachment does not exist.'` raised after the code assertion passed.

Why: ConflictException's message is the only place this API distinguishes a conflict with the template's current state from a missing resource, and callers routinely branch on it because the code alone (ConflictException) is also raised by name-collision and concurrent-mutation paths of this service. A client that matches, logs or surfaces the documented conflict sentence sees an unrecognised not-found sentence instead: idempotent-delete handling that treats "there is conflict in the current state of resource" as "already deleted, proceed" fails to match and the retry is escalated as an unknown error, and operator-facing text misreports a state conflict as a missing attachment.

Fix: the refusal wording in this operation's handler and the message constant it uses — services/service-com-amazonaws-qconnect-wisdomservice/src/ops/delete_message_template_attachment.rs (the `conflict(...)` refusal at the `!stored.remove_attachment(...)` branch) must raise ConflictException with the conflict-class fixed text AWS words for this operation, rather than the attachment not-found sentence defined in services/service-com-amazonaws-qconnect-wisdomservice/src/state/message_template_attachments.rs (MESSAGE_MESSAGE_TEMPLATE_ATTACHMENT_NOT_FOUND). That not-found constant stays correct where a 404 ResourceNotFoundException is raised for a missing knowledge base or template; only the 409 path must stop borrowing it. Note that the open issue 20261118T023847Z-wisdom-qconnect-message-template-attachment-reads-diverge-epoch-uploadedtime.md (cause 3) reports this operation as a 501 scaffold stub whose delete never took effect; in this checkout the delete DOES take effect and the attachment does disappear from GetMessageTemplate, so that issue does not cover this message divergence, and neither does 20261115T171229Z-wisdom-qconnect-rendermessagetemplate-omits-the-attachments-member-for-a.md.

Traces: behavioral check qconnect.message-template-attachment-round-trip (the final step: a second DeleteMessageTemplateAttachment for an attachmentId the first delete removed).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator booted from this checkout: 23/30 passed; this is the only surviving failure after skipping the checks the report marks OWNED by an open issue.

Check qconnect.message-template-attachment-round-trip, verdict mismatch. The check creates an ACTIVE MESSAGE_TEMPLATES knowledge base, creates an EMAIL message template, uploads an attachment (contentDisposition ATTACHMENT, name receipt.pdf, base64 body), confirms the attachmentId appears in GetMessageTemplate.messageTemplate.attachments, deletes it, confirms it is gone — all of which PASS — then deletes the same attachmentId again.

Expected (recorded AWS): HTTP 409, Error.Code = "ConflictException", Error.Message = "Unable to delete message template attachment: there is conflict in the current state of resource" (fixed text; no variable parts).
Actual (emulator): HTTP 409, Error.Code = "ConflictException" (matches), Error.Message = "Message template attachment does not exist."
Failure: AssertionError: unexpected message 'Message template attachment does not exist.'

Code: services/service-com-amazonaws-qconnect-wisdomservice/src/ops/delete_message_template_attachment.rs:256-260 returns `conflict(crate::state::MESSAGE_MESSAGE_TEMPLATE_ATTACHMENT_NOT_FOUND)`; that constant is defined at services/service-com-amazonaws-qconnect-wisdomservice/src/state/message_template_attachments.rs:41-42 as "Message template attachment does not exist."
Model: DeleteMessageTemplateAttachment http {method DELETE, uri /knowledgeBases/{knowledgeBaseId}/messageTemplates/{messageTemplateId}/attachments/{attachmentId}, code 204}, idempotent; ConflictException = structure{message: String}, errorKind client, httpError 409.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, then replay behavioral check qconnect.message-template-attachment-round-trip from suite simparity-behavioral-aws-v1.
2. CreateKnowledgeBase(knowledgeBaseType=MESSAGE_TEMPLATES) and poll GetKnowledgeBase until status ACTIVE.
3. CreateMessageTemplate(channelSubtype=EMAIL) on that knowledge base and keep the messageTemplateId.
4. CreateMessageTemplateAttachment(knowledgeBaseId, messageTemplateId, contentDisposition=ATTACHMENT, name="receipt.pdf", body=<base64 bytes>) and keep attachment.attachmentId.
5. DeleteMessageTemplateAttachment(knowledgeBaseId, messageTemplateId, attachmentId) — succeeds (204) and the id disappears from GetMessageTemplate.
6. Call DeleteMessageTemplateAttachment again with the same attachmentId and inspect the error body: the code is ConflictException as AWS returns, but the message is "Message template attachment does not exist." where AWS returns "Unable to delete message template attachment: there is conflict in the current state of resource".
