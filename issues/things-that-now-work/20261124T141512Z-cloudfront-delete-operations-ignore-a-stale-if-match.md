Issue-Source: trace-sweep-divergence
Issue-Trace: cfetag-field-level-encryption-profile-etag-chain
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cfetag-field-level-encryption-profile-etag-chain cfetag-public-key-etag-chain
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|4xx->2xx|$
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|4xx->2xx|$headers.content-length
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|4xx->2xx|status
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|2xx->4xx|status
Issue-Key: cloudfront|DeletePublicKey|4xx->2xx|$
Issue-Key: cloudfront|DeletePublicKey|4xx->2xx|$headers.content-length
Issue-Key: cloudfront|DeletePublicKey|4xx->2xx|status
Issue-Commit: 7a3560cb2ad6221574700506c0d4ad6cbe570c9e

# CloudFront delete operations ignore a stale If-Match precondition

> _Note: this issue was identified against the code base at commit `7a3560cb2ad6221574700506c0d4ad6cbe570c9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteFieldLevelEncryptionProfile

## Description
Three causes, all one shared fault: the CloudFront DELETE handlers never evaluate the caller's If-Match header against the resource's current ETag. A non-current (stale) If-Match is accepted as if it matched, the resource is destroyed, and the conditional-delete contract AWS uses for optimistic concurrency is absent. The third cause is the same omission on a second resource type, and the second cause is the state damage it leaves behind.

Cause 1 of 3: DeleteFieldLevelEncryptionProfile accepts a stale If-Match instead of refusing with PreconditionFailed.
AWS: Smithy: DeleteFieldLevelEncryptionProfile input member IfMatch is bound to the If-Match HTTP header; the operation models error PreconditionFailed @httpError 412 ("The precondition in one or more of the request fields evaluated to false.") alongside InvalidIfMatchVersion (400) and NoSuchFieldLevelEncryptionProfile (404). Success is HTTP 204 with an empty body. When If-Match carries a well-formed but no-longer-current version, AWS leaves the resource in place and answers 412 with error Type 'Sender', Code 'PreconditionFailed' and the recorded message "The request failed because it didn't meet the preconditions in one or more request-header fields." — that sentence is entirely fixed text for this error class; no part of it is taken from the request. The body is the REST-XML ErrorResponse envelope (ErrorResponse/Error/{Type,Code,Message} plus RequestId, RequestId being a per-run value) and Content-Length is present.
Emulator: answers HTTP 204 with an empty body and no Content-Length, i.e. it reports a successful delete, and the profile is actually removed. The ErrorResponse envelope is absent entirely.
Why: a client that reads a resource, is handed an ETag, and then issues a conditional delete to be sure nothing changed underneath it gets no protection at all. A delete built on an out-of-date version silently wins, so a concurrent update by another caller is destroyed with no error anywhere; code that specifically tests for 412 to re-read and retry never takes that branch.
Fix: the DeleteFieldLevelEncryptionProfile handler's precondition check — the If-Match comparison must run against the stored resource version before any state mutation, and must short-circuit to the modeled PreconditionFailed error rather than falling through to the delete.
Traces: cfetag-field-level-encryption-profile-etag-chain#task#5 (expected 412, got 204).

Cause 2 of 3: the same chain's subsequent delete with the current ETag is then answered 404 instead of 204.
AWS: once the stale-If-Match delete has been refused with 412 the profile still exists, so the follow-up DeleteFieldLevelEncryptionProfile carrying the resource's current ETag succeeds with HTTP 204 and an empty body (Smithy: http code 204, output Unit).
Emulator: answers HTTP 404, because the earlier wrongly-accepted delete already removed the profile, so the correctly-conditioned delete finds nothing to delete.
Why: the conventional AWS sequence — attempt, get 412, re-read the ETag, retry conditionally — cannot complete against the emulator: the retry that AWS answers 204 reports the resource as absent. A caller cannot distinguish "someone else deleted it" from "my own earlier refused call deleted it", so cleanup and teardown logic reports spurious failures.
Fix: the same precondition check in the DeleteFieldLevelEncryptionProfile handler — refusing before mutating state is what keeps the resource available for the conditional retry; no separate change is needed once Cause 1 is gated.
Traces: cfetag-field-level-encryption-profile-etag-chain#task#6 (expected 204, got 404).

Cause 3 of 3: DeletePublicKey accepts a stale If-Match instead of refusing with PreconditionFailed.
AWS: Smithy: DeletePublicKey input member IfMatch is bound to the If-Match header; modeled errors include PreconditionFailed @httpError 412, InvalidIfMatchVersion (400), NoSuchPublicKey (404) and PublicKeyInUse (409); success is HTTP 204. A non-current If-Match leaves the public key in place and is answered 412 with Type 'Sender', Code 'PreconditionFailed' and the same fixed sentence "The request failed because it didn't meet the preconditions in one or more request-header fields.", in the REST-XML ErrorResponse envelope with Content-Length set.
Emulator: answers HTTP 204 with an empty body and no Content-Length, deleting the public key.
Why: identical loss of optimistic-concurrency protection on a second resource type, which shows the gap is in the shared conditional-request path rather than in one handler: any conditional delete in this service is effectively unconditional, so a client's ETag is decorative.
Fix: the DeletePublicKey handler's precondition check, and the shared If-Match/ETag evaluation the CloudFront delete handlers call into, so every operation modeling PreconditionFailed enforces it.
Traces: cfetag-public-key-etag-chain#task#6 (expected 412, got 204).

Related open issue, which does NOT cover these: 20261121T221736Z-cloudfront-s3-the-trust-store-stale-if-match.md reports a trust-store stale-If-Match refusal whose message is HTTP-generic — there the refusal happens and only the wording is wrong, whereas here no refusal happens at all and the resource is destroyed. 20261118T213617Z-cloudfront-public-key-lifecycle-refuses-creates-and-deletes.md covers other public-key lifecycle divergences but not the ignored precondition.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout:

1) cfetag-field-level-encryption-profile-etag-chain#task#5 DeleteFieldLevelEncryptionProfile (stale If-Match)
   expected status 412, actual 204
   expected body {'ErrorResponse': {'Error': {'Type': 'Sender', 'Code': 'PreconditionFailed', 'Message': "The request failed because it didn't meet the preconditions in one or more request-header fields."}, 'RequestId': <per-run>}}
   actual body {} (empty_body); $headers.content-length missing (expected present, got None)

2) cfetag-field-level-encryption-profile-etag-chain#task#6 DeleteFieldLevelEncryptionProfile (current ETag)
   expected status 204, actual 404 — the resource was already gone because step task#5 deleted it

3) cfetag-public-key-etag-chain#task#6 DeletePublicKey (stale If-Match)
   expected status 412, actual 204
   expected body {'ErrorResponse': {'Error': {'Type': 'Sender', 'Code': 'PreconditionFailed', 'Message': "The request failed because it didn't meet the preconditions in one or more request-header fields."}, 'RequestId': <per-run>}}
   actual body {} (empty_body); $headers.content-length missing

Model evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31 Smithy model): DeleteFieldLevelEncryptionProfile and DeletePublicKey each bind IfMatch to the If-Match header and declare PreconditionFailed with httpError 412; PreconditionFailed is documented "The precondition in one or more of the request fields evaluated to false." Success code for both is 204.

Fixed vs variable message text: for PreconditionFailed the whole sentence is fixed for the error class — no identifier, name, limit or member name is substituted in. RequestId in the envelope is a per-run value and is not asserted.

## Steps to Reproduce
1. Replay the wire corpus against the emulator (replay_wire_corpus on this checkout).
2. Fixture cfetag-field-level-encryption-profile-etag-chain: create an FLE profile, read it to capture its ETag, update it so that ETag is no longer current, then call DeleteFieldLevelEncryptionProfile with If-Match set to the now-stale ETag (step task#5). Observe HTTP 204 and an empty body instead of 412 with the PreconditionFailed ErrorResponse envelope.
3. In the same chain, call DeleteFieldLevelEncryptionProfile again with the profile's current ETag (step task#6). Observe HTTP 404 instead of 204, because step 2's delete already removed the profile.
4. Fixture cfetag-public-key-etag-chain: create a public key, capture its ETag, change it, then call DeletePublicKey with the stale If-Match (step task#6). Observe HTTP 204 instead of 412 PreconditionFailed.
