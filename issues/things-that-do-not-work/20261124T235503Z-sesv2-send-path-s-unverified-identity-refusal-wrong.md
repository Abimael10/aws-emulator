Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-send-bulk-email-entry-recipient-limits
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-send-bulk-email-entry-recipient-limits email-email-template-create-render-send-update-delete sesv2-send-email-content-form-exclusivity sesv2-send-bulk-email-entry-replacement-validation
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|SendBulkEmail|4xx->2xx|$.message
Issue-Key: sesv2|SendBulkEmail|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: sesv2|SendBulkEmail|4xx->2xx|status
Issue-Key: sesv2|SendEmail|4xx->4xx|$.message
Issue-Key: sesv2|SendBulkEmail|4xx->4xx|$.message
Issue-Key: sesv2|SendBulkEmail|4xx->4xx|$headers.x-amzn-errortype
Issue-Commit: a28aa3cbb761848919217b218a8a95a512704746

# sesv2 send path's unverified-identity refusal: wrong identity list, skipped by SendBulkEmail, and preempted by tag-name validation

> _Note: this issue was identified against the code base at commit `a28aa3cbb761848919217b218a8a95a512704746`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendBulkEmail

## Description
Three net-new causes on service token sesv2, all in the outbound send path's identity-verification gate (SendEmail POST /v2/email/outbound-emails and SendBulkEmail POST /v2/email/outbound-bulk-emails): the gate does not run at all for SendBulkEmail, it is preempted by an emulator-authored tag-name validation, and when it does fire its refusal message lists the wrong set of identities.

Cause 1 of 3 - SendBulkEmail accepts a send whose destination identity is not verified (fixture sesv2-send-bulk-email-entry-recipient-limits, step task#1, operation SendBulkEmail).
AWS: Smithy (sesv2): SendBulkEmail is POST /v2/email/outbound-bulk-emails, success code 200, output BulkEmailEntryResults (required); its modeled refusals include MessageRejected (errorKind client, httpError 400, doc "The message can't be sent because it contains invalid content.") alongside AccountSuspendedException/BadRequestException/LimitExceededException/MailFromDomainNotVerifiedException (400), NotFoundException (404) and TooManyRequestsException (429). Recorded behavior: when a BulkEmailEntries destination address is not a verified identity, the whole call is refused with HTTP 400 and the message "Email address is not verified. The following identities failed the check in region <REGION>: <identity>[, <identity>...]" - the leading sentence and "The following identities failed the check in region " are fixed text, while <REGION> is the request's region rendered in upper case and the identity list is taken from the request and the emulator's identity state. The recorded step asserts the error header x-amzn-errortype only by shape, so the exact error shape is not pinned by the recording; the status, the message's fixed text and the rule that an unverified destination is refused are the requirements. Only the identity values are per-run.
Emulator: answers HTTP 200 with no error at all - replay reports "status: expected 400, got 200", "$.message: missing (expected 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-bulkdst-49567886@example.com', got None)" and "$headers.x-amzn-errortype: missing". The verification gate SendEmail applies is simply not reached on the bulk path.
Why: a client that sends bulk mail to unverified recipients is told the send succeeded. In a sandboxed account AWS refuses the call, so test suites and applications that assert the refusal (or that rely on it to detect an unverified recipient list) see a false success and silently drop mail; nothing downstream ever reports the rejection.
Fix: the SendBulkEmail handler and its identity-verification gate (services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_bulk_email.rs), which must apply the same verification check the single-send path applies, to the From identity and to every BulkEmailEntries[].Destination address, before accepting the request.
Traces: sesv2-send-bulk-email-entry-recipient-limits#task#1.

Cause 2 of 3 - SendBulkEmail refuses an unverified-identity send with an emulator-authored tag-name refusal instead of the unverified-identity refusal (fixture sesv2-send-bulk-email-entry-replacement-validation, step task#3, operation SendBulkEmail).
AWS: Smithy (sesv2): BulkEmailEntry carries ReplacementTags (MessageTagList) and ReplacementEmailContent.ReplacementTemplate; MessageTag.Name/Value document the permitted character set in prose. Recorded behavior: for this request AWS does not refuse on the replacement tag/template name at all - it refuses the send with HTTP 400 and the unverified-identity message "Email address is not verified. The following identities failed the check in region <REGION>: <identity>" (same fixed text and same variable parts as Cause 1; here the recorded identity is the bulk destination address). So identity verification is evaluated before (or in place of) any tag-name character check on this input.
Emulator: answers HTTP 400 but with a different error entirely - x-amzn-errortype "BadRequestException" and the message "Invalid tag name <slot one>: only alphanumeric ASCII characters, '_',  '-' are allowed." (note also the doubled space before '-' in that emulator-authored text). Replay: "$.message: value_mismatch" plus "$headers.x-amzn-errortype: shape_mismatch (expected '<shaped-not-asserted>', got 'BadRequestException')".
Why: the client is told its request is malformed (a tag-name syntax problem it must "fix") when the real reason the send is rejected is that an identity is unverified. The error class is wrong too, so code that branches on MessageRejected-style send rejections versus request-shape errors takes the wrong branch, and the real cause (unverified identity) is never surfaced.
Fix: the SendBulkEmail handler's validation ordering and its tag-name validation (services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_bulk_email.rs plus the shared send-path validation module) - the identity-verification refusal must take precedence as the recording shows, and the emulator-authored tag-name message must not be invented for input AWS accepts.
Traces: sesv2-send-bulk-email-entry-replacement-validation#task#3.

Cause 3 of 3 - the unverified-identity refusal message lists identities AWS's message does not (fixture email-email-template-create-render-send-update-delete, step task#6, operation SendEmail; the same cluster also covers sesv2-send-email-content-form-exclusivity task#1, task#2 and task#3).
AWS: Smithy (sesv2): SendEmail is POST /v2/email/outbound-emails with modeled 400 refusals including MessageRejected; the message text itself is recorded behavior only. Recorded behavior: the refusal message is "Email address is not verified. The following identities failed the check in region <REGION>: " followed by the identities that actually failed the check for that request - fixed text up to and including the colon, with <REGION> and the identity list as the variable parts drawn from the request and the emulator's identity state.
Emulator: composes a different identity list for the same request: replay reports "$.message: value_mismatch (expected 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-sender-4d4c2072@example.com...', got 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-sender-4d4c2072@example.com, sim2real-probe-sesv2-recipient-4d4c2072@example.com')" - i.e. the emulator appends the destination addresses to the From identity in the list. The status (400) agrees; only $.message diverges. Note the expected value as rendered in the replay detail ends in "...", so a fixer should diff against the fixture's full recorded message rather than against the elided rendering, and must fix the membership rule (which identities belong in the list), not hard-code the recorded addresses.
Why: the message is the only place a client learns which identity to verify. Listing addresses that did not fail the check sends operators off to verify identities AWS never complained about, and any log scraping or assertion on the identity list disagrees with AWS.
Fix: the message composition for the unverified-identity refusal in the send path (services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_email.rs and the shared send-path verification helper) - the list must contain exactly the identities that failed verification for the request, in the order AWS records.
Traces: email-email-template-create-render-send-update-delete#task#6; sesv2-send-email-content-form-exclusivity#task#1, #task#2, #task#3.

Related open issues that do NOT carry these steps' replay keys, so none of them closes any of these causes (the replay report lists all three clusters as unowned): 20261109T155522Z-ses-sesv2-sendemail-to-an-unverified-non-simulator.md (single-send SendEmail wrongly ACCEPTED for an unverified recipient - a different operation and a different step), 20261121T085411Z-sesv2-sendemail-through-a-configuration-set-with-an-sns.md (a configuration-set send refused 400 where AWS accepts), 20261118T045426Z-sesv2-non-send-reads-skip-modeled-validation-and.md (non-send reads) and 20261027T161051Z-sesv2-sns-sqs-sesv2-sendemail-with-content-template-is-refused-501.md (templated single sends answering 501).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; 384 steps, 52 mismatched). Three unowned sesv2 clusters (stub_path empty, owned_by empty):

1) fixture sesv2-send-bulk-email-entry-recipient-limits, step task#1, SendBulkEmail, verdict mismatch, expected_status 400, actual_status 200, affected_count 1:
   status: expected 400, got 200
   $.message: missing (expected 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-bulkdst-49567886@example.com', got None)
   $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)

2) fixture sesv2-send-bulk-email-entry-replacement-validation, step task#3, SendBulkEmail, verdict mismatch, expected_status 400, actual_status 400, affected_count 1:
   $.message: value_mismatch (expected 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-bulkrep-c8252506@example.com', got "Invalid tag name <slot one>: only alphanumeric ASCII characters, '_',  '-' are allowed.")
   $headers.x-amzn-errortype: shape_mismatch (expected '<shaped-not-asserted>', got 'BadRequestException')

3) fixture email-email-template-create-render-send-update-delete, step task#6, SendEmail, verdict mismatch, expected_status 400, actual_status 400, affected_count 4 (also sesv2-send-email-content-form-exclusivity#task#1, #task#2, #task#3):
   $.message: value_mismatch (expected 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-sender-4d4c2072@example.com...', got 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-sender-4d4c2072@example.com, sim2real-probe-sesv2-recipient-4d4c2072@example.com')

Model evidence (smithy_query, services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json): SendEmail POST /v2/email/outbound-emails (200) and SendBulkEmail POST /v2/email/outbound-bulk-emails (200) both model MessageRejected, BadRequestException, MailFromDomainNotVerifiedException, AccountSuspendedException, LimitExceededException and SendingPausedException at httpError 400, NotFoundException 404, TooManyRequestsException 429; MessageRejected doc: "The message can't be sent because it contains invalid content."; BulkEmailEntry members Destination (required), ReplacementTags, ReplacementEmailContent, ReplacementHeaders.

The identity values, region casing aside, are per-run fixture values; the requirement is the status, the error class and the message's fixed text with its list filled from the request/state.

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
3. Cause 1: replay fixture sesv2-send-bulk-email-entry-recipient-limits through step task#1 - call SendBulkEmail with a DefaultContent template and BulkEmailEntries whose Destination.ToAddresses names an address that is NOT a verified identity. Observe HTTP 200 with BulkEmailEntryResults instead of the recorded HTTP 400 "Email address is not verified. The following identities failed the check in region US-WEST-2: <that address>".
4. Cause 2: replay fixture sesv2-send-bulk-email-entry-replacement-validation through step task#3 - call SendBulkEmail with a BulkEmailEntries entry carrying a ReplacementTags name containing a space (e.g. 'slot one') and an unverified destination address. Observe 400 BadRequestException "Invalid tag name <slot one>: only alphanumeric ASCII characters, '_',  '-' are allowed." instead of the recorded unverified-identity refusal.
5. Cause 3: replay fixture email-email-template-create-render-send-update-delete through step task#6 (and sesv2-send-email-content-form-exclusivity task#1-#3) - call SendEmail from an unverified sender to an unverified recipient. Observe the 400 message listing BOTH the sender and the recipient, where the recording lists only the identities that failed the check for that request.
6. After the fix, re-replay sesv2-send-bulk-email-entry-recipient-limits#task#1, sesv2-send-bulk-email-entry-replacement-validation#task#3 and email-email-template-create-render-send-update-delete#task#6 (plus the three sesv2-send-email-content-form-exclusivity steps in that cluster) and confirm all four clusters pass.
