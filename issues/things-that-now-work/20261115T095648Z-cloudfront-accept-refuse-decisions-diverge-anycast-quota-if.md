Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-anycast-ip-list-create-limit-exceeded
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-anycast-ip-list-create-limit-exceeded cloudfront-anycast-ip-list-error-paths cloudfront-anycast-ip-list-lifecycle cloudfront-fle-config-not-found-and-invalid-ifmatch-errors cloudfront-icc-distribution-identifier-mistakes
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreateAnycastIpList|4xx->2xx|$.ErrorResponse
Issue-Key: cloudfront|CreateAnycastIpList|4xx->2xx|status
Issue-Key: cloudfront|DeletePublicKey|2xx->4xx|status
Issue-Key: cloudfront|CreateInvalidation|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: ee92bfca9604c8b68a1eb4b8dbd89b919c3f4b6a

# CloudFront accept/refuse decisions diverge: anycast quota, If-Match ETag, invalidation id format

> _Note: this issue was identified against the code base at commit `ee92bfca9604c8b68a1eb4b8dbd89b919c3f4b6a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAnycastIpList

## Description
Three net-new wire divergence clusters, one shared fault: the CloudFront handlers decide whether to accept or refuse a request on the wrong grounds. One request AWS always refuses is accepted, one request AWS always accepts is refused, and one refusal is reported with the wrong reason. All three are decided before any resource state is read or written, so they belong to the request-admission path of their operations (quota check, conditional-request check, identifier-format check).

Cause 1 of 3: CreateAnycastIpList is accepted instead of refused on the account quota.
AWS: Smithy: CreateAnycastIpList http {method POST, uri /2020-05-31/anycast-ip-list, code 202}; modelled errors include EntityLimitExceeded -> httpError 400 ("The entity limit has been exceeded."). Recorded behavior: every CreateAnycastIpList in the corpus — including the ones recorded as the first step of a plain lifecycle fixture — is refused with HTTP 400 and ErrorResponse/Error with Type 'Sender', Code 'EntityLimitExceeded' and the message 'To request a limit increase for your Anycast static IP list, contact AWS Support: https://us-east-1.console.aws.amazon.com/servicequotas/home/services/cloudfront/quotas/L-6A19EDFD'. That message is entirely fixed text — it has no variable parts taken from the request; the quota code L-6A19EDFD and the Service Quotas console URL are part of the wording. (Hypothesis: the anycast static IP list quota is zero until a limit increase is granted, so the create can never succeed for an account that has not requested one.) The RequestId member of the recorded ErrorResponse is per-run and is not a requirement.
Emulator: answers HTTP 202 with a materialised AnycastIpList body and no ErrorResponse at all, i.e. it creates the resource.
Why: a client is told it owns anycast static IP addresses it does not own. It then wires those addresses into distribution configs, DNS or firewall allow-lists, and the quota refusal — the one signal that tells an operator to open a limit-increase request — is never surfaced. Code paths that handle EntityLimitExceeded are never exercised, and the emulator's answer is a success status for an operation AWS fails 100% of the time in this account shape.
Fix: CreateAnycastIpList's handler/admission check — refuse the create with the modelled EntityLimitExceeded (400) and AWS's limit-increase wording before allocating the list, rather than returning 202.
Traces: cloudfront-anycast-ip-list-create-limit-exceeded#task#1 (representative; cluster also covers cloudfront-anycast-ip-list-error-paths#task#1 and cloudfront-anycast-ip-list-lifecycle#task#1).

Cause 2 of 3: DeletePublicKey refuses a valid, current If-Match ETag with 412.
AWS: Smithy: DeletePublicKey http {method DELETE, uri /2020-05-31/public-key/{Id}, code 204}; input member IfMatch is bound to the If-Match header; modelled errors PreconditionFailed -> 412, InvalidIfMatchVersion -> 400, NoSuchPublicKey -> 404, PublicKeyInUse -> 409. When the If-Match header carries the public key's current ETag — the value the service itself returned in the ETag response header of the preceding create/get — the delete succeeds with HTTP 204 and an empty body. 412 PreconditionFailed is reserved for an ETag that does not match the entity's current version.
Emulator: answers HTTP 412 for the current ETag, so the public key cannot be deleted at all; the caller holds no other ETag than the one just rejected.
Why: the resource is undeletable. Teardown of a correctly created public key is impossible, every ETag-gated lifecycle around public keys wedges, and a client that (correctly) treats 412 as "refetch the ETag and retry" loops forever because the refetched ETag is the same rejected value.
Fix: the shared If-Match/ETag comparison used by CloudFront's mutating handlers, as reached from DeletePublicKey — compare against the entity's current ETag and only raise PreconditionFailed on a genuine mismatch. The open issue 20261029T212223Z-cloudfront-if-match-etag-handling-wrong-precondition-message.md describes this same family for other mutators, but it does NOT carry DeletePublicKey's replay key (cloudfront|DeletePublicKey|204->412|status) — there is no DeletePublicKey Issue-Key in it — so this cause is un-gated by that issue and is filed here.
Traces: cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#teardown#8.

Cause 3 of 3: CreateInvalidation reports a malformed DistributionId as a missing distribution.
AWS: Smithy: CreateInvalidation http {method POST, uri /2020-05-31/distribution/{DistributionId}/invalidation, code 201}; DistributionId is an httpLabel; modelled errors include NoSuchDistribution -> 404 (doc "The specified distribution does not exist."). Recorded behavior: when DistributionId is not a well-formed distribution id, AWS answers 404 whose Error.Message is the fixed text 'DistributionId format is invalid' — note AWS names the rejected input member (DistributionId) in this operation's wording rather than a generic "Id". The fixed text is the whole message; it has no variable parts.
Emulator: answers 404 (status and Error.Code match) with Error.Message 'The specified distribution does not exist.' — the not-found wording.
Why: the two refusals mean different things. A format refusal is permanent and tells the caller its own input is wrong; a not-found implies a well-formed id that is merely absent, so a client retries or polls for eventual consistency that will never arrive, and it cannot tell which request member to correct.
Fix: CreateInvalidation's input validation — validate DistributionId's format ahead of the state lookup and word the message with the member name. The open issue 20261105T231732Z-cloudfront-path-identifiers-are-not-format-validated-before.md discusses this condition in prose (its Cause 6) but carries only cloudfront|CreateInvalidation|4xx->5xx|... replay keys, not this cluster's cloudfront|CreateInvalidation|404->404|$.ErrorResponse.Error.Message, so replaying that issue's pointer does not exercise this step.
Traces: cloudfront-icc-distribution-identifier-mistakes#task#2 (cluster also covers #task#3).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 1303 steps, score 89.0%, 60 divergence clusters, all cloudfront). These are the three clusters not owned by an open issue's replay keys.

Cause 1 — cloudfront-anycast-ip-list-create-limit-exceeded#task#1, CreateAnycastIpList: expected status 400 with $.ErrorResponse = {Error: {Type 'Sender', Code 'EntityLimitExceeded', Message 'To request a limit increase for your Anycast static IP list, contact AWS Support: https://us-east-1.console.aws.amazon.com/servicequotas/home/services/cloudfront/quotas/L-6A19EDFD'}}; got status 202 and $.ErrorResponse missing (a created list instead). 3 affected steps across 3 fixtures. divergence_key service=cloudfront|CreateAnycastIpList|status=400->202|keys=$.ErrorResponse,status. The RequestId in the recorded error is per-run and not asserted as a requirement.

Cause 2 — cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#teardown#8, DeletePublicKey: expected status 204 (empty body); got status 412. divergence_key service=cloudfront|DeletePublicKey|status=204->412|keys=status. The ETag value itself is per-run; the requirement is that an If-Match carrying the ETag the emulator last returned for that public key is accepted.

Cause 3 — cloudfront-icc-distribution-identifier-mistakes#task#2, CreateInvalidation: $.ErrorResponse.Error.Message expected 'DistributionId format is invalid', got 'The specified distribution does not exist.' (both 404, same Error.Code). 2 affected steps (#task#2, #task#3). divergence_key service=cloudfront|CreateInvalidation|status=404->404|keys=$.ErrorResponse.Error.Message.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus.
3. Cause 1: replay fixture cloudfront-anycast-ip-list-create-limit-exceeded, step task#1 (representative) — POST /2020-05-31/anycast-ip-list with a valid Name and IpCount. Expect 400 EntityLimitExceeded with AWS's limit-increase message; the emulator answers 202 with an AnycastIpList. Same at cloudfront-anycast-ip-list-error-paths#task#1 and cloudfront-anycast-ip-list-lifecycle#task#1.
4. Cause 2: replay cloudfront-fle-config-not-found-and-invalid-ifmatch-errors, step teardown#8 — create a public key, read the ETag from the create/get response header, then DELETE /2020-05-31/public-key/{Id} with exactly that ETag as If-Match. Expect 204; the emulator answers 412.
5. Cause 3: replay cloudfront-icc-distribution-identifier-mistakes, steps task#2 and task#3 — POST /2020-05-31/distribution/{DistributionId}/invalidation with a DistributionId that is not a well-formed distribution id. Expect 404 with Error.Message 'DistributionId format is invalid'; the emulator answers 404 with 'The specified distribution does not exist.'
6. All three causes must close before submitting: the quota refusal, the accepted current-ETag delete, and the id-format message.

## Resolution

Cause 1 FIXED, cause 3 already correct in the current code base and now pinned by a suite,
cause 2 DECLINED as a per-run value.

- Cause 1 FIXED: how many Anycast static IP lists an account may hold is the value its
  adjustable Service Quotas quota for them is in force at, and that quota's AWS default is
  zero — which is why the service's own refusal of the create tells the caller to have it
  raised. The CloudFront create now reads the value in force out of the store Service Quotas
  serves `GetServiceQuota` from and records an approved `RequestServiceQuotaIncrease` into
  (`state::anycast_entitlement`), and refuses a create that would take the account past it
  with the modelled `EntityLimitExceeded` before anything is allocated. The Service Quotas
  catalog publishes that quota (`cloudfront` / `L-6A19EDFD`, default 0, adjustable,
  account-wide), so an account that asks for an increase may allocate up to the raised value.
  The three anycast fixtures replay end to end.
- Cause 3 FIXED (already implemented): an operation addressed by a resource identifier in its
  path reads that identifier's FORM before resolving it against the store and words the
  refusal with the model's name for the member it arrived in, keeping the absent-resource
  sentence for a value that could address one. `CreateInvalidation` answers a malformed
  `DistributionId` with 404 `NoSuchDistribution` / "DistributionId format is invalid", covered
  end to end by `tests/all/unreadable_path_identifiers.rs`.
- Cause 2 DECLINED: an `ETag` is a per-run, server-generated value. The emulator already
  compares an `If-Match` against the addressed entity's own current token; at the cited
  teardown the replay carries the token this emulator minted for the field-level encryption
  CONFIGURATION created earlier in the same fixture, not the profile's or the public key's, so
  answering 204 would mean accepting a version token issued to another resource — the very
  condition the issue states must be 412. The rule is pinned instead by
  `tests/all/if_match_is_the_entitys_own_version.rs`.
