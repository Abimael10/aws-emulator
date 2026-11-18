Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-public-key-encoded-key-size-and-algorithm
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-public-key-encoded-key-size-and-algorithm cloudfront-fle-config-not-found-and-invalid-ifmatch-errors cloudfront-key-group-config-bounds-and-member-dedup
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreatePublicKey|2xx->4xx|$.PublicKey
Issue-Key: cloudfront|CreatePublicKey|2xx->4xx|$headers.etag
Issue-Key: cloudfront|CreatePublicKey|2xx->4xx|$headers.location
Issue-Key: cloudfront|CreatePublicKey|2xx->4xx|status
Issue-Key: cloudfront|DeletePublicKey|2xx->4xx|status
Issue-Commit: e5b2a7530949298760223c07a5dc6decb6b73137

# CloudFront public-key lifecycle refuses creates and deletes AWS accepts

> _Note: this issue was identified against the code base at commit `e5b2a7530949298760223c07a5dc6decb6b73137`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePublicKey

## Description
THREE (3) distinct net-new causes on CloudFront's signed-URL public-key resource. They share one fault: the emulator refuses public-key requests that AWS accepts — one admission refusal on CreatePublicKey and two delete refusals on DeletePublicKey — so a caller can neither create nor dispose of a public key the real service handles. All three must be closed before this issue is submitted.

Replay reports all three unowned. The nearest open issues cover OTHER public-key behavior and do NOT cover the causes below: 20261118T074023Z-cloudfront-public-key-and-response-headers-policy-admission.md (its cause 1 is the OPPOSITE direction — CreatePublicKey admitting empty/unparsable/out-of-limits key material with 201 where AWS answers 400, on task#1-task#3 of the same fixture; causes 2-3 are Comment-bound message wording), 20261116T042606Z-cloudfront-getpublickey-reports-an-absent-public-key-id.md (GetPublicKey 404 wording) and 20261118T074023Z-cloudfront-key-group-bounds-member-dedup-and-etag.md (key-group admission, member dedup and key-group ETag, not the public-key delete).

=== Cause 1 of 3: CreatePublicKey rejects a well-formed non-RSA (ECDSA P-256) public key AWS accepts ===
AWS: Smithy: CreatePublicKey POST /2020-05-31/public-key, success code 201, output PublicKey as httpPayload plus Location and ETag response headers; input PublicKeyConfig @required httpPayload; PublicKeyConfig.EncodedKey is documented only as "The public key that you can use with signed URLs and signed cookies, or with field-level encryption" — the model states NO algorithm restriction, no key-size range and no pattern. Modeled errors are InvalidArgument (client, httpError 400, doc "An argument is invalid."), PublicKeyAlreadyExists (client, 409) and TooManyPublicKeys (client, 400). Recorded behavior shows AWS ACCEPTING a PEM-armoured SEC1/SPKI ECDSA P-256 public key (an EncodedKey whose body begins with the SPKI prefix for id-ecPublicKey / prime256v1): HTTP 201 carrying PublicKey with a generated Id, a CreatedTime, and the submitted PublicKeyConfig echoed back verbatim (CallerReference, Name, EncodedKey and Comment exactly as sent), plus the ETag and Location response headers. So the admission rule is: a parsable PEM public key is admitted regardless of whether its algorithm is RSA; AWS does not require RSA key material here.
Emulator: HTTP 400 — the whole success response is absent ($.PublicKey missing, no ETag header, no Location header). The emulator's EncodedKey check evidently accepts only RSA key material and treats any other algorithm as invalid argument.
Why: a caller that provisions an EC key for field-level encryption or for a trusted-key-group signer cannot create the resource at all. There is no id to put into a KeyGroup, so every downstream step (CreateKeyGroup, trusted-signer configuration on a distribution, signed-URL verification) is unreachable in the emulator while it works against AWS — the emulator is strictly more restrictive than the service it stands in for, which is the failure mode a client cannot work around.
Fix: CreatePublicKey's input validation in the cloudfront public-key handler — the EncodedKey admission check must decode the PEM/SPKI envelope and accept the key when it parses, rather than gating on an RSA-only algorithm test. Note this is the SAME validation routine as cause 1 of 20261118T074023Z-cloudfront-public-key-and-response-headers-policy-admission.md but the opposite error: that one is too permissive about unusable key material, this one is too strict about a usable non-RSA key; a fix that only tightens the check will not close this cause.
Traces: cloudfront-public-key-encoded-key-size-and-algorithm#task#5 (representative).

=== Cause 2 of 3: DeletePublicKey answers 412 PreconditionFailed to a delete carrying the ETag the emulator itself issued ===
AWS: Smithy: DeletePublicKey DELETE /2020-05-31/public-key/{Id}, success code 204 with an empty body (output Unit); input Id @httpLabel @required and IfMatch bound to the If-Match request header. Modeled errors: AccessDenied (403), InvalidIfMatchVersion (400), NoSuchPublicKey (404), PreconditionFailed (client, httpError 412, doc "The precondition in one or more of the request fields evaluated to false.") and PublicKeyInUse (409). The rule is that PreconditionFailed is raised ONLY when the If-Match value does not equal the resource's current ETag; when the caller presents the ETag most recently published for that public key, the delete succeeds with 204 and no body. The recorded teardown deletes the public key with the ETag handed back by the preceding create/read and AWS answers 204.
Emulator: HTTP 412 (PreconditionFailed) instead of 204 — the delete is refused even though the If-Match value is the one the emulator published for that resource. Hypothesis: the emulator's public-key ETag is recomputed or bumped by requests that did not change the resource (the fixture's preceding steps are deliberately-failing not-found and invalid-If-Match probes), so the version the caller holds no longer equals the stored one.
Why: the resource becomes undeletable — a caller holding the current ETag is told its precondition failed and has no newer version to retry with, so public keys leak and the emulator's state drifts permanently from what the client believes it created. Teardown of any signed-URL fixture cannot complete.
Fix: the public-key state's ETag/version bookkeeping and DeletePublicKey's If-Match comparison in the cloudfront public-key handler — an unchanged resource must keep the ETag it last published, and a failed or read-only request must not advance it.
Traces: cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#teardown#8.

=== Cause 3 of 3: DeletePublicKey answers 409 PublicKeyInUse after every referencing key group is gone ===
AWS: Smithy: DeletePublicKey DELETE /2020-05-31/public-key/{Id}, success 204 empty body; PublicKeyInUse (client, httpError 409, doc "The specified public key is in use."). The rule is that PublicKeyInUse is raised only while the public key is still referenced — i.e. it is a member of an existing key group (or of a field-level-encryption profile); once those referants are deleted the key is free and the delete returns 204. The recorded teardown deletes the key groups first and AWS then answers 204 to DeletePublicKey.
Emulator: HTTP 409 (PublicKeyInUse) instead of 204 — the emulator still believes the key is referenced after its key groups have been deleted. Hypothesis: the emulator's in-use accounting for public keys is never decremented on key-group delete, and the duplicate/phantom key-group membership this same fixture exposes (a key group whose Items retain the member id twice, and an id the update was supposed to remove) inflates the reference set further.
Why: the public key is permanently undeletable, so a client cannot clean up its signed-URL key material and the modeled success path of DeletePublicKey is unreachable. A 409 is also a conflict a client is expected to resolve by removing referants — here there are none left to remove, so the caller loops or gives up. This is a distinct handler from cause 2 (reference accounting, not If-Match) and must be verified separately.
Fix: the public-key reference/in-use accounting in the cloudfront key-group and public-key state — releasing a key-group membership (on UpdateKeyGroup and DeleteKeyGroup) must drop the reference, and DeletePublicKey's in-use test must consult live referants rather than a counter. The membership-duplication defect that feeds it is tracked separately in 20261118T074023Z-cloudfront-key-group-bounds-member-dedup-and-etag.md, which does NOT cover this delete refusal.
Traces: cloudfront-key-group-config-bounds-and-member-dedup#teardown#14.

## Evidence
Cause 1 — CreatePublicKey, fixture cloudfront-public-key-encoded-key-size-and-algorithm step task#5: expected status 201 with $.PublicKey present (Id, CreatedTime and PublicKeyConfig echoing the submitted CallerReference, Name, the PEM ECDSA P-256 EncodedKey and Comment) plus $headers.etag and $headers.location; actual status 400 with $.PublicKey, $headers.etag and $headers.location all missing. Replay divergence_key: service=cloudfront|CreatePublicKey|status=201->400|keys=$.PublicKey,$headers.etag,$headers.location,status.

Cause 2 — DeletePublicKey, fixture cloudfront-fle-config-not-found-and-invalid-ifmatch-errors step teardown#8: expected status 204 (empty body); actual status 412. Replay divergence_key: service=cloudfront|DeletePublicKey|status=204->412|keys=status.

Cause 3 — DeletePublicKey, fixture cloudfront-key-group-config-bounds-and-member-dedup step teardown#14: expected status 204 (empty body); actual status 409. Replay divergence_key: service=cloudfront|DeletePublicKey|status=204->409|keys=status.

In all three the diverging HTTP status is the requirement: 201 (cause 1) and 204 (causes 2 and 3) are the success codes bound in the Smithy model for CreatePublicKey and DeletePublicKey respectively. The refusal codes the emulator substitutes (400 InvalidArgument, 412 PreconditionFailed, 409 PublicKeyInUse) are all modeled errors of those operations, but none of their preconditions holds for these requests.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture cloudfront-public-key-encoded-key-size-and-algorithm up to step task#5 — CreatePublicKey with a PublicKeyConfig whose EncodedKey is a PEM-armoured ECDSA P-256 (prime256v1) public key and whose Comment describes it. Expect 201 with the PublicKey payload, ETag and Location; observe 400 with no body. (Steps task#1-task#3 of this fixture are the separate over-permissive admission cause already filed under 20261118T074023Z-cloudfront-public-key-and-response-headers-policy-admission.md — do not confuse them.)
3. Cause 2: replay fixture cloudfront-fle-config-not-found-and-invalid-ifmatch-errors in full, including step teardown#8 — DeletePublicKey for the public key created by the fixture, sending If-Match set to the ETag the emulator last published for it. Expect 204 with an empty body; observe 412 PreconditionFailed.
4. Cause 3: replay fixture cloudfront-key-group-config-bounds-and-member-dedup in full, including step teardown#14 — after the fixture's key groups have been deleted, DeletePublicKey for the public key they referenced. Expect 204 with an empty body; observe 409 PublicKeyInUse.
5. Confirm with smithy_query(query="operation", operation="CreatePublicKey,DeletePublicKey", model_path="services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json") that the bound success codes are 201 and 204, and that PreconditionFailed/PublicKeyInUse/InvalidArgument are conditional errors, not the default answers.
