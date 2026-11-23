Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-function-js-1-empty-comment-publish-live-and-development-etags-and-not-found
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-function-js-1-empty-comment-publish-live-and-development-etags-and-not-found
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|TestFunction|2xx->2xx|$.TestResult.FunctionOutput
Issue-Commit: 74fb065dbfcdabb80a136d7542fc7487ef053384

# CloudFront TestFunction result omits the cookies member of the function event object

> _Note: this issue was identified against the code base at commit `74fb065dbfcdabb80a136d7542fc7487ef053384`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestFunction

## Description
One cause.
AWS: Smithy (cloudfront 2020-05-31) TestFunction — output member TestResult is the @httpPayload, and TestResult.FunctionOutput is documented as 'The event object returned by the function ... see Event object structure in the Amazon CloudFront Developer Guide'. The CloudFront Functions event structure always carries the full response object: a response produced by a function returning a redirect serialises statusCode, statusDescription, a headers map, AND a cookies map — the cookies map is present as an empty object when the function set no cookies, exactly as the recording shows ('cookies':{}). The requirement is the SHAPE of the serialised event object: every member of the response object the structure defines is present, with absent collections serialised as empty objects. The particular statusCode/location values in the recording come from the fixture's function source and are not part of the requirement.
Emulator: FunctionOutput is serialised without the cookies member at all — the returned JSON stops after headers, so a function result that legitimately has no cookies is indistinguishable from one whose event object lacks the member.
Why: a client parsing FunctionOutput to validate a function under test (the normal use of TestFunction in a deploy pipeline) must index response.cookies; against AWS that key always exists, against the emulator it is absent, so the parse throws or the test wrongly concludes the function's cookie handling is broken. Nothing in the result tells the caller the member was dropped by the service rather than by the function.
Fix: the TestFunction result serializer / the function-runtime event-object construction in the cloudfront handler — emit the complete response event object, including an empty cookies map when the function set none.
Not filed as a defect: the recording and the emulator also differ in the ORDER of the keys inside the headers map (location before cloudfront-functions vs the reverse). JSON object member order is not semantically meaningful and is not an AWS requirement, so this issue is grounded solely in the missing cookies member.
Traces: cloudfront-function-js-1-empty-comment-publish-live-and-development-etags-and-not-found#task#6.

## Evidence
Fixture cloudfront-function-js-1-empty-comment-publish-live-and-development-etags-and-not-found, step task#6, TestFunction, both sides HTTP 200:
  expected $.TestResult.FunctionOutput: {"response":{"statusCode":302,"statusDescription":"Found","headers":{"location":{"value":"https://example.com/"},"cloudfront-functions":{"value":"generated-by-CloudFront-Functions"}},"cookies":{}}}
  actual   $.TestResult.FunctionOutput: {"response":{"statusCode":302,"statusDescription":"Found","headers":{"cloudfront-functions":{"value":"generated-by-CloudFront-Functions"},"location":{"value":"https://example.com/"}}}}
  divergence_key: service=cloudfront|TestFunction|status=200->200|keys=$.TestResult.FunctionOutput
The only structural difference is the absent "cookies":{} member; the remaining difference is header-map key order, which is not asserted as a requirement here.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Replay fixture cloudfront-function-js-1-empty-comment-publish-live-and-development-etags-and-not-found and observe step task#6: POST /2020-05-31/function/{Name}/test with a valid If-Match and an EventObject, against a function whose code returns a 302 response and sets no cookies.
3. Parse $.TestResult.FunctionOutput as JSON and read response.cookies. Against AWS it is present (an empty object); the emulator omits the member entirely.
