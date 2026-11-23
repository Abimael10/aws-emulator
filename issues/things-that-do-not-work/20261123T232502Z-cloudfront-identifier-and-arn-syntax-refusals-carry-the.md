Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-anycast-ip-list-error-paths
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-anycast-ip-list-error-paths cloudfront-domain-and-alias-association-error-paths cloudfront-list-distributions-by-owned-resource-filters-correctly
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetAnycastIpList|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: cloudfront|GetAnycastIpList|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetAnycastIpList|4xx->4xx|status
Issue-Key: cloudfront|ListDistributionsByOwnedResource|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 74fb065dbfcdabb80a136d7542fc7487ef053384

# CloudFront identifier and ARN syntax refusals carry the wrong error class and wording

> _Note: this issue was identified against the code base at commit `74fb065dbfcdabb80a136d7542fc7487ef053384`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAnycastIpList

## Description
Two net-new causes, both the same shared fault: a CloudFront read operation that is handed a syntactically unacceptable identifier or ARN does not refuse it as a syntax problem. The emulator skips the syntactic admission check and instead answers with the result of (or a message about) its state lookup, so the HTTP status, the error Code and the error Message all differ from AWS's.

Cause 1 of 2: GetAnycastIpList answers a malformed Anycast IP list Id as a missing resource instead of a malformed argument.
AWS: Smithy (cloudfront 2020-05-31) GetAnycastIpList — input member Id is @required and @httpLabel on GET /2020-05-31/anycast-ip-list/{Id}; the operation models BOTH InvalidArgument (httpError 400) and EntityNotFound (httpError 404). AWS applies the syntactic check on Id first: an Id that is not in CloudFront's resource-id form is refused 400 with ErrorResponse/Error Type 'Sender', Code 'InvalidArgument', Message 'Id format is invalid'. That message is entirely fixed text — it interpolates nothing from the request or from state — so it is reproducible verbatim. EntityNotFound/404 is reserved for an id that IS well formed but names nothing.
Emulator: answers 404 with Code 'EntityNotFound' and Message 'The specified Anycast IP list does not exist' — i.e. it performs no syntax validation at all and reports the lookup miss. Status, Code and Message all diverge.
Why: a client cannot distinguish a programming error (it built a bad id, e.g. pasted an id of another resource type) from a genuine absence. Retry/backoff and 'create if missing' logic keyed on EntityNotFound/404 fires on input it must instead reject, and code branching on the 400/InvalidArgument pair never runs.
Fix: GetAnycastIpList's input validation in the cloudfront operation handler — validate the Id's format before the state lookup and map the failure to InvalidArgument (400), leaving EntityNotFound (404) for well-formed-but-absent ids.
Traces: cloudfront-anycast-ip-list-error-paths#task#2.

Cause 2 of 2: ListDistributionsByOwnedResource refuses an ARN of an unsupported resource type by claiming the ResourceArn parameter was not supplied.
AWS: Smithy (cloudfront 2020-05-31) ListDistributionsByOwnedResource — input member ResourceArn is @required and @httpLabel on GET /2020-05-31/distributionsByOwnedResource/{ResourceArn}; errors InvalidArgument (400), EntityNotFound (404). Recorded behavior: when a syntactically parseable ARN names a resource type this operation does not accept, AWS refuses it 400 with Message 'Resource has invalid syntax: invalid resource type: <resourceArn>'. The fixed text is 'Resource has invalid syntax: invalid resource type: '; the only variable part is the ARN taken verbatim from the request's ResourceArn label (in the recording an arn:aws:cloudfront::<account>:vpc-origin/<name> value — that particular ARN is the fixture's input, not part of the requirement).
Emulator: answers 400 with Message 'The ResourceArn parameter is required. Specify a valid ARN and try again.' — the generic 'parameter absent' sentence of a different (resource-policy style) surface, for a request that DID carry ResourceArn. The status matches; the message does not, and it misstates the reason.
Why: the refusal tells the caller to supply a parameter it already supplied, so a client (or a human reading a log) cannot learn that the ARN's resource type is the problem; message-matching assertions and any 'add the missing parameter' remediation path are both wrong.
Fix: ListDistributionsByOwnedResource's input validation / error mapping in the cloudfront operation handler — distinguish 'ResourceArn absent' from 'ResourceArn present but names an unsupported resource type' and emit AWS's invalid-syntax sentence with the submitted ARN interpolated.
Traces: cloudfront-domain-and-alias-association-error-paths#task#8 and cloudfront-list-distributions-by-owned-resource-filters-correctly#task#2.

Related open issue, which does NOT cover these steps (the replay reports both clusters as unowned, so neither carries this replay key): 20261123T034604Z-cloudfront-listdistributionsbyownedresource-refuses-a-malformed-arn-and-an-unsupported.md describes ListDistributionsByOwnedResource's refusal wording from the static surface; cause 2 is the wire-replayed instance of it and must be re-replayed against the fixtures named above. Several open issues (e.g. 20261117T051410Z-cloudfront-getfieldlevelencryptionprofile-answers-the-not-found-message-for-an.md) cover the same malformed-vs-absent confusion on OTHER resource types; none names GetAnycastIpList.

## Evidence
Cause 1 — fixture cloudfront-anycast-ip-list-error-paths, step task#2, GetAnycastIpList:
  expected: HTTP 400, $.ErrorResponse.Error.Code = 'InvalidArgument', $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Code = 'EntityNotFound', $.ErrorResponse.Error.Message = 'The specified Anycast IP list does not exist'
  divergence_key: service=cloudfront|GetAnycastIpList|status=400->404|keys=$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message,status

Cause 2 — fixture cloudfront-domain-and-alias-association-error-paths, step task#8, ListDistributionsByOwnedResource (cluster also covers cloudfront-list-distributions-by-owned-resource-filters-correctly#task#2):
  expected: HTTP 400, $.ErrorResponse.Error.Message = 'Resource has invalid syntax: invalid resource type: arn:aws:cloudfront::123456789012:vpc-origin/vo-sim2real-probe-nonexistent'
  actual:   HTTP 400, $.ErrorResponse.Error.Message = 'The ResourceArn parameter is required. Specify a valid ARN and try again.'
  divergence_key: service=cloudfront|ListDistributionsByOwnedResource|status=400->400|keys=$.ErrorResponse.Error.Message

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture cloudfront-anycast-ip-list-error-paths and observe step task#2 — GET /2020-05-31/anycast-ip-list/{Id} with an Id that is not a valid CloudFront resource-id form. Expect 400 InvalidArgument 'Id format is invalid'; the emulator answers 404 EntityNotFound 'The specified Anycast IP list does not exist'.
3. Cause 2: replay fixture cloudfront-domain-and-alias-association-error-paths and observe step task#8 (and cloudfront-list-distributions-by-owned-resource-filters-correctly#task#2) — GET /2020-05-31/distributionsByOwnedResource/{ResourceArn} with a parseable ARN whose resource type the operation does not accept. Expect 400 with 'Resource has invalid syntax: invalid resource type: <the submitted ARN>'; the emulator answers 400 with 'The ResourceArn parameter is required. Specify a valid ARN and try again.'
