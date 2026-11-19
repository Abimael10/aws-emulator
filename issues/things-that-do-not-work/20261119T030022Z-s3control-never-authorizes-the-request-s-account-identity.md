Issue-Source: trace-sweep-divergence
Issue-Trace: s3c-account-id-host-prefix-is-authorized-before-the-request
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3c-account-id-host-prefix-is-authorized-before-the-request
Issue-Shape-Id: com.amazonaws.s3control#AWSS3ControlServiceV20180820
Issue-Key: s3control|GetAccessPoint|4xx->2xx|$.ErrorResponse
Issue-Key: s3control|GetAccessPoint|4xx->2xx|status
Issue-Key: s3control|GetAccessPoint|4xx->4xx|$.ErrorResponse.Error.AccountId
Issue-Key: s3control|GetAccessPoint|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: s3control|GetAccessPoint|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: s3control|GetAccessPoint|4xx->4xx|status
Issue-Key: s3control|ListAccessPoints|4xx->2xx|$.ErrorResponse
Issue-Key: s3control|ListAccessPoints|4xx->2xx|status
Issue-Related: 20261110T084205Z-s3control-getaccesspoint-success-bodies-diverge-endpoints-map-order.md
Issue-Commit: 9b7b0440ca3eb4edd2b2fd218f92aa2e777da262

# s3control never authorizes the request's account identity, so cross-account reads are served instead of refused AccessDenied (3 causes)

> _Note: this issue was identified against the code base at commit `9b7b0440ca3eb4edd2b2fd218f92aa2e777da262`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAccessPoint, ListAccessPoints

## Description
Three net-new divergences in the s3control read surface, all reported as unowned mismatch clusters by the simparity-wire-aws-v1 replay (stub_path empty, owned_by empty). The shared fault: the account identity every s3control operation carries -- the @required AccountId member, bound both to the x-amz-account-id header and to the operation's '{AccountId}.' endpoint host prefix -- is never authorized against the calling identity. On AWS that authorization runs BEFORE the operation resolves any resource, so a request naming an account the caller is not is refused HTTP 403 AccessDenied whatever the resource state is; the emulator instead routes straight into state and answers from it. Each cause must be closed separately.

Cause 1 of 3: GetAccessPoint serves an access point's full configuration to a caller whose account identity does not own it.
AWS: Smithy GetAccessPoint -- GET /v20180820/accesspoint/{Name}, success code 200, endpoint hostPrefix '{AccountId}.', input AccountId @required with @httpHeader x-amz-account-id + hostLabel and pattern ^\d{12}$, Name @httpLabel length 3..255; the operation binds no modeled errors (errors []), so the refusal itself is recorded behavior only: S3 Control answers HTTP 403 with a REST-XML <ErrorResponse><Error> whose Code is 'AccessDenied' and Message is 'Access Denied' -- both fixed text -- plus an Error.AccountId member carrying the 12-digit account id taken from the request's host prefix / x-amz-account-id header (a request value, not a literal), and RequestId/HostId which are per-run values.
Emulator: answers HTTP 200 with a complete GetAccessPointResult and no ErrorResponse at all ('$.ErrorResponse: missing'), including the four Endpoints entries, the bucket name and the public-access-block of an access point the requesting identity is not entitled to see.
Why: cross-account isolation is unobservable. A client (or a conformance test) that proves a foreign account cannot read an access point's configuration passes against the emulator and fails against AWS, and the emulator hands a non-owning caller the access point's endpoint hostnames and underlying bucket name.
Fix: the account-identity authorization step in the GetAccessPoint handler -- it must run before the access-point state lookup and must serialize the REST-XML AccessDenied error (with the Error.AccountId member) instead of a result body. NOTE: open issue 20261110T084205Z-s3control-getaccesspoint-success-bodies-diverge-endpoints-map-order.md describes this same authorization gap as its Cause 2, but the replay reports this cluster as unowned (owned_by empty), so it is filed here and must be re-replayed on this step; 20261106T121347Z-s3control-access-point-reads-diverge-authorization-and-existence.md is also related and owns neither.
Traces: s3c-account-id-host-prefix-is-authorized-before-the-request#task#4.

Cause 2 of 3: the account-identity check runs AFTER the resource lookup, so an unauthorized request for a name that is absent from the named account is answered 404 NoSuchAccessPoint instead of 403 AccessDenied.
AWS: recorded behavior only (GetAccessPoint binds no errors in the model; the fixture is named for this ordering). The account identity in the host prefix / x-amz-account-id header is authorized before the access point is resolved, so the answer is HTTP 403 with Error.Code 'AccessDenied', Error.Message 'Access Denied' (fixed text) and Error.AccountId = the request's 12-digit account id. Existence is never disclosed to an identity that is not authorized for that account, so NoSuchAccessPoint is not reachable on this path.
Emulator: answers HTTP 404 with Error.Code 'NoSuchAccessPoint', Error.Message 'The specified accesspoint does not exist' and NO Error.AccountId member ('$.ErrorResponse.Error.AccountId: missing'), i.e. it resolved the name in state first and reported the lookup result.
Why: the precedence is inverted, so the refusal a client receives depends on resource state rather than on identity: an unauthorized caller can distinguish 'exists' from 'does not exist' in another account (an existence oracle), and a client branching on 403/AccessDenied to re-resolve credentials instead sees 404/NoSuchAccessPoint and concludes the resource is gone.
Fix: the ordering in the GetAccessPoint handler -- the account-identity authorization gate must precede the state lookup for every s3control operation, and its error serializer must emit AccessDenied with the Error.AccountId member rather than the existence error.
Traces: s3c-account-id-host-prefix-is-authorized-before-the-request#task#5.

Cause 3 of 3: ListAccessPoints enumerates an account's access points for a caller whose identity is not that account.
AWS: Smithy ListAccessPoints -- GET /v20180820/accesspoint, success code 200, endpoint hostPrefix '{AccountId}.', input AccountId @required @httpHeader x-amz-account-id + hostLabel pattern ^\d{12}$, optional Bucket/NextToken/MaxResults/DataSourceId/DataSourceType query members, errors []. Recorded behavior: the listing is refused HTTP 403 with Error.Code 'AccessDenied' and Error.Message 'Access Denied' (fixed text) plus Error.AccountId = the request's account id, before any enumeration happens.
Emulator: answers HTTP 200 with a ListAccessPointsResult body and no ErrorResponse ('status: expected 403, got 200'; '$.ErrorResponse: missing').
Why: an unauthorized caller can enumerate another account's access-point inventory, and the negative path of every ownership/isolation test is dead -- the emulator never produces the 403 the client is written to handle.
Fix: the same account-identity authorization gate, applied in the ListAccessPoints handler ahead of enumeration, with the REST-XML AccessDenied serialization. Because all three causes are the same missing gate on three handlers, the fix belongs in the shared s3control request-authorization path rather than per operation.
Traces: s3c-account-id-host-prefix-is-authorized-before-the-request#task#8.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 34 fixtures replayed, 341 steps, 310 passed, 31 mismatched, score 90.9%, 14 divergence clusters. All three causes below are verdict=mismatch with stub_path empty and owned_by empty (unowned, net-new).

Cause 1 -- fixture s3c-account-id-host-prefix-is-authorized-before-the-request, step task#4, operation GetAccessPoint, divergence_key 'service=s3control|GetAccessPoint|status=403->200|keys=$.ErrorResponse,$.GetAccessPointResult.Endpoints.entry[0..3].value,status':
  status: expected 403, got 200
  $.ErrorResponse: missing (expected {'Error': {'Code': 'AccessDenied', 'Message': 'Access Denied', 'AccountId': '<the request's 12-digit account id>'}, 'RequestId': '<per-run>', 'HostId': '<per-run>'}, got None)
  $.GetAccessPointResult.Endpoints.entry[0..3].value: host_not_own (expected None, got the four access-point hostnames)
The recorded RequestId/HostId values in that step are per-run and are not part of the requirement; Code 'AccessDenied' and Message 'Access Denied' are fixed text, and Error.AccountId is the account id value copied from the request.

Cause 2 -- same fixture, step task#5, operation GetAccessPoint, divergence_key 'service=s3control|GetAccessPoint|status=403->404|keys=$.ErrorResponse.Error.AccountId,$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message,status':
  status: expected 403, got 404
  $.ErrorResponse.Error.Code: value_mismatch (expected 'AccessDenied', got 'NoSuchAccessPoint')
  $.ErrorResponse.Error.Message: value_mismatch (expected 'Access Denied', got 'The specified accesspoint does not exist')
  $.ErrorResponse.Error.AccountId: missing (expected the request's 12-digit account id, got None)

Cause 3 -- same fixture, step task#8, operation ListAccessPoints, divergence_key 'service=s3control|ListAccessPoints|status=403->200|keys=$.ErrorResponse,status':
  status: expected 403, got 200
  $.ErrorResponse: missing (expected {'Error': {'Code': 'AccessDenied', 'Message': 'Access Denied', 'AccountId': '<the request's 12-digit account id>'}, 'RequestId': '<per-run>', 'HostId': '<per-run>'}, got None)

Model evidence (Smithy, services/service-com-amazonaws-s3control-awss3controlservicev20180820/smithy-model/com-amazonaws-s3control-awss3controlservicev20180820.json): GetAccessPoint GET /v20180820/accesspoint/{Name} code 200, endpoint hostPrefix '{AccountId}.', input AccountId @required @httpHeader x-amz-account-id + hostLabel pattern ^\d{12}$ length 0..64, Name @httpLabel length 3..255, errors []; ListAccessPoints GET /v20180820/accesspoint code 200, endpoint hostPrefix '{AccountId}.', same AccountId binding, paginated NextToken/MaxResults, errors []. Since neither operation binds an error shape, the AccessDenied/NoSuchAccessPoint classifications above come from the recording.

Note: the emulator DOES serve the '{AccountId}.' host prefix in this replay (these steps were delivered and answered), so the open 'host prefix is unserved' issues (e.g. 20261116T222947Z-s3control-the-account-id-host-prefix-every-operation.md) describe a different, earlier failure mode and do not cover these; what is missing here is authorization of that account identity, not routing of it.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. replay_wire_corpus() over trace set simparity-wire-aws-v1.
3. Cause 1: fixture s3c-account-id-host-prefix-is-authorized-before-the-request, step task#4 -- create an access point in the owning account, then issue GET /v20180820/accesspoint/{Name} with an account identity (host prefix '{AccountId}.' and x-amz-account-id) that is not entitled to that account. Expect HTTP 403 with REST-XML Error.Code 'AccessDenied', Error.Message 'Access Denied' and Error.AccountId = the account id sent in the request; the emulator answers 200 with the full GetAccessPointResult including the Endpoints map.
4. Cause 2: same fixture, step task#5 -- repeat the unauthorized GetAccessPoint for a {Name} that does not exist under the named account. Expect the same 403 AccessDenied (authorization precedes the lookup); the emulator answers 404 NoSuchAccessPoint 'The specified accesspoint does not exist' with no Error.AccountId.
5. Cause 3: same fixture, step task#8 -- issue GET /v20180820/accesspoint (ListAccessPoints) with the same unauthorized account identity. Expect HTTP 403 AccessDenied with Error.AccountId; the emulator answers 200 with a ListAccessPointsResult.
6. Re-replay the three steps named in the Traces lines above to confirm all three refusals.
