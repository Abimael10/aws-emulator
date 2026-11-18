Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-public-key-encoded-key-size-and-algorithm
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-public-key-encoded-key-size-and-algorithm cloudfront-public-key-name-and-comment-bounds cloudfront-response-headers-policy-quota-and-remove-header-refused cloudfront-update-managed-response-headers-policy-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreatePublicKey|4xx->2xx|$.ErrorResponse
Issue-Key: cloudfront|CreatePublicKey|4xx->2xx|status
Issue-Key: cloudfront|CreatePublicKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|UpdatePublicKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|CreateResponseHeadersPolicy|4xx->2xx|$.ErrorResponse
Issue-Key: cloudfront|CreateResponseHeadersPolicy|4xx->2xx|status
Issue-Key: cloudfront|CreateResponseHeadersPolicy|2xx->4xx|$.ResponseHeadersPolicy
Issue-Key: cloudfront|CreateResponseHeadersPolicy|2xx->4xx|$headers.etag
Issue-Key: cloudfront|CreateResponseHeadersPolicy|2xx->4xx|$headers.location
Issue-Key: cloudfront|CreateResponseHeadersPolicy|2xx->4xx|status
Issue-Key: cloudfront|GetResponseHeadersPolicy|2xx->2xx|$.ResponseHeadersPolicy.LastModifiedTime
Issue-Key: cloudfront|GetResponseHeadersPolicy|2xx->2xx|$.ResponseHeadersPolicy.ResponseHeadersPolicyConfig.SecurityHeadersConfig.ContentTypeOptions.Override
Issue-Commit: 9fdcce976c9d3a333330e6c021629b669a23ff81

# CloudFront public key and response-headers-policy admission and readback diverge from AWS

> _Note: this issue was identified against the code base at commit `9fdcce976c9d3a333330e6c021629b669a23ff81`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreatePublicKey

## Description
SIX (6) distinct net-new causes across CloudFront's signed-URL public keys and its response-headers policies: admission checks that are absent, refusal wordings the emulator authors itself, and a managed-policy readback that reports the wrong values. All six must be closed before this issue is submitted. Replay reports each of them unowned; the nearest open issues (20261117T151821Z-cloudfront-6-more-net-new-causes-staging-public.md, 20261114T135924Z-cloudfront-2-net-new-causes-a-response-headers.md and 20261116T042606Z-cloudfront-getpublickey-reports-an-absent-public-key-id.md) cover other public-key/response-headers-policy behavior, NOT the causes below.

=== Cause 1 of 6: CreatePublicKey admits an EncodedKey (and a Name) outside the service's bounds ===
AWS: Smithy: CreatePublicKey POST /2020-05-31/public-key, success 201; input PublicKeyConfig @required httpPayload with CallerReference, Name and EncodedKey all @required; modeled error InvalidArgument (errorKind client, httpError 400, doc "An argument is invalid."). The recorded AWS response when EncodedKey is empty, is not a parsable RSA public key, or is of a key size outside the accepted range is HTTP 400, Error.Type 'Sender', Code 'InvalidArgument', Message 'Your request contains empty/invalid/out of limits RSA Encoded Key'. The whole message is fixed text — AWS interpolates nothing from the request, so the same sentence covers the empty, malformed and out-of-limits cases.
Emulator: HTTP 201 with no ErrorResponse — the public key resource is created from the unusable key material.
Why: a caller gets a public key id that real CloudFront would never have issued, so key groups, trusted-signer configuration and signed-URL verification are all built on a key AWS rejects; the failure surfaces much later (or only in production) instead of at CreatePublicKey.
Fix: CreatePublicKey's input validation in the cloudfront public-key handler — decode and bound-check PublicKeyConfig.EncodedKey (and the Name bound) before creating state.
Traces: cloudfront-public-key-encoded-key-size-and-algorithm#task#1 (representative), #task#2, #task#3; cloudfront-public-key-name-and-comment-bounds#task#1, #task#2.

=== Cause 2 of 6: CreatePublicKey's over-long-Comment refusal uses emulator-authored prose ===
AWS: Smithy: PublicKeyConfig.Comment is an optional string documented "A comment to describe the public key. The comment cannot be longer than 128 characters."; modeled error InvalidArgument (client, 400). Recorded AWS response: HTTP 400, Error.Type 'Sender', Code 'InvalidArgument', Message 'The parameter Comment is too big.' The entire message is fixed text apart from the parameter label Comment; AWS does NOT quote the limit or the submitted length.
Emulator: HTTP 400 / InvalidArgument with Message 'An argument is invalid: the comment cannot be longer than 128 characters but the request submits 129' — emulator-authored wording that additionally leaks the submitted length.
Why: the status and code are right but the text is not AWS's, so clients and operators matching the refusal do not recognize it; the emulator also sets an expectation (a length-bearing message) AWS never meets.
Fix: CreatePublicKey's validation message template — use AWS's 'The parameter <Name> is too big.' form for an over-bound Comment.
Traces: cloudfront-public-key-name-and-comment-bounds#task#3.

=== Cause 3 of 6: UpdatePublicKey's over-long-Comment refusal uses the same emulator-authored prose ===
AWS: Smithy: UpdatePublicKey PUT /2020-05-31/public-key/{Id}/config, success 200; errors include InvalidArgument (client, 400). Recorded AWS response: HTTP 400, Error.Type 'Sender', Code 'InvalidArgument', Message 'The parameter Comment is too big.' — same fixed text as cause 2.
Emulator: HTTP 400 / InvalidArgument with Message 'An argument is invalid: the comment cannot be longer than 128 characters but the request submits 129'.
Why: same text-matching break as cause 2, on the update path — a caller editing an existing public key's comment sees a refusal it cannot classify.
Fix: UpdatePublicKey's validation message template (share the parameter-too-big template with CreatePublicKey rather than re-wording it). This is a separate handler from cause 2 and must be verified separately.
Traces: cloudfront-public-key-name-and-comment-bounds#task#5.

=== Cause 4 of 6: CreateResponseHeadersPolicy does not enforce the custom-headers quota ===
AWS: Smithy: CreateResponseHeadersPolicy POST /2020-05-31/response-headers-policy, success 201; ResponseHeadersPolicyConfig.CustomHeadersConfig targets ResponseHeadersPolicyCustomHeadersConfig (Quantity @required, Items a ResponseHeadersPolicyCustomHeaderList); modeled error TooManyCustomHeadersInResponseHeadersPolicy (errorKind client, httpError 400, doc "The number of custom headers in the response headers policy exceeds the maximum."). Recorded AWS response when CustomHeadersConfig carries more headers than the per-policy quota: HTTP 400, Error.Type 'Sender', Code 'TooManyCustomHeadersInResponseHeadersPolicy', Message 'Your request contains more headers in the Response Headers Policy than are allowed.' The whole message is fixed text.
Emulator: HTTP 201 with no ErrorResponse — the over-quota policy is created.
Why: the emulator admits a response-headers policy AWS refuses, so a client never learns it has exceeded the custom-header quota and an association built on that policy succeeds in the emulator and fails against AWS; the modeled quota error is unreachable.
Fix: CreateResponseHeadersPolicy's validation — count CustomHeadersConfig.Items against the per-policy custom-header quota and raise TooManyCustomHeadersInResponseHeadersPolicy before creating state.
Traces: cloudfront-response-headers-policy-quota-and-remove-header-refused#task#1 (representative), #task#4.

=== Cause 5 of 6: CreateResponseHeadersPolicy answers 409 to a policy creation AWS accepts ===
AWS: Smithy: CreateResponseHeadersPolicy success 201 with ResponseHeadersPolicy httpPayload plus Location and ETag response headers; ResponseHeadersPolicyAlreadyExists (client, 409, doc "A response headers policy with this name already exists. You must provide a unique name.") applies only when the submitted ResponseHeadersPolicyConfig.Name is already taken. The recording shows HTTP 201 carrying ResponseHeadersPolicy with Id, LastModifiedTime and the submitted ResponseHeadersPolicyConfig (Comment, Name, CustomHeadersConfig with its Quantity and Items), plus the ETag and Location headers.
Emulator: HTTP 409 with no ResponseHeadersPolicy body, no ETag header and no Location header, for a name the recording establishes as free. Hypothesis: the emulator reserved the name during the preceding over-quota request that cause 4 wrongly admitted, so a request AWS never created still occupies the name.
Why: a client is told its policy name is taken when it is not — it cannot create the policy at all, and retries or name-mangles around a conflict that does not exist; it also loses the Location/ETag it needs for the subsequent conditional update.
Fix: CreateResponseHeadersPolicy's name-uniqueness check and the response-headers-policy state write — a refused create must not leave the name reserved.
Traces: cloudfront-response-headers-policy-quota-and-remove-header-refused#task#2 (representative), #task#5.

=== Cause 6 of 6: GetResponseHeadersPolicy misreports an AWS managed policy's LastModifiedTime and ContentTypeOptions.Override ===
AWS: Smithy: GetResponseHeadersPolicy GET /2020-05-31/response-headers-policy/{Id}, 200, output ResponseHeadersPolicy httpPayload + ETag; ResponseHeadersPolicyContentTypeOptions.Override is a @required boolean deciding whether CloudFront overrides the origin's X-Content-Type-Options header. For the AWS-managed response-headers policies the model states no values, so recorded behavior only: the recording shows ResponseHeadersPolicy.LastModifiedTime as the epoch timestamp '1970-01-01T00:00:00Z' (a fixed constant AWS publishes for managed policies, not a per-run clock value) and SecurityHeadersConfig.ContentTypeOptions.Override as 'true'.
Emulator: HTTP 200 but LastModifiedTime '2020-05-31T00:00:00Z' and SecurityHeadersConfig.ContentTypeOptions.Override 'false'.
Why: Override is the policy's actual behavioral contract — a client reading the managed policy concludes CloudFront will NOT override the origin's X-Content-Type-Options when AWS's managed policy says it will, so security-header expectations and any config diff/drift check against the managed policy are wrong. The managed policy's LastModifiedTime is a fixed published constant, so a client caching or comparing on it also diverges.
Fix: the built-in/managed response-headers-policy definitions in the cloudfront state seed, read by the GetResponseHeadersPolicy serializer.
Traces: cloudfront-update-managed-response-headers-policy-rejected#task#4 (representative), #task#6.

## Evidence
Cause 1 — CreatePublicKey (cloudfront-public-key-encoded-key-size-and-algorithm#task#1, #task#2, #task#3; cloudfront-public-key-name-and-comment-bounds#task#1, #task#2): expected status 400 with ErrorResponse {Error:{Type:'Sender', Code:'InvalidArgument', Message:'Your request contains empty/invalid/out of limits RSA Encoded Key'}}; actual status 201, ErrorResponse missing.
Cause 2 — CreatePublicKey over-long Comment (cloudfront-public-key-name-and-comment-bounds#task#3): expected 400 Message 'The parameter Comment is too big.'; actual 400 Message 'An argument is invalid: the comment cannot be longer than 128 characters but the request submits 129'.
Cause 3 — UpdatePublicKey over-long Comment (cloudfront-public-key-name-and-comment-bounds#task#5): expected 400 Message 'The parameter Comment is too big.'; actual 400 Message 'An argument is invalid: the comment cannot be longer than 128 characters but the request submits 129'.
Cause 4 — CreateResponseHeadersPolicy over quota (cloudfront-response-headers-policy-quota-and-remove-header-refused#task#1, #task#4): expected status 400 with ErrorResponse {Error:{Type:'Sender', Code:'TooManyCustomHeadersInResponseHeadersPolicy', Message:'Your request contains more headers in the Response Headers Policy than are allowed.'}}; actual status 201, ErrorResponse missing.
Cause 5 — CreateResponseHeadersPolicy, free name (cloudfront-response-headers-policy-quota-and-remove-header-refused#task#2, #task#5): expected status 201 with $.ResponseHeadersPolicy (Id, LastModifiedTime, ResponseHeadersPolicyConfig{Comment, Name, CustomHeadersConfig{Quantity, Items}}) plus $headers.etag and $headers.location; actual status 409 with $.ResponseHeadersPolicy, $headers.etag and $headers.location all missing.
Cause 6 — GetResponseHeadersPolicy, managed policy (cloudfront-update-managed-response-headers-policy-rejected#task#4, #task#6): expected $.ResponseHeadersPolicy.LastModifiedTime '1970-01-01T00:00:00Z' and $.ResponseHeadersPolicy.ResponseHeadersPolicyConfig.SecurityHeadersConfig.ContentTypeOptions.Override 'true'; actual '2020-05-31T00:00:00Z' and 'false'.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture cloudfront-public-key-encoded-key-size-and-algorithm steps task#1-task#3: CreatePublicKey with an empty / unparsable / out-of-range-size PublicKeyConfig.EncodedKey -> expect 400 InvalidArgument 'Your request contains empty/invalid/out of limits RSA Encoded Key', observe 201.
3. Replay fixture cloudfront-public-key-name-and-comment-bounds: steps task#1-task#2 (out-of-bound Name/EncodedKey -> expect 400, observe 201); step task#3 (CreatePublicKey with a Comment longer than 128 characters -> compare the message); step task#5 (UpdatePublicKey with a Comment longer than 128 characters -> compare the message).
4. Replay fixture cloudfront-response-headers-policy-quota-and-remove-header-refused: steps task#1 and task#4 (CreateResponseHeadersPolicy whose CustomHeadersConfig exceeds the custom-header quota -> expect 400 TooManyCustomHeadersInResponseHeadersPolicy, observe 201); steps task#2 and task#5 (CreateResponseHeadersPolicy with an unused Name -> expect 201 + ResponseHeadersPolicy + ETag + Location, observe 409).
5. Replay fixture cloudfront-update-managed-response-headers-policy-rejected steps task#4 and task#6: GetResponseHeadersPolicy on an AWS managed response-headers policy -> compare LastModifiedTime and SecurityHeadersConfig.ContentTypeOptions.Override.
