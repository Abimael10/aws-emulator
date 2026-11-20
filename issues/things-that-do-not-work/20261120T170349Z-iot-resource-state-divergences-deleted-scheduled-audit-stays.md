Issue-Source: trace-sweep-divergence
Issue-Trace: iot-scheduled-audit-duplicate-and-not-found-errors
Issue-Step: teardown#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-scheduled-audit-duplicate-and-not-found-errors iot-scheduled-audit-lifecycle iot-scheduled-audit-weekly-lifecycle iot-principal-policy-lifecycle iot-topic-rule-destination-http-lifecycle
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeScheduledAudit|4xx->2xx|$.message
Issue-Key: iot|DescribeScheduledAudit|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|DescribeScheduledAudit|4xx->2xx|status
Issue-Key: iot|DeleteCertificate|2xx->4xx|status
Issue-Key: iot|GetTopicRuleDestination|2xx->2xx|$.topicRuleDestination.statusReason
Issue-Commit: db1b3ad25a652e4a15df88815e8c32da3f07f40e

# iot resource-state divergences: deleted scheduled audit stays readable, DeleteCertificate refuses 409, destination statusReason authored

> _Note: this issue was identified against the code base at commit `db1b3ad25a652e4a15df88815e8c32da3f07f40e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScheduledAudit

## Description
Three net-new causes where the emulator's stored state, not its wording, diverges: a delete that is not applied, a delete that is wrongly refused, and a status reason the emulator invents. THREE causes must be closed before this issue is submitted.

Cause 1 of 3: a deleted scheduled audit remains readable. AWS: Smithy DescribeScheduledAudit (GET /audit/scheduledaudits/{scheduledAuditName}) raises ResourceNotFoundException (httpError 404) when the named scheduled audit does not exist, and DeleteScheduledAudit removes it; after a successful delete the read must answer 404 with the message 'Resource scheduledAuditName=<name> was not found' (fixed text around the requested name — see the companion refusal-text issue for the wording cause itself). Emulator: the post-delete read answers HTTP 200 with the audit's full description, and no x-amzn-errortype header — the delete left the record in state (or the read ignores its tombstone). Why: a client cannot observe that a scheduled audit was deleted; cleanup/verification loops that poll for the 404 never terminate, re-creating the same name sees a stale record, and quota-sensitive callers count audits that should be gone. Fix: DeleteScheduledAudit must remove the record from the scheduled-audit state so DescribeScheduledAudit (and the list/describe paths that share it) resolve it as absent. Traces: iot-scheduled-audit-duplicate-and-not-found-errors#teardown#8, iot-scheduled-audit-lifecycle#teardown#7, iot-scheduled-audit-weekly-lifecycle#teardown#8.

Cause 2 of 3: DeleteCertificate refuses a fully detached certificate with 409. AWS: Smithy DeleteCertificate (DELETE /certificates/{certificateId}, success code 200) models DeleteConflictException (httpError 409) only for a certificate that still has attached policies or things (and CertificateStateException 406 for an ACTIVE certificate without forceDelete). In the recorded run the certificate has had its policies and thing principals detached and has been set INACTIVE first, so the delete succeeds with HTTP 200 and an empty body. Emulator: answers HTTP 409 (DeleteConflictException) and the certificate is never deleted. Hypothesis: the detach operations (DetachPolicy / DetachThingPrincipal) do not clear the attachment index DeleteCertificate consults, so the conflict guard still sees attachments. Why: the documented teardown order for a device certificate (detach, deactivate, delete) cannot complete against the emulator — every suite and client that cleans up certificates leaks them and then fails, and the 409 wrongly tells the caller dependents still exist. Fix: the DeleteCertificate handler's conflict guard and the certificate-attachment state it reads — detaches must be reflected there before the guard runs. Traces: iot-principal-policy-lifecycle#teardown#9.

Cause 3 of 3: GetTopicRuleDestination reports an invented statusReason. AWS: Smithy TopicRuleDestination.statusReason is documented as 'Additional details or reason why the topic rule destination is in the current status', and status IN_PROGRESS means 'a topic rule destination was created but has not been confirmed' — a send failure is the ERROR status ('confirmation could not be completed'), not IN_PROGRESS. The recording asserts statusReason's presence and shape but not its value, and the emulator's value does not satisfy that assertion. Emulator: for a freshly created HTTP-URL destination it returns the two-sentence authored prose 'Awaiting confirmation. The confirmation message could not be sent to the destination endpoint.' — which both diverges from the recorded shape and contradicts its own IN_PROGRESS status by asserting the confirmation message failed to send. Why: a client (or operator) reading statusReason concludes the confirmation challenge failed and that the destination needs re-creation, when the destination is simply unconfirmed; an automation that branches on the reason text takes the error branch on a healthy destination. Fix: the GetTopicRuleDestination serializer / topic-rule-destination state must carry a reason consistent with the destination's actual status, and must not assert a delivery failure for an IN_PROGRESS destination. Traces: iot-topic-rule-destination-http-lifecycle#task#2.

Related open issues that do NOT cover these causes: 20261118T071347Z-iot-ca-certificate-plane-diverges-updatecacertificate-never-applies.md (CA certificates, not device certificates), 20261117T200355Z-iot-5-net-new-causes-updatecertificate-s-two.md (UpdateCertificate state guards), 20261112T033305Z-iot-confirmtopicruledestination-s-invalidrequestexception-message-omits-the-trailing.md (ConfirmTopicRuleDestination refusal text), 20261119T122911Z-iot-createscheduledaudit-duplicate-name-resourcealreadyexistsexception-message-is-emulator.md (CreateScheduledAudit duplicate wording).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (score 72.3%, 53 divergence clusters):

1) iot-scheduled-audit-duplicate-and-not-found-errors#teardown#8 (also iot-scheduled-audit-lifecycle#teardown#7, iot-scheduled-audit-weekly-lifecycle#teardown#8) DescribeScheduledAudit after DeleteScheduledAudit — expected 404 with $.message 'Resource scheduledAuditName=<name> was not found' and an x-amzn-errortype header; actual 200, no message, no errortype header (the audit is still described).
2) iot-principal-policy-lifecycle#teardown#9 DeleteCertificate — expected status 200; actual status 409 (DeleteConflictException) after the certificate's policy and thing principal were detached and its status set INACTIVE.
3) iot-topic-rule-destination-http-lifecycle#task#2 GetTopicRuleDestination — 200 both sides; $.topicRuleDestination.statusReason expected to match the recorded shape (value redacted as '<shaped-not-asserted>'), actual 'Awaiting confirmation. The confirmation message could not be sent to the destination endpoint.' while status is IN_PROGRESS.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the simparity-wire-aws-v1 wire corpus.
3. Cause 1: replay iot-scheduled-audit-duplicate-and-not-found-errors through teardown#8 — CreateScheduledAudit, DeleteScheduledAudit, then GET /audit/scheduledaudits/{name}; the read must answer 404 ResourceNotFoundException, not 200 with the audit.
4. Cause 2: replay iot-principal-policy-lifecycle through teardown#9 — create a certificate, attach a policy and a thing principal, detach both, UpdateCertificate to INACTIVE, then DELETE /certificates/{certificateId}; expect 200 with an empty body, not 409.
5. Cause 3: replay iot-topic-rule-destination-http-lifecycle task#2 — CreateTopicRuleDestination with an HTTP URL, then GET /destinations/{arn}; inspect topicRuleDestination.status and .statusReason — the reason must not claim the confirmation message could not be sent while the status is IN_PROGRESS.
