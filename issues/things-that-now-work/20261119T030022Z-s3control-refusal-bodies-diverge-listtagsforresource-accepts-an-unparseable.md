Issue-Source: trace-sweep-divergence
Issue-Trace: s3control-generic-tagging-bucket-tag-lifecycle-and-both-refusals
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3control-generic-tagging-bucket-tag-lifecycle-and-both-refusals s3c-access-point-policy-rejection-collapses-eight-causes-into-one-code
Issue-Shape-Id: com.amazonaws.s3control#AWSS3ControlServiceV20180820
Issue-Key: s3control|ListTagsForResource|4xx->2xx|$.ErrorResponse
Issue-Key: s3control|ListTagsForResource|4xx->2xx|status
Issue-Key: s3control|PutAccessPointPolicy|4xx->4xx|$.ErrorResponse.Error.Detail
Issue-Commit: 9b7b0440ca3eb4edd2b2fd218f92aa2e777da262

# s3control refusal bodies diverge: ListTagsForResource accepts an unparseable tags URI, and a policy rejection's Error.Detail drops the offending JSON member (2 causes)

> _Note: this issue was identified against the code base at commit `9b7b0440ca3eb4edd2b2fd218f92aa2e777da262`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource, PutAccessPointPolicy

## Description
Two net-new divergences in the s3control REST-XML refusal path, both reported by the simparity-wire-aws-v1 replay as unowned mismatch clusters (stub_path empty, owned_by empty). The shared fault: when a request must be refused, the emulator's <ErrorResponse> does not match AWS's -- in one case it is not emitted at all, in the other its diagnostic member carries a different value than AWS puts there. Each cause must be closed separately.

Cause 1 of 2: ListTagsForResource answers 200 for a resource ARN whose URI AWS cannot parse, instead of refusing it 400 InvalidURI.
AWS: Smithy ListTagsForResource -- GET /v20180820/tags/{ResourceArn+}, success code 200, endpoint hostPrefix '{AccountId}.', input AccountId @required (@httpHeader x-amz-account-id + hostLabel, pattern ^\d{12}$) and ResourceArn @required @httpLabel (greedy label, target S3ResourceArn, length 0..1011, pattern ^arn:[^:]+:s3(express)?:[^:]), output Tags (TagList 0..50), errors []. The refusal is therefore recorded behavior only: when the ARN placed in the greedy {ResourceArn+} label is not a resource form this surface can address, S3 Control fails URI parsing and answers HTTP 400 with a REST-XML <ErrorResponse><Error> whose Code is 'InvalidURI' and whose Message is "Couldn't parse the specified URI." -- both fixed text -- plus an Error.URI member carrying the unparsed request path relative to the API version prefix, i.e. 'tags/<the resource ARN exactly as sent>' (a request value, not a literal), alongside per-run RequestId/HostId. Hypothesis: the regional bucket-style ARN form used here (arn:aws:s3:<region>:<account>:bucket/<name>) is not an addressable generic-tagging resource, so the path never resolves to a resource and the failure surfaces as a URI parse error rather than a not-found.
Emulator: answers HTTP 200 with a ListTagsForResourceResult and no ErrorResponse at all ('status: expected 400, got 200'; '$.ErrorResponse: missing'), i.e. it accepts the ARN form and reports a tag set for it.
Why: the generic tagging surface silently accepts resource identifiers AWS rejects, so a client that mis-addresses a resource gets an empty-or-stale 200 instead of InvalidURI and never discovers the mistake; code written to catch InvalidURI (and to read Error.URI to report which path failed) has no reachable branch, and tag state can be read under an identifier that will never work against AWS.
Fix: ResourceArn parsing/validation in the generic-tagging handlers (ListTagsForResource and its TagResource/UntagResource siblings), which must classify an unaddressable ARN form as InvalidURI and serialize the REST-XML error with the Error.URI member instead of falling through to a success body. NOTE: open issue 20261022T213144Z-s3control-access-points-directory-bucket-listing-job-object.md (Cause 7) covers the OPPOSITE direction for a different ARN form -- the plain 'arn:aws:s3:::<bucket>' form being wrongly REFUSED, and refused with a JSON ValidationException -- and does not own this over-acceptance; both must end up in one coherent ARN classifier.
Traces: s3control-generic-tagging-bucket-tag-lifecycle-and-both-refusals#task#7.

Cause 2 of 2: the PutAccessPointPolicy rejection's Error.Detail quotes only the offending principal value instead of the offending JSON member from the submitted policy.
AWS: Smithy PutAccessPointPolicy -- PUT /v20180820/accesspoint/{Name}/policy, success code 200 (empty output), endpoint hostPrefix '{AccountId}.', input AccountId @required (@httpHeader x-amz-account-id + hostLabel pattern ^\d{12}$), Name @required @httpLabel length 3..255, Policy @required; errors []. Recorded behavior: a policy whose principal cannot be resolved is refused HTTP 400 with a REST-XML <ErrorResponse><Error> carrying, besides Code and Message, an Error.Detail member that reproduces the offending fragment of the SUBMITTED policy document -- the whole JSON member, key included, with the document's own quoting and spacing ('"AWS" : "<principal-arn>"'). The member name and the principal ARN inside it are values taken from the request body; what is required is that the Detail is that document fragment and not a value extracted out of it.
Emulator: the status (400), the error Code and the Message all match, but Error.Detail carries the bare principal ARN with the '"AWS" : ' key and the surrounding quotes stripped -- '$.ErrorResponse.Error.Detail: value_mismatch (expected '"AWS" : "arn:aws:iam::<account>:role/<role>"', got 'arn:aws:iam::<account>:role/<role>')'.
Why: Error.Detail is the only part of this refusal that tells a client WHERE in its policy the fault is; stripped to the value, it no longer identifies the policy member, so tooling that locates the offending statement by matching the Detail against the document it sent finds nothing, and the emulator's refusal is not the diagnostic AWS's is. The fixture name records that AWS collapses eight distinct policy faults onto one error code, which makes Detail the only discriminator.
Fix: the error serializer / Detail construction in the PutAccessPointPolicy validation path -- it must emit the offending policy member verbatim as it appeared in the request document rather than the extracted principal value.
Traces: s3c-access-point-policy-rejection-collapses-eight-causes-into-one-code#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 34 fixtures replayed, 341 steps, 310 passed, 31 mismatched, score 90.9%, 14 divergence clusters. Both causes are verdict=mismatch with stub_path empty and owned_by empty.

Cause 1 -- fixture s3control-generic-tagging-bucket-tag-lifecycle-and-both-refusals, step task#7, operation ListTagsForResource, divergence_key 'service=s3control|ListTagsForResource|status=400->200|keys=$.ErrorResponse,status':
  status: expected 400, got 200
  $.ErrorResponse: missing (expected {'Error': {'Code': 'InvalidURI', 'Message': "Couldn't parse the specified URI.", 'URI': 'tags/<the resource ARN exactly as sent, here a regional bucket-style arn:aws:s3:<region>:<account>:bucket/<name>>'}, 'RequestId': '<per-run>', 'HostId': '<per-run>'}, got None)
Code 'InvalidURI' and Message "Couldn't parse the specified URI." are fixed text; the Error.URI member is the request path 'tags/' + the ARN from the request URI. The recorded RequestId/HostId are shaped-not-asserted per-run values.

Cause 2 -- fixture s3c-access-point-policy-rejection-collapses-eight-causes-into-one-code, step task#11, operation PutAccessPointPolicy, divergence_key 'service=s3control|PutAccessPointPolicy|status=400->400|keys=$.ErrorResponse.Error.Detail' (status 400 -> 400, only Detail diverges):
  $.ErrorResponse.Error.Detail: value_mismatch (expected '"AWS" : "arn:aws:iam::<account>:role/<role>"', got 'arn:aws:iam::<account>:role/<role>')
The account id and role name in that fragment come from the policy document the step submits; the requirement is the enclosing JSON member text ('"AWS" : "..."'), which the emulator drops.

Model evidence (Smithy, services/service-com-amazonaws-s3control-awss3controlservicev20180820/smithy-model/com-amazonaws-s3control-awss3controlservicev20180820.json): ListTagsForResource GET /v20180820/tags/{ResourceArn+} code 200, hostPrefix '{AccountId}.', ResourceArn @required @httpLabel target S3ResourceArn length 0..1011 pattern ^arn:[^:]+:s3(express)?:[^:], output Tags TagList 0..50, errors []; PutAccessPointPolicy PUT /v20180820/accesspoint/{Name}/policy code 200, hostPrefix '{AccountId}.', Policy @required, output Unit, errors []. Neither operation binds an error shape, so the InvalidURI / Detail requirements above are taken from the recorded responses.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. replay_wire_corpus() over trace set simparity-wire-aws-v1.
3. Cause 1: fixture s3control-generic-tagging-bucket-tag-lifecycle-and-both-refusals, step task#7 -- after the bucket tag lifecycle steps, issue GET /v20180820/tags/{ResourceArn+} (ListTagsForResource) with the regional bucket-style ARN form arn:aws:s3:<region>:<account>:bucket/<bucket> in the greedy label. Expect HTTP 400 with Error.Code 'InvalidURI', Error.Message "Couldn't parse the specified URI." and Error.URI = 'tags/' + that ARN; the emulator answers HTTP 200 with a tag list.
4. Cause 2: fixture s3c-access-point-policy-rejection-collapses-eight-causes-into-one-code, step task#11 -- PUT /v20180820/accesspoint/{Name}/policy with a policy whose Principal names an unresolvable IAM role. The 400, the Code and the Message already match; compare Error.Detail. AWS returns the submitted document's member text '"AWS" : "<principal-arn>"'; the emulator returns only '<principal-arn>'.
5. Re-replay both steps named under Traces to confirm the refusal body and the Detail member.
