Issue-Source: trace-sweep-divergence
Issue-Trace: qconnect-message-template-attachment-render
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: qconnect-message-template-attachment-render
Issue-Shape-Id: com.amazonaws.qconnect#WisdomService
Issue-Key: wisdom|DeleteMessageTemplateAttachment|2xx->2xx|status
Issue-Key: wisdom|DeleteMessageTemplateAttachment|4xx->4xx|$.message
Issue-Commit: 043c88a15d40a18dbcb89d6870550dbf8d0c4458

# qconnect DeleteMessageTemplateAttachment wire responses diverge: success status and ConflictException message text

> _Note: this issue was identified against the code base at commit `043c88a15d40a18dbcb89d6870550dbf8d0c4458`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteMessageTemplateAttachment

## Description
Two net-new root causes on the Amazon Q in Connect (wire service token `wisdom`, Smithy service com.amazonaws.qconnect#WisdomService) message-template attachment DELETE path. Both are on the response side of one operation, DeleteMessageTemplateAttachment: the successful delete answers with the wrong HTTP status, and the refusal of a delete that conflicts with the template's current state answers with the wrong message text. 2 causes in this bundle.

=== Cause 1 of 2: a successful DeleteMessageTemplateAttachment answers HTTP 204 where AWS answers HTTP 200 (fixture qconnect-message-template-attachment-render, step task#6) ===

AWS: Smithy (services/service-com-amazonaws-qconnect-wisdomservice/smithy-model/com-amazonaws-qconnect-wisdomservice.json) — DeleteMessageTemplateAttachment, @http{method DELETE, uri /knowledgeBases/{knowledgeBaseId}/messageTemplates/{messageTemplateId}/attachments/{attachmentId}, code 204}, @idempotent; output shape DeleteMessageTemplateAttachmentResponse has NO members, so the success body is empty either way and the status is the only observable part of the success response. The model's declared code (204) is NOT what the service puts on the wire: for the success status this is recorded behavior only — the recorded AWS response to a delete of an attachmentId the $LATEST working copy does hold is HTTP 200 with an empty body. On the wire the recording governs, so the requirement is status 200 for every successful delete of this operation, regardless of knowledge base, template or attachment id. Hypothesis: the generated handler took its success status from the model's @http code member rather than from the service's actual REST-JSON framing, which is why the one modelled-vs-real discrepancy in this operation is exactly the thing that diverges.

Emulator: HTTP 204 with an empty body. Replay detail for step task#6: `status: expected 200, got 204`. Everything else about the step matches — the delete does take effect and the attachment disappears from the template.

Why: a client branching on the success status of an idempotent delete cannot distinguish "deleted" from "nothing to send" the way AWS frames it. 204 is specified as carrying no body and SDK/HTTP layers are entitled to drop any entity and to treat the exchange as body-less; code that asserts a 200 on this operation (a status allow-list, an HTTP-level retry/logging policy, a proxy or gateway that normalises 2xx by exact code, or a test harness pinned to the recorded status) sees an unexpected status and either escalates the successful delete as an error or skips response handling altogether. The content-free body makes the status the ONLY signal, so getting it wrong makes a correct delete look like a different outcome.

Fix: the success-status framing of this operation's response — the serializer/response path for DeleteMessageTemplateAttachment in the qconnect service crate, which must emit 200 with an empty body on the Ok(DeleteMessageTemplateAttachmentOutput {}) return rather than the model's declared 204. The handler's state mutation is correct and must not change.

Traces: fixture qconnect-message-template-attachment-render, step task#6.

=== Cause 2 of 2: ConflictException for an attachment the working copy no longer holds carries the emulator's not-found sentence instead of AWS's conflict sentence (fixture qconnect-message-template-attachment-render, step teardown#8) ===

AWS: Smithy — DeleteMessageTemplateAttachment's modelled errors are AccessDeniedException(403), ConflictException(409), ResourceNotFoundException(404), ThrottlingException(400), ValidationException(400); ConflictException is a structure with a single `message` member (String), errorKind client, httpError 409, documented "The request could not be processed because of conflict in the current state of the resource." The model does not fix the message TEXT, so for the text this is recorded behavior only: the recorded AWS response to deleting an attachmentId that the template's $LATEST working copy no longer holds is HTTP 409, code ConflictException, message exactly `Unable to delete message template attachment: there is conflict in the current state of resource`. That entire string is FIXED text for this error class — it interpolates no attachment id, template id, knowledge base id, name, limit or member name from the request or from state, which is why it is compared for equality and not by substring.

Emulator: HTTP 409 with code ConflictException (the status and the code are correct) but message `Message template attachment does not exist.` Replay detail for step teardown#8: `$.message: value_mismatch (expected 'Unable to delete message template attachment: there is conflict in the current state of resource', got 'Message template attachment does not exist.')`. The wording comes from the not-found constant the conflict branch borrows (crate::state::MESSAGE_MESSAGE_TEMPLATE_ATTACHMENT_NOT_FOUND), i.e. a not-found sentence about the attachment resource reused for the conflict class.

Why: ConflictException's message is the only place this API separates "your request conflicts with the template's current state" from "the resource is missing" — the code alone is also raised on this service by name-collision and concurrent-mutation paths. A client that matches, logs or surfaces the documented conflict sentence to make a repeated delete idempotent (treating "there is conflict in the current state of resource" as "already gone, proceed") fails to match an unrecognised not-found sentence, so the retry is escalated as an unknown error; operator-facing text likewise misreports a state conflict as a missing attachment.

Fix: the refusal wording in this operation's handler and the constant it uses — the `conflict(...)` refusal taken on the `!stored.remove_attachment(...)` branch of services/service-com-amazonaws-qconnect-wisdomservice/src/ops/delete_message_template_attachment.rs must raise ConflictException with the conflict-class fixed text AWS words for this operation, instead of the attachment not-found constant defined in the service's state module. That not-found constant stays correct where a 404 ResourceNotFoundException is raised for a missing knowledge base or template; only the 409 path must stop borrowing it.

Traces: fixture qconnect-message-template-attachment-render, step teardown#8.

=== Relation to open issues ===
Open issue 20261122T082139Z-qconnect-deletemessagetemplateattachment-conflictexception-for-an-already-deleted-attachment.md describes the same wording fault as cause 2, but it was filed from the BEHAVIORAL corpus (Issue-Corpus: simparity-behavioral-aws-v1, Issue-Trace-Ids: qconnect.message-template-attachment-round-trip) and carries no wire replay key, so the replay reports this step as unowned and the wire fixture/step binding recorded here is net-new; it also does not mention the success-status divergence of cause 1. Open issues 20261118T023847Z-wisdom-qconnect-message-template-attachment-reads-diverge-epoch-uploadedtime.md and 20261115T171229Z-wisdom-qconnect-rendermessagetemplate-omits-the-attachments-member-for-a.md concern attachment READ projections (uploadedTime/urlExpiry formatting, a deleted attachment still rendered, RenderMessageTemplate omitting attachments) and cover neither cause here.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator booted from this checkout: emulator_up true, 12 fixtures replayed, 114 steps (90 passed, 23 mismatched, 1 blocked), score 78.9%, 5 divergence clusters. Three clusters are reported owned_by an open issue (wisdom CreateAIAgentVersion, ListAIAgentVersions, GetMessageTemplate) and are skipped; the two below are the unowned remainder and are bundled here.

Cause 1 — fixture qconnect-message-template-attachment-render, step task#6, verdict mismatch, divergence_key `service=wisdom|DeleteMessageTemplateAttachment|status=200->204|keys=status`:
  Expected (recorded AWS): HTTP 200, empty body.
  Actual (emulator):       HTTP 204, empty body.
  Detail: `status: expected 200, got 204`.
  Model: @http{method DELETE, uri /knowledgeBases/{knowledgeBaseId}/messageTemplates/{messageTemplateId}/attachments/{attachmentId}, code 204}, @idempotent; DeleteMessageTemplateAttachmentResponse has no members. The modelled code and the recorded wire status disagree; the recording is the wire requirement.

Cause 2 — fixture qconnect-message-template-attachment-render, step teardown#8, verdict mismatch, divergence_key `service=wisdom|DeleteMessageTemplateAttachment|status=409->409|keys=$.message`:
  Expected (recorded AWS): HTTP 409, ConflictException, message "Unable to delete message template attachment: there is conflict in the current state of resource" (fixed text; no variable parts).
  Actual (emulator):       HTTP 409, ConflictException (status and code match), message "Message template attachment does not exist."
  Detail: `$.message: value_mismatch (expected 'Unable to delete message template attachment: there is conflict in the current state of resource', got 'Message template attachment does not exist.')`.
  Code: services/service-com-amazonaws-qconnect-wisdomservice/src/ops/delete_message_template_attachment.rs:256-260 returns `conflict(crate::state::MESSAGE_MESSAGE_TEMPLATE_ATTACHMENT_NOT_FOUND)` on the `!stored.remove_attachment(&input.attachment_id)` branch.

No per-run value (request id, clock time, generated uuid, host or port) is part of either assertion: cause 1 is a bare status and cause 2 is fixed error text.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, then replay wire fixture qconnect-message-template-attachment-render from trace set simparity-wire-aws-v1.
2. CreateKnowledgeBase(knowledgeBaseType=MESSAGE_TEMPLATES) and poll GetKnowledgeBase until status ACTIVE.
3. CreateMessageTemplate(channelSubtype=EMAIL) on that knowledge base; keep messageTemplateId.
4. CreateMessageTemplateAttachment(knowledgeBaseId, messageTemplateId, contentDisposition=ATTACHMENT, name=<file name>, body=<base64 bytes>); keep attachment.attachmentId.
5. Cause 1 (step task#6): DELETE /knowledgeBases/{knowledgeBaseId}/messageTemplates/{messageTemplateId}/attachments/{attachmentId} for that attachmentId and inspect the HTTP status line only — the emulator answers 204 where AWS answers 200 (both with an empty body).
6. Cause 2 (step teardown#8): issue the same DELETE a second time, for an attachmentId the working copy no longer holds, and read the error body — status 409 and code ConflictException are correct, but message is "Message template attachment does not exist." where AWS returns the fixed conflict text "Unable to delete message template attachment: there is conflict in the current state of resource".
