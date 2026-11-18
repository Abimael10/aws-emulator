Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-batch-get-metric-data-interval-boundary
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-batch-get-metric-data-interval-boundary sesv2-send-email-sending-authorization-identity-arn
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|BatchGetMetricData|4xx->2xx|$.message
Issue-Key: sesv2|BatchGetMetricData|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: sesv2|BatchGetMetricData|4xx->2xx|status
Issue-Key: sesv2|GetEmailIdentity|2xx->2xx|$.VerificationInfo
Issue-Related: 20261029T171854Z-sesv2-email-identity-dkim-and-mail-from-attributes-are.md
Issue-Commit: 2b493b29800d7a854b42f905b4db8bab42375d7a

# sesv2 non-send reads skip modeled validation and omit modeled response members

> _Note: this issue was identified against the code base at commit `2b493b29800d7a854b42f905b4db8bab42375d7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetMetricData

## Description
This issue bundles 2 net-new causes on sesv2 read-side operations. The shared fault is that the response is produced without the model's contract being honoured: one operation accepts a request the model forbids and answers 200, the other answers 200 but leaves modeled members unpopulated.

Cause 1 of 2 — BatchGetMetricData accepts a batch whose query identifiers are not unique.
AWS: Smithy: BatchGetMetricData input Queries is @required with @length(min 1, max 10); each BatchGetMetricDataQuery.Id is @required, @length(min 1, max 255) and documented as "The query identifier"; the results are keyed back to those ids, so they must be distinct. Modeled errors include BadRequestException (httpError 400). Recorded behavior: a batch containing two queries with the same Id is refused 400 with the message "All queries must have a unique identifier." — entirely fixed text, no value interpolated from the request.
Emulator: answers 200 with a normal result document ($.message missing, $headers.x-amzn-errortype missing, status: expected 400, got 200), so the duplicate-id batch is processed.
Why: a client that accidentally reuses an Id cannot correlate results to queries, and the collision is silently resolved by the emulator (one result wins or results are merged) instead of being refused. Code written against AWS, which relies on the refusal to catch the id-generation bug, passes under the emulator and then fails against the real service.
Fix: BatchGetMetricData's input validation — reject a Queries list with duplicate Id values with BadRequestException/400 and AWS's fixed message, before any metric lookup.
Traces: sesv2-batch-get-metric-data-interval-boundary#task#3 (cluster also covers #task#5).

Cause 2 of 2 — GetEmailIdentity omits DkimAttributes.NextSigningKeyLength, the whole MailFromAttributes object and the whole VerificationInfo object.
AWS: Smithy: GetEmailIdentity output carries DkimAttributes, MailFromAttributes and VerificationInfo. DkimAttributes.NextSigningKeyLength is a DkimSigningKeyLength enum ([Easy DKIM] "the key length of the future DKIM key pair to be generated"), so an Easy-DKIM identity has it set alongside CurrentSigningKeyLength. MailFromAttributes has three @required members (MailFromDomain, MailFromDomainStatus, BehaviorOnMxFailure); the recording shows AWS returning the object for an identity with no custom MAIL FROM configured, with BehaviorOnMxFailure 'USE_DEFAULT_VALUE' (the documented default, meaning amazonses.com is used) and the domain/status members null. VerificationInfo is likewise returned as an object (LastCheckedTimestamp/LastSuccessTimestamp/ErrorType/SOARecord unset) on an identity that has not yet been checked.
Emulator: returns none of them — $.DkimAttributes.NextSigningKeyLength missing (expected 'RSA_1024_BIT'), $.MailFromAttributes missing (whole structure None), $.VerificationInfo missing (whole structure None) — while still answering 200.
Why: a client reading an identity's DKIM key length, its MAIL FROM behaviour or its verification state gets null/absent members and cannot distinguish "not configured" from "not reported"; SDK code that dereferences MailFromAttributes.BehaviorOnMxFailure or VerificationInfo on a 200 response raises instead of reading the documented defaults.
Fix: the GetEmailIdentity response serializer plus the identity state that backs it — populate NextSigningKeyLength from the identity's Easy-DKIM key configuration, and always emit MailFromAttributes (with BehaviorOnMxFailure defaulted) and VerificationInfo for an existing identity.
Traces: sesv2-send-email-sending-authorization-identity-arn#task#2.
Related open issue that does NOT cover this cluster: 20261029T171854Z-sesv2-email-identity-dkim-and-mail-from-attributes-are.md owns the identity-attribute clusters of other fixtures, but not this fixture/step, which the replay reports unowned.

## Evidence
Cause 1 — sesv2-batch-get-metric-data-interval-boundary#task#3, BatchGetMetricData: status expected 400, got 200; $.message missing (expected 'All queries must have a unique identifier.'); $headers.x-amzn-errortype missing (expected an error type; recorded value not asserted). Cluster also covers #task#5.
Cause 2 — sesv2-send-email-sending-authorization-identity-arn#task#2, GetEmailIdentity, 200->200: $.DkimAttributes.NextSigningKeyLength missing (expected 'RSA_1024_BIT', got None); $.MailFromAttributes missing (expected {'BehaviorOnMxFailure': 'USE_DEFAULT_VALUE', 'MailFromDomain': None, 'MailFromDomainStatus': None}, got None); $.VerificationInfo missing (expected {'ErrorType': None, 'LastCheckedTimestamp': None, 'LastSuccessTimestamp': None, 'SOARecord': None}, got None).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus simparity-wire-aws-v1 against it (replay_wire_corpus).
3. Cause 1: replay fixture sesv2-batch-get-metric-data-interval-boundary steps task#3 and task#5 — BatchGetMetricData with two queries sharing one Id; observe 200 instead of 400 BadRequestException 'All queries must have a unique identifier.'
4. Cause 2: replay fixture sesv2-send-email-sending-authorization-identity-arn step task#2 — create a domain identity, then GetEmailIdentity; observe DkimAttributes.NextSigningKeyLength, MailFromAttributes and VerificationInfo all absent from the 200 response.
