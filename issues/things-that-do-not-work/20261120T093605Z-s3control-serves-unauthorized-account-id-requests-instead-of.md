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
Issue-Commit: a07844ce34e0c9873e72adc5edaddfb14e92856b

# s3control serves unauthorized account-id requests instead of 403 AccessDenied

> _Note: this issue was identified against the code base at commit `a07844ce34e0c9873e72adc5edaddfb14e92856b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAccessPoint

## Description
Three net-new divergence clusters in the s3control account-scoped read path, all reported by the replay with empty stub_path and empty owned_by. The shared fault: the account identity every s3control operation carries (the '{AccountId}.' endpoint host prefix / x-amz-account-id header) is never authorized, so a request made under an account id that does not authorize it is executed against the emulator's state instead of being refused. This bundle has 3 causes; all three must be closed.

Cause 1 of 3: GetAccessPoint serves the full access-point configuration to a request whose account identity does not authorize it.
AWS: Smithy (services/service-com-amazonaws-s3control-awss3controlservicev20180820/smithy-model/com-amazonaws-s3control-awss3controlservicev20180820.json) GetAccessPoint: GET /v20180820/accesspoint/{Name}, success code 200, endpoint hostPrefix '{AccountId}.', input member AccountId @required, bound BOTH as @httpHeader('x-amz-account-id') and as @hostLabel, target AccountId with pattern ^\d{12}$; errors [] - the operation models no errors, so the refusal is recorded behavior only. The recording shows S3 Control authorizing that account identity FIRST and answering HTTP 403 with a REST-XML body <ErrorResponse><Error><Code>AccessDenied</Code><Message>Access Denied</Message><AccountId>...</AccountId></Error>...</ErrorResponse>. Fixed text: the status 403, the code 'AccessDenied' and the message 'Access Denied' exactly. Variable parts: Error.AccountId is the 12-digit account id taken from the request's host prefix / x-amz-account-id header; RequestId and HostId are per-run values and are not requirements.
Emulator: answers HTTP 200 with a complete GetAccessPointResult and no ErrorResponse at all - the replay reports 'status: expected 403, got 200' and '$.ErrorResponse: missing', plus four $.GetAccessPointResult.Endpoints.entry[*].value members (expected None) that should not be present in a refusal.
Why: cross-account isolation is unobservable. A caller presenting an account identity that does not own the access point reads its bucket name, VPC configuration, public-access-block settings, alias, ARN and its four access-point endpoint hostnames. Any client or test that proves ownership isolation passes against the emulator and fails against AWS, and a client that relies on AccessDenied to detect a wrong-account configuration sees success instead.
Fix: the account-identity authorization step of the GetAccessPoint handler - it must run before the access-point state lookup and serialize the REST-XML AccessDenied error (Error.Code/Error.Message/Error.AccountId) instead of a result body. Related open issue 20261110T084205Z-s3control-getaccesspoint-success-bodies-diverge-endpoints-map-order.md describes this same authorization gap as its Cause 2, but the replay reports NO owner for this cluster, so it does not cover it and this step must be re-replayed here.
Traces: s3c-account-id-host-prefix-is-authorized-before-the-request#task#4.

Cause 2 of 3: the existence lookup runs BEFORE the account-identity authorization, so an unauthorized read of an absent name leaks which name does not exist.
AWS: same model facts as Cause 1 (AccountId @required, @hostLabel + x-amz-account-id, pattern ^\d{12}$; no modeled errors, so recorded behavior only). The recording shows that when the request's account identity does not authorize the call, AWS refuses with HTTP 403, Error.Code 'AccessDenied', Error.Message 'Access Denied' and Error.AccountId = the request's 12-digit account id - REGARDLESS of whether the {Name} in the URI names an existing access point. Authorization precedes resource resolution, so an unauthorized caller cannot distinguish an existing access point from an absent one. (The 404 NoSuchAccessPoint / 'The specified accesspoint does not exist' refusal is the AUTHORIZED caller's answer for an absent name; it is not reachable by an unauthorized one.)
Emulator: resolves the name first and answers the existence refusal to the unauthorized caller - 'status: expected 403, got 404', '$.ErrorResponse.Error.Code: value_mismatch (expected AccessDenied, got NoSuchAccessPoint)', '$.ErrorResponse.Error.Message: value_mismatch (expected "Access Denied", got "The specified accesspoint does not exist")', '$.ErrorResponse.Error.AccountId: missing (expected the request's account id, got None)'.
Why: a client gets the wrong status (404 vs 403), the wrong error code and the wrong message, and the Error.AccountId member AWS includes in AccessDenied is absent. Error-handling code that branches on NoSuchAccessPoint takes a branch AWS never takes, and the emulator turns an authorization failure into an existence oracle: an unauthorized caller can enumerate which access-point names exist in another account by reading 403 vs 404.
Fix: the ordering inside the GetAccessPoint handler - the account-identity authorization check must be evaluated before the state lookup that produces NoSuchAccessPoint, and the AccessDenied serializer must emit the Error.AccountId member.
Traces: s3c-account-id-host-prefix-is-authorized-before-the-request#task#5.

Cause 3 of 3: ListAccessPoints answers a listing to a request whose account identity does not authorize it.
AWS: Smithy ListAccessPoints: GET /v20180820/accesspoint, success code 200, endpoint hostPrefix '{AccountId}.', input AccountId @required, @httpHeader('x-amz-account-id') + @hostLabel, pattern ^\d{12}$; optional Bucket/NextToken/MaxResults/DataSourceId/DataSourceType query members; errors [] - refusal is recorded behavior only. The recording shows HTTP 403 with <ErrorResponse><Error><Code>AccessDenied</Code><Message>Access Denied</Message><AccountId>...</AccountId></Error>...: status, code and message are fixed text, Error.AccountId is the account id carried by the request, and RequestId/HostId are per-run values.
Emulator: answers HTTP 200 with a ListAccessPointsResult and no refusal body - 'status: expected 403, got 200', '$.ErrorResponse: missing'.
Why: the same cross-account isolation break on the collection endpoint, and worse than on the single read: an unauthorized caller enumerates every access point the emulator holds (names, buckets, network origin, aliases, ARNs) rather than being refused. A client that uses ListAccessPoints under a wrong account id to confirm it has no access reads a populated list.
Fix: the same account-identity authorization step, applied in the ListAccessPoints handler before the state enumeration, serializing the REST-XML AccessDenied error instead of a result body. This authorization check belongs in the shared account-scoped request path so every s3control operation carrying the '{AccountId}.' host prefix is gated uniformly, not just these two reads.
Traces: s3c-account-id-host-prefix-is-authorized-before-the-request#task#8.

Not covered by existing open issues: the replay reports owned_by=[] for all three clusters. 20261116T222947Z-s3control-the-account-id-host-prefix-every-operation.md concerns the host-prefixed ADDRESS not being served at all (the behavioral corpus never reaches the handlers); here the wire corpus does reach the handlers over the bare authority and the account id they carry is simply not authorized. 20261113T153902Z-s3control-reopens-an-internet-borne-read-of-a-vpc.md covers VPC-origin data-path denial, a different check.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 34 fixtures replayed, 341 steps, 310 passed, 31 mismatched, 0 errored, 0 blocked, score 90.9%, emulator_up=true, bootstrap_signal=false, divergence_count=14. All three clusters below are verdict=mismatch with stub_path='' and owned_by=[].

Cause 1 - fixture s3c-account-id-host-prefix-is-authorized-before-the-request, step task#4, operation GetAccessPoint, divergence_key 'service=s3control|GetAccessPoint|status=403->200|keys=$.ErrorResponse,$.GetAccessPointResult.Endpoints.entry[0].value,...entry[3].value,status', affected_count 1:
  status: expected 403, got 200
  $.ErrorResponse: missing (expected {'Error': {'Code': 'AccessDenied', 'Message': 'Access Denied', 'AccountId': <the request's 12-digit account id>}, 'RequestId': <per-run>, 'HostId': <per-run>}, got None)
  $.GetAccessPointResult.Endpoints.entry[0..3].value: host_not_own (expected None, got the four derived access-point hostnames)
Expected: HTTP 403, Error.Code 'AccessDenied', Error.Message 'Access Denied', Error.AccountId = the request's account id, and NO GetAccessPointResult. Actual: HTTP 200 with the full configuration and endpoint map.

Cause 2 - same fixture, step task#5, operation GetAccessPoint, divergence_key 'service=s3control|GetAccessPoint|status=403->404|keys=$.ErrorResponse.Error.AccountId,$.ErrorResponse.Error.Code,$.ErrorResponse.Error.Message,status', affected_count 1:
  status: expected 403, got 404
  $.ErrorResponse.Error.Code: value_mismatch (expected 'AccessDenied', got 'NoSuchAccessPoint')
  $.ErrorResponse.Error.Message: value_mismatch (expected 'Access Denied', got 'The specified accesspoint does not exist')
  $.ErrorResponse.Error.AccountId: missing (expected the request's 12-digit account id, got None)
Expected: the authorization refusal, independent of whether the {Name} exists. Actual: the existence refusal, with no AccountId member.

Cause 3 - same fixture, step task#8, operation ListAccessPoints, divergence_key 'service=s3control|ListAccessPoints|status=403->200|keys=$.ErrorResponse,status', affected_count 1:
  status: expected 403, got 200
  $.ErrorResponse: missing (expected {'Error': {'Code': 'AccessDenied', 'Message': 'Access Denied', 'AccountId': <the request's 12-digit account id>}, 'RequestId': <per-run>, 'HostId': <per-run>}, got None)
Expected: HTTP 403 AccessDenied. Actual: HTTP 200 with a ListAccessPointsResult.

Model evidence (smithy_query, services/service-com-amazonaws-s3control-awss3controlservicev20180820/smithy-model/com-amazonaws-s3control-awss3controlservicev20180820.json):
  GetAccessPoint: http GET /v20180820/accesspoint/{Name} code 200; endpoint.hostPrefix '{AccountId}.'; input AccountId @required @httpHeader 'x-amz-account-id' @hostLabel, pattern ^\d{12}$; input Name @httpLabel length 3..255; errors [].
  ListAccessPoints: http GET /v20180820/accesspoint code 200; endpoint.hostPrefix '{AccountId}.'; input AccountId @required @httpHeader 'x-amz-account-id' @hostLabel, pattern ^\d{12}$; errors [].
Because neither operation binds a modeled error, the 403/AccessDenied and 404/NoSuchAccessPoint shapes above come from the recorded responses, not the model.

Account ids, hostnames, ports, RequestId/HostId and timestamps in the recording are per-environment values; the requirements asserted here are the statuses (403 for an unauthorized account identity), the codes ('AccessDenied'), the fixed message text ('Access Denied'), the presence of Error.AccountId filled from the request, and the absence of any result body in a refusal.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus) and look at fixture s3c-account-id-host-prefix-is-authorized-before-the-request.
3. Cause 1 (step task#4): create an access point under the authorized account, then issue GetAccessPoint for it with an account-id host prefix / x-amz-account-id that does not authorize the request. Expect HTTP 403, Error.Code 'AccessDenied', Error.Message 'Access Denied', Error.AccountId = the account id sent on the request, and no GetAccessPointResult; the emulator answers 200 with the full configuration plus the four Endpoints entries.
4. Cause 2 (step task#5): repeat the same unauthorized GetAccessPoint but for an access-point name that does not exist. Expect the identical 403 AccessDenied body (authorization precedes existence, so 403 and 404 are indistinguishable to an unauthorized caller); the emulator answers 404 with Error.Code 'NoSuchAccessPoint', Error.Message 'The specified accesspoint does not exist' and no Error.AccountId.
5. Cause 3 (step task#8): issue ListAccessPoints (GET /v20180820/accesspoint) with the same non-authorizing account identity. Expect HTTP 403 AccessDenied with Error.AccountId; the emulator answers 200 with a ListAccessPointsResult enumerating the access points it holds.
6. After the fix, re-replay the fixture and confirm all three steps: s3c-account-id-host-prefix-is-authorized-before-the-request#task#4, #task#5 and #task#8 must each produce the 403 AccessDenied refusal with the Error.AccountId member and no result body.
