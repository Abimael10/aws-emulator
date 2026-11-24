Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-send-email-address-syntax-validation
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-send-email-address-syntax-validation sesv2-send-email-attachment-validation sesv2-send-email-custom-headers-validation sesv2-send-email-destination-address-encoding sesv2-send-email-inline-template-validation sesv2-send-email-message-tag-validation sesv2-send-email-recipient-count-boundary sesv2-send-email-sending-authorization-identity-arn sesv2-send-email-simple-body-and-charset email-email-template-create-render-send-update-delete sesv2-send-email-content-form-exclusivity sesv2-send-email-recipient-field-placement sesv2-send-validation-precedes-configuration-set-lookup sesv2-send-bulk-email-empty-destination-entries sesv2-send-bulk-email-request-validation
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|SendEmail|4xx->4xx|$.message
Issue-Key: sesv2|SendEmail|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: sesv2|SendEmail|4xx->4xx|status
Issue-Key: sesv2|SendBulkEmail|2xx->4xx|$.BulkEmailEntryResults
Issue-Key: sesv2|SendBulkEmail|2xx->4xx|status
Issue-Key: sesv2|SendBulkEmail|4xx->4xx|$.message
Issue-Key: sesv2|SendBulkEmail|4xx->4xx|$headers.x-amzn-errortype
Issue-Commit: 2b493b29800d7a854b42f905b4db8bab42375d7a

# sesv2 send path skips request and content validation, so sends are refused with the wrong status, error type and message

> _Note: this issue was identified against the code base at commit `2b493b29800d7a854b42f905b4db8bab42375d7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendEmail

## Description
This issue bundles 7 net-new causes, all in the SESv2 outbound-send path (SendEmail and SendBulkEmail). The shared fault is that the send handlers do not run AWS's request/content validation stage at all: every malformed request falls straight through to whatever check the emulator does implement (sender verification, configuration-set lookup, template lookup), so the status, the error type and the message a client sees are all wrong, and the per-entry bulk result document is never produced.

Cause 1 of 7 — SendEmail performs no request/content validation before the sender-verification check.
AWS: Smithy: SendEmail input Content is @required (EmailContent), Destination is a Destination structure, EmailTags a MessageTagList, FromEmailAddress an EmailAddress; modeled errors include BadRequestException (httpError 400) and MessageRejected (httpError 400). AWS validates the request and the message content FIRST and refuses with 400 BadRequestException naming the offending element, and only then evaluates whether the sending identity is verified. The recorded responses show this for every family of malformed input exercised by the corpus: a syntactically invalid address is refused with the message "Missing final '@domain'" (fixed text; AWS reports the syntax defect of the address taken from the request), and the sibling steps in the same cluster record the equivalent refusals for attachment validation, unsupported custom headers, message-tag validation, recipient-count limits, destination-address encoding and inline-template fields — each refusal naming the member/value from the request.
Emulator: every one of those 19 steps gets the SAME response instead — 400 with x-amzn-errortype MessageRejected and the message "Email address is not verified. The following identities failed the check in region US-WEST-2: <from-address>" (reported as $.message value_mismatch against "Missing final '@domain'" plus $headers.x-amzn-errortype shape_mismatch, got 'MessageRejected'). The malformed input is never looked at; only the unverified sender is reported.
Why: a client that submits a bad address, header, tag, attachment or recipient list is told its sender identity is unverified. It will chase an identity-verification problem that does not exist, and SDK error handling keyed on BadRequestException vs MessageRejected takes the wrong branch, so programmatic retry/repair of the offending field never happens.
Fix: the SendEmail handler's validation stage — add the request/content validation pass (address syntax, destination encoding, custom headers, message tags, recipient count, attachment and inline-template fields) and run it BEFORE the identity-verification check, raising BadRequestException/400 with AWS's per-check wording.
Traces: sesv2-send-email-address-syntax-validation#task#1 (representative, 19 steps in the cluster, incl. sesv2-send-email-attachment-validation#task#1, sesv2-send-email-custom-headers-validation#task#4, sesv2-send-email-message-tag-validation#task#1, sesv2-send-email-recipient-count-boundary#task#2, sesv2-send-email-destination-address-encoding#task#1, sesv2-send-email-inline-template-validation#task#2).

Cause 2 of 7 — SendEmail's content/header validation does not precede the configuration-set lookup.
AWS: Smithy: SendEmail errors BadRequestException (400) and NotFoundException (404); ConfigurationSetName is an optional input member. Recorded behavior: a request that both names a nonexistent configuration set and carries an unsupported header is refused 400 with the message "Header <From> is not supported" — fixed text "Header <" + the header name taken from the request + "> is not supported" — i.e. request validation is ordered before resource lookup.
Emulator: answers 404 with x-amzn-errortype NotFoundException and "Configuration set <name> does not exist." (status: expected 400, got 404). The unsupported header is never reported.
Why: the client is told to create a configuration set when the real defect is in the message it is sending; after creating the set the same request still fails, now for a reason it was never shown.
Fix: the SendEmail handler's ordering — run header/content validation before resolving ConfigurationSetName.
Traces: sesv2-send-validation-precedes-configuration-set-lookup#task#4.

Cause 3 of 7 — the unverified-identity message omits the other identities that failed the check.
AWS: recorded behavior only (the model gives MessageRejected/400 but not the text). AWS words it "Email address is not verified. The following identities failed the check in region <REGION>: <identity list>" — fixed text up to the colon, with the region and then EVERY identity that failed the check, comma-separated, drawn from the request and the account's identity state (for an address send that is the address and the domain it belongs to). The recorded message continues past the address (the report shows the expected value elided with a trailing ellipsis), while the emulator's stops there.
Emulator: emits only the single from-address: "...region US-WEST-2: <from-address>" — the message is a strict prefix of AWS's, so the remaining failed identities are never named.
Why: a caller (or a human reading the error) cannot tell WHICH identities need verifying — notably that verifying the domain would satisfy the send — and message-matching assertions against the real service's text fail.
Fix: the SendEmail handler's MessageRejected construction — build the identity list from all identities the verification check rejected, not just the from-address.
Traces: email-email-template-create-render-send-update-delete#task#6 (9 steps in the cluster, incl. sesv2-send-email-content-form-exclusivity#task#1, sesv2-send-email-simple-body-and-charset#task#1).

Cause 4 of 7 — SendBulkEmail fails the whole call instead of reporting a per-entry INVALID_PARAMETER result.
AWS: Smithy: SendBulkEmail output BulkEmailEntryResults is @required, a list of BulkEmailEntryResult{Status: BulkEmailStatus, Error: ErrorMessage, MessageId}; BulkEmailStatus includes INVALID_PARAMETER, and Error is documented as "A description of an error that prevented a message being sent". A per-entry defect is therefore reported INSIDE a 200 response, not as a request-level error: an entry whose Destination has no recipients yields HTTP 200 with one result per entry, Status INVALID_PARAMETER, Error "Destination cannot be empty" (fixed text) and MessageId null.
Emulator: answers 400 and emits no BulkEmailEntryResults at all ($.BulkEmailEntryResults missing, status: expected 200, got 400).
Why: a caller sending a batch where one entry is bad loses the whole batch and gets no per-entry diagnosis; the documented partial-success contract (good entries get MessageIds, bad ones get a Status) is unobservable, so batch-send code cannot work out which entry to repair.
Fix: the SendBulkEmail handler — move per-entry validation out of the request-level error path into the per-entry result builder that populates BulkEmailEntryResults.
Traces: sesv2-send-bulk-email-empty-destination-entries#task#1 (cluster also covers #task#2).

Cause 5 of 7 — the same empty-destination bulk send answers 404 on the variant that exercises it through a named resource.
AWS: as Cause 4 — HTTP 200 with BulkEmailEntryResults[0] = {Status: INVALID_PARAMETER, Error: "Destination cannot be empty", MessageId: null}.
Emulator: answers 404 with no BulkEmailEntryResults (status: expected 200, got 404), so the empty-destination entry is reported as a missing resource.
Why: the client sees a not-found error for a resource that exists and never learns the entry's Destination was empty; 404 also drives SDK retry/error classification differently from a 200 partial result.
Fix: the SendBulkEmail handler's lookup/validation ordering plus the per-entry result builder, as in Cause 4.
Traces: sesv2-send-bulk-email-empty-destination-entries#task#3.

Cause 6 of 7 — the bulk destination-count refusal uses a non-AWS message and a non-AWS error type.
AWS: Smithy: SendBulkEmail BulkEmailEntries is @required; modeled errors include BadRequestException (400) and LimitExceededException (400). Recorded behavior: a batch with no entries or with more than the allowed number of destinations is refused 400 with the message "You must specify at least 1 destination, and no more than 50 destinations" — entirely fixed text, stating the rule rather than echoing the submitted count — and the recorded error type is not the emulator's. Hypothesis: AWS classes this request-shape violation as BadRequestException.
Emulator: returns x-amzn-errortype LimitExceededException ($headers.x-amzn-errortype shape_mismatch) with the re-worded message "A bulk send may address no more than 50 destinations, but 51 were provided." — and uses a count-echoing template AWS does not use.
Why: callers matching on AWS's message text or on the error type mis-handle the refusal; LimitExceededException reads as a throttling/quota condition that a client may retry, whereas the request is permanently malformed.
Fix: SendBulkEmail's input validation — emit AWS's fixed message and the BadRequestException classification for both the zero-destination and over-limit cases.
Traces: sesv2-send-bulk-email-request-validation#task#2 (cluster also covers #task#3, #task#7, #task#8).

Cause 7 of 7 — a bulk send with no template content is refused as a missing template instead of as missing content.
AWS: Smithy: SendBulkEmail DefaultContent is @required and BulkEmailContent carries a single optional Template member. Recorded behavior: when neither the default content nor the entry supplies template content, AWS refuses 400 with "Template content must be specified" (fixed text) — a validation failure on the request, independent of what templates exist.
Emulator: refuses with "The default template does not exist" — it resolves a template name it was never given and reports a lookup failure.
Why: the caller is pointed at template storage (and may create a template) when the defect is an unpopulated DefaultContent.Template in the request; message-matching clients mis-classify the failure.
Fix: SendBulkEmail's validation of DefaultContent/BulkEmailEntries — check that template content is present before any template lookup, with AWS's wording.
Traces: sesv2-send-bulk-email-request-validation#task#4 (cluster also covers #task#5, #task#6).

Related open issues that do NOT cover these clusters: 20261109T232952Z-sesv2-sendemail-refuses-the-configuration-set-send-with.md (owns the configuration-set send 400), 20261109T155522Z-ses-sesv2-sendemail-to-an-unverified-non-simulator.md (unverified recipient acceptance), 20261114T220956Z-sesv2-sendemail-with-content-template-answers-501-notimplemented.md (templated send stub). None carries the replay keys above.

## Evidence
Cause 1 — sesv2-send-email-address-syntax-validation#task#1, SendEmail, expected 400 / got 400: $.message value_mismatch (expected "Missing final '@domain'", got 'Email address is not verified. The following identities failed the check in region US-WEST-2: sim2real-probe-sesv2-72ad0a56@example.com'); $headers.x-amzn-errortype shape_mismatch (got 'MessageRejected'). 19 steps in the cluster.
Cause 2 — sesv2-send-validation-precedes-configuration-set-lookup#task#4, SendEmail: status expected 400, got 404; $.message expected 'Header <From> is not supported', got 'Configuration set <...> does not exist.'; x-amzn-errortype got 'NotFoundException'.
Cause 3 — email-email-template-create-render-send-update-delete#task#6, SendEmail, 400->400: $.message expected 'Email address is not verified. The following identities failed the check in region US-WEST-2: <address>...' (expected value elided by the report), got the same text ending at the single address. 9 steps in the cluster.
Cause 4 — sesv2-send-bulk-email-empty-destination-entries#task#1, SendBulkEmail: status expected 200, got 400; $.BulkEmailEntryResults missing (expected [{'Error': 'Destination cannot be empty', 'MessageId': None, 'Status': 'INVALID_PARAMETER'}]).
Cause 5 — sesv2-send-bulk-email-empty-destination-entries#task#3, SendBulkEmail: status expected 200, got 404; $.BulkEmailEntryResults missing (same expected value).
Cause 6 — sesv2-send-bulk-email-request-validation#task#2, SendBulkEmail, 400->400: $.message expected 'You must specify at least 1 destination, and no more than 50 destinations', got 'A bulk send may address no more than 50 destinations, but 51 were provided.'; $headers.x-amzn-errortype shape_mismatch, got 'LimitExceededException'. 4 steps in the cluster.
Cause 7 — sesv2-send-bulk-email-request-validation#task#4, SendBulkEmail, 400->400: $.message expected 'Template content must be specified', got 'The default template does not exist'. 3 steps in the cluster.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus simparity-wire-aws-v1 against it (replay_wire_corpus).
3. Cause 1: replay fixture sesv2-send-email-address-syntax-validation step task#1 — SendEmail with a syntactically invalid address from a sender that is not verified; observe MessageRejected/unverified-identity text instead of 400 BadRequestException "Missing final '@domain'". Same for the attachment/custom-header/message-tag/recipient-count/destination-encoding/inline-template fixtures in the cluster.
4. Cause 2: replay sesv2-send-validation-precedes-configuration-set-lookup step task#4 — SendEmail naming a nonexistent configuration set AND an unsupported header; observe 404 NotFoundException instead of 400 'Header <From> is not supported'.
5. Cause 3: replay email-email-template-create-render-send-update-delete step task#6 — SendEmail from an unverified address; compare the identity list in the message with the recorded one.
6. Causes 4 and 5: replay sesv2-send-bulk-email-empty-destination-entries steps task#1 and task#3 — SendBulkEmail with an entry whose Destination has no recipients; observe 400 / 404 with no BulkEmailEntryResults instead of 200 with Status INVALID_PARAMETER and Error 'Destination cannot be empty'.
7. Cause 6: replay sesv2-send-bulk-email-request-validation steps task#2 and task#7 — SendBulkEmail with more than 50 destinations and with none; observe LimitExceededException and the re-worded message.
8. Cause 7: replay sesv2-send-bulk-email-request-validation step task#4 — SendBulkEmail with no template content; observe 'The default template does not exist'.
