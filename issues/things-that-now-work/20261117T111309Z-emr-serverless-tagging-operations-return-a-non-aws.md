Issue-Source: trace-sweep-divergence
Issue-Trace: emr-serverless-tag-resource-upsert-and-unknown-application-refusals
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: emr-serverless-tag-resource-upsert-and-unknown-application-refusals
Issue-Shape-Id: com.amazonaws.emrserverless#AwsToledoWebService
Issue-Key: emr-serverless|ListTagsForResource|4xx->4xx|$.message
Issue-Key: emr-serverless|ListTagsForResource|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: emr-serverless|TagResource|4xx->4xx|$.message
Issue-Key: emr-serverless|TagResource|4xx->4xx|$headers.x-amzn-errortype
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# EMR Serverless tagging operations return a non-AWS ResourceNotFoundException message for an unknown application

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
The EMR Serverless tagging operations correctly refuse a resource ARN that names a non-existent application with HTTP 404 / ResourceNotFoundException, but the error payload they serialize is the emulator's own wording rather than the wording AWS uses for this error class. Two operations are affected and both share one root cause: the emulator's tagging-resource lookup raises a single internally-worded not-found error that every tagging entry point serializes verbatim.

Cause 1 of 2: ListTagsForResource on an ARN whose application does not exist.
AWS: Smithy (services/service-com-amazonaws-emrserverless-awstoledowebservice) models ListTagsForResource (GET /tags/{resourceArn}) with error ResourceNotFoundException, errorKind client, httpError 404, doc "The specified resource was not found.", whose single required member is `message` (String1024, @length 1..1024, @pattern .*\S.*). The model does not fix the message text, so the requirement comes from the recording: AWS answers 404 with body member `message` = "Resource was not found". That string is entirely fixed text for this error class — it interpolates no ARN, application id, tag key or other value from the request or from service state, so there is nothing per-run in it. The recorded response also carries an `x-amzn-errortype` header whose value is of a different shape than the bare exception name the emulator emits. Hypothesis: AWS appends the Coral qualifier (ResourceNotFoundException:<qualifier>) as it does for other restJson1 services; the recording asserts only the header's shape, not its literal value.
Emulator: answers 404 with `message` = "Application does not exist" and `x-amzn-errortype` = "ResourceNotFoundException" (reported as a shape mismatch against the recorded header).
Why: a client that matches on the error message, or that surfaces it to a user or a log-scraping assertion, sees text AWS never emits; code that keys off the message text to distinguish "resource absent" from other 404s mis-classifies the failure. The status code and error code are right, so the divergence is silent until the message itself is read.
Fix: the EMR Serverless tagging handlers' not-found path — the shared resource-ARN resolution/validation that builds ResourceNotFoundException for TagResource / UntagResource / ListTagsForResource — plus the restJson1 error serializer that writes `x-amzn-errortype`. The message template must be AWS's fixed text "Resource was not found" with no interpolated values.
Traces: emr-serverless-tag-resource-upsert-and-unknown-application-refusals#task#8.

Cause 2 of 2: TagResource on an ARN whose application does not exist.
AWS: Smithy models TagResource (POST /tags/{resourceArn}, required members resourceArn @httpLabel and tags) with the same ResourceNotFoundException (client, httpError 404). The recorded AWS response is 404 with `message` = "Resource was not found" — again wholly fixed text, no request- or state-derived parts — and the same differently-shaped `x-amzn-errortype` header value.
Emulator: answers 404 with `message` = "Application does not exist" and `x-amzn-errortype` = "ResourceNotFoundException".
Why: identical client impact to Cause 1; a caller cannot rely on AWS's documented not-found wording on the write path either, and the two operations now have to be special-cased separately by any consumer written against AWS.
Fix: same component as Cause 1 — the tagging handlers' shared not-found construction and the error serializer, not a per-operation string.
Traces: emr-serverless-tag-resource-upsert-and-unknown-application-refusals#task#9.

Not covered by any existing issue: the open issue 20261113T152206Z-emr-serverless-tagging-responses-omit-the-content-length.md concerns the missing Content-Length header on empty 204 tagging bodies, and 20261109T183045Z-emr-serverless-read-responses-are-unusable-getjobrun-omits.md concerns GetJobRun/GetSessionEndpoint payloads; neither addresses the ResourceNotFoundException message text or the errortype header on the tagging refusal path.

## Evidence
Fixture emr-serverless-tag-resource-upsert-and-unknown-application-refusals.

Step task#8 (ListTagsForResource, expected 404, actual 404):
  $.message: value_mismatch — expected 'Resource was not found', got 'Application does not exist'
  $headers.x-amzn-errortype: shape_mismatch — expected '<shaped-not-asserted>', got 'ResourceNotFoundException'

Step task#9 (TagResource, expected 404, actual 404):
  $.message: value_mismatch — expected 'Resource was not found', got 'Application does not exist'
  $headers.x-amzn-errortype: shape_mismatch — expected '<shaped-not-asserted>', got 'ResourceNotFoundException'

Model evidence: Smithy ResourceNotFoundException (com.amazonaws.emrserverless#ResourceNotFoundException) — errorKind client, httpError 404, doc "The specified resource was not found.", required member message: String1024 @length 1..1024 @pattern .*\S.*; bound as an error of ListTagsForResource, TagResource and UntagResource.

Fixed vs variable parts of the required message: "Resource was not found" is entirely fixed text for this error class; no ARN, application id, tag key, request id or timestamp appears in it. The emulator's "Application does not exist" is likewise fixed text, but it is not AWS's text.

## Steps to Reproduce
1. Boot the emulator and replay fixture emr-serverless-tag-resource-upsert-and-unknown-application-refusals.
2. At step task#8, issue GET /tags/{resourceArn} (ListTagsForResource) with a well-formed emr-serverless application ARN (matching ^arn:(aws[a-zA-Z0-9-]*):emr-serverless:.+:(\d{12}):\/applications\/[0-9a-zA-Z]+$) for an application that does not exist.
3. Observe HTTP 404 with JSON body {"message": "Application does not exist"}; AWS returns {"message": "Resource was not found"}.
4. At step task#9, issue POST /tags/{resourceArn} (TagResource) with a tags map against the same non-existent application ARN.
5. Observe the same HTTP 404 with "Application does not exist" instead of AWS's "Resource was not found", and x-amzn-errortype emitted as the bare name 'ResourceNotFoundException' where the recording's header value has a different shape.
