Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-distribution-not-found-error-paths
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-distribution-not-found-error-paths
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreateInvalidation|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: e5b2a7530949298760223c07a5dc6decb6b73137

# CloudFront CreateInvalidation reports an absent distribution id as malformed

> _Note: this issue was identified against the code base at commit `e5b2a7530949298760223c07a5dc6decb6b73137`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInvalidation

## Description
ONE (1) net-new cause.

=== Cause 1 of 1: CreateInvalidation's 404 for an unknown distribution carries an emulator-authored malformed-id message instead of NoSuchDistribution's text ===
AWS: Smithy: CreateInvalidation POST /2020-05-31/distribution/{DistributionId}/invalidation, success code 201; DistributionId is @httpLabel @required; modeled errors include NoSuchDistribution (errorKind client, httpError 404, doc "The specified distribution does not exist.") and, separately, InvalidArgument (client, 400, doc "An argument is invalid."). The two are distinct conditions: a DistributionId that is well formed but names no distribution is NoSuchDistribution, and the recorded AWS response is HTTP 404 with Error.Type 'Sender', Error.Code 'NoSuchDistribution' and Error.Message 'The specified distribution does not exist.' — the entire message is FIXED text; AWS interpolates nothing from the request, so the submitted id never appears in it and no part of it is a per-run value.
Emulator: HTTP 404 with the right error code but Error.Message 'DistributionId format is invalid' — emulator-authored prose that belongs to the malformed-input class, naming the request member (DistributionId) and asserting a syntax problem the request does not have.
Why: the status and code are right but the sentence tells the caller the wrong thing: that it mis-formatted the id, when in fact the id is syntactically valid and simply does not exist. A client (or an operator reading the log) retries with a reformatted id instead of creating the distribution or correcting which distribution it targets, and any text matching or message-based classification over CloudFront's not-found refusals fails to recognize it. The emulator also conflates NoSuchDistribution with InvalidArgument, so the two conditions become indistinguishable by message.
Fix: CreateInvalidation's distribution lookup and its error construction in the cloudfront invalidation handler — an unresolved but well-formed DistributionId must raise NoSuchDistribution with the modeled fixed message rather than falling through to an id-format refusal template. The emulator makes this mistake in BOTH directions elsewhere: 20261115T235211Z-cloudfront-getdistributionconfig-and-getoriginaccesscontrol-report-absent-ids-as.md is the open issue for GetDistributionConfig/GetOriginAccessControl returning 'Id format is invalid' where AWS answers the resource-specific text (and, on the Get path, the reverse), and it does NOT cover CreateInvalidation; the shared refusal-message templates should be unified so the id-format text is only used for genuinely malformed labels.
Traces: cloudfront-distribution-not-found-error-paths#task#8.

## Evidence
CreateInvalidation, fixture cloudfront-distribution-not-found-error-paths step task#8: expected status 404 with $.ErrorResponse.Error.Message 'The specified distribution does not exist.'; actual status 404 with $.ErrorResponse.Error.Message 'DistributionId format is invalid'. Replay divergence_key: service=cloudfront|CreateInvalidation|status=404->404|keys=$.ErrorResponse.Error.Message. Status (404) and error code (NoSuchDistribution) match; the message is the only divergence, and it is entirely fixed text in AWS's wording — there is no variable part taken from the request or from emulator state, so the expected string is reproducible verbatim and the divergence is not a per-run value difference.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture cloudfront-distribution-not-found-error-paths step task#8 — CreateInvalidation (POST /2020-05-31/distribution/{DistributionId}/invalidation) against a well-formed CloudFront distribution id that does not exist in state, with a valid InvalidationBatch payload.
3. Expect HTTP 404 with ErrorResponse.Error.Type 'Sender', Code 'NoSuchDistribution' and Message 'The specified distribution does not exist.'; observe HTTP 404 with Message 'DistributionId format is invalid'.
4. Confirm with smithy_query(query="shape", shape="NoSuchDistribution", model_path="services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json") that NoSuchDistribution is httpError 404 documented "The specified distribution does not exist.", and that CreateInvalidation models it separately from InvalidArgument (400).
