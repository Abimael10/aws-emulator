Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.cf-response-headers-policy-adds-and-overrides-headers-at-edge cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.cf-response-headers-policy-adds-and-overrides-headers-at-edge|run#29|AssertionError|assert headers.get("x-amz-meta-passthrough") == "from-origin", (
Issue-Key: cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected|run#7|AssertionError|assert c_malformed_body == "EntityNotFound", c_malformed_body
Issue-Commit: 743af1aef2870b34c6c9092ae87b31cfc860bafb

# cloudfront: 2 net-new causes — a response-headers-policy custom header with Override=false still replaces the origin's value at the edge, and UpdateAnycastIpList validates the body before resolving a nonexistent Id

> _Note: this issue was identified against the code base at commit `743af1aef2870b34c6c9092ae87b31cfc860bafb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CloudFront edge data-plane response (GET https://<distribution DomainName>/<path>); UpdateAnycastIpList

## Description
TWO net-new root causes in the cloudfront service; a fixer must close BOTH before submitting. Neither is carried by an open issue's replay key.

Cause 1 of 2: a ResponseHeadersPolicy CustomHeadersConfig entry with Override=false replaces the value the origin sent for that same header.
AWS: Smithy: ResponseHeadersPolicyCustomHeader.Override is documented "A Boolean that determines whether CloudFront overrides a response header with the same name received from the origin with the header specified here." So for an entry with Override=true the policy's Value replaces the origin's value for that header name; for an entry with Override=false, when the origin response ALREADY carries that header name, CloudFront leaves the origin's value in place and does not inject the policy's Value. Recorded behavior: with an S3 origin whose object metadata produces x-amz-meta-conflict: from-origin and x-amz-meta-passthrough: from-origin, and a policy declaring {x-fidelity: edge, Override=true}, {x-amz-meta-conflict: edge, Override=true}, {x-amz-meta-passthrough: edge, Override=false}, the viewer response carries x-fidelity: edge, x-amz-meta-conflict: edge and x-amz-meta-passthrough: from-origin.
Emulator: the pure addition and the Override=true replacement are now correct, but the Override=false header is also replaced — the viewer receives x-amz-meta-passthrough: 'edge' where the origin sent 'from-origin'. The Override flag is effectively ignored and every policy custom header is applied unconditionally.
Why: a client that relies on the documented arbitration gets the wrong value for any header the origin owns. Override=false is exactly how a caller declares "the origin is authoritative for this header, inject only a default": on the emulator the origin's value (a content type, a cache directive, object metadata, a correlation id) is silently discarded, so a header-precedence contract that works in production fails offline and vice-versa.
Fix: the edge response pipeline's response-headers-policy application step — when applying CustomHeadersConfig (and the equivalent Override/OriginOverride flags of SecurityHeadersConfig and CorsConfig), consult the per-entry Override flag against the headers already present on the origin response and keep the origin's value when Override is false.
Traces: cloudfront.cf-response-headers-policy-adds-and-overrides-headers-at-edge. Related but NOT covering it: 20261110T105638Z-cloudfront-10-net-new-causes-in-the-now.md (its Cause 1 carries the replay key for the assertion that the policy's custom header never reached the viewer at all; the policy IS applied now, and the check fails further on, at the Override=false arbitration).

Cause 2 of 2: UpdateAnycastIpList validates the request body before resolving the target, so a nonexistent Id is refused InvalidArgument instead of EntityNotFound.
AWS: Smithy: UpdateAnycastIpList (PUT /2020-05-31/anycast-ip-list/{Id}) models both EntityNotFound (httpError 404, doc "The entity was not found.") and InvalidArgument (httpError 400). Recorded behavior: the operation resolves its target FIRST, so a well-formed but nonexistent anycast-ip-list Id is refused with error code EntityNotFound / HTTP 404 both for a malformed body (IpamCidrConfigs with Cidr '300.1.1.0/24', an unparseable CIDR) and for a well-formed body (IpAddressType=ipv4). The fixed text of the message is AWS's entity-not-found wording and its variable part is the Id from the request path; the recording asserts the error CODE and status, not the message text.
Emulator: the malformed-body call is refused with error code InvalidArgument (HTTP 400) — the body is validated before the Id is looked up, so the existence failure is masked by a 400. The positive control (ListAnycastIpLists) succeeds, so the feature is reachable and this is not a blanket refusal.
Why: a caller cannot distinguish "this list does not exist" from "this body is bad". Retry and reconciliation logic keyed on EntityNotFound/404 (recreate the resource) instead takes the 400 InvalidArgument path (treat the request as permanently bad), so an Id drift is reported as a client bug and never healed.
Fix: UpdateAnycastIpList's handler — resolve {Id} (and emit EntityNotFound/404) before validating IpamCidrConfigs/IpAddressType, matching the order the recording shows.
Traces: cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected. Related but NOT covering it: 20261102T072100Z-cloudfront-three-refusal-divergences-oversized-tenant-invalidation-batch.md (carries the key for cloudfront.update-anycast-ip-list-invalid-argument-rejected, a different check).

## Evidence
Cause 1 — check cloudfront.cf-response-headers-policy-adds-and-overrides-headers-at-edge:
  AssertionError: Override=false wrongly replaced the origin's value: 'edge' (origin sent 'from-origin')
  Expected (AWS): x-amz-meta-passthrough: from-origin (the origin's value kept, because that custom-header entry has Override=false), alongside x-fidelity: edge and x-amz-meta-conflict: edge (both Override=true).
  Actual (emulator): x-amz-meta-passthrough: edge.
  The two preceding assertions (pure addition, Override=true replacement) passed, so the divergence is isolated to the Override=false branch. Bucket names, distribution ids and policy ids in the check are per-run values, not requirements.

Cause 2 — check cloudfront.update-anycast-ip-list-malformed-ip-invalid-argument-rejected:
  AssertionError: InvalidArgument   (assert c_malformed_body == "EntityNotFound")
  Request: UpdateAnycastIpList Id='IAL1A2B3C4D5E6' (nonexistent), IfMatch='ETVPDKIKX0DER', IpamCidrConfigs=[{Cidr: '300.1.1.0/24', IpamPoolArn: arn:aws:ec2:...:ipam-pool/...}].
  Expected (AWS): error code EntityNotFound, HTTP 404.
  Actual (emulator): error code InvalidArgument, HTTP 400.
  The Id and IfMatch literals are the recording's values filling the template; the requirement is that target resolution precedes body validation.

## Steps to Reproduce
Cause 1:
1. Create an S3 bucket and PutObject with Metadata={conflict: from-origin, passthrough: from-origin} so the origin response carries x-amz-meta-conflict and x-amz-meta-passthrough.
2. CreateResponseHeadersPolicy with CustomHeadersConfig items: {Header: x-fidelity, Value: edge, Override: true}, {Header: x-amz-meta-conflict, Value: edge, Override: true}, {Header: x-amz-meta-passthrough, Value: edge, Override: false}.
3. CreateDistribution over that bucket with an OAC origin, the managed CachingDisabled cache policy and that ResponseHeadersPolicyId on the default behavior; grant the bucket policy to cloudfront.amazonaws.com; wait for Status=Deployed.
4. GET https://<DomainName>/<key> -> x-amz-meta-passthrough is 'edge' instead of the origin's 'from-origin'.

Cause 2:
1. ListAnycastIpLists (positive control, succeeds).
2. UpdateAnycastIpList with a well-formed but nonexistent Id, any IfMatch, and IpamCidrConfigs carrying an unparseable CIDR -> InvalidArgument/400 instead of EntityNotFound/404.
3. Repeat with a well-formed body (IpAddressType=ipv4) -> this one already answers EntityNotFound, showing the ordering is only wrong when the body is invalid.

Or replay the check ids in trace_ids.

## Resolution

Cause 2 is FIXED here; cause 1 was already fixed on the mainline this branch forked from and
its check replays green.

- Cause 1 ALREADY FIXED: the edge applies each response-headers-policy header under its own
  `Override` against the headers the answer already carries, so a custom-header entry with
  `Override` false leaves the origin's value in place (`service::edge::response_headers::put`,
  covered end to end by `tests/all/edge_deployed_distribution_effects.rs` and
  `tests/all/edge_response_headers_policy.rs`).
- Cause 2 FIXED: the operations addressed by an Anycast static IP list `Id` read of the value
  only whether it is written in a form CloudFront writes an identifier in at all — under a
  namespace label, or in the alphabet an entity identifier is written in — and resolve every
  such value against the store, so an identifier of the entity alphabet that no list answers
  to is `EntityNotFound` (404) rather than `InvalidArgument` (400). The ordering the recording
  shows follows from that: the target is resolved before anything the body submits is read.

