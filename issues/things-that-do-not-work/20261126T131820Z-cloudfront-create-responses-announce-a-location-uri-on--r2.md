Issue-Source: trace-sweep-divergence
Issue-Trace: cfdeep-invalidation-pagination-walk
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cfdeep-invalidation-pagination-walk cloudfront-distribution-create-invalidate-monitor-webacl cloudfront-icc-caller-reference-retry-and-conflict cloudfront-icc-distribution-identifier-mistakes cloudfront-icc-path-format-mistakes cloudfront-icc-per-object-batch-too-large cloudfront-icc-readback-identifier-and-paging-mistakes cloudfront-icc-reference-invalidation-settles-completed cloudfront-icc-sdk-batch-construction-mistakes cloudfront-invalidation-create-get-list cloudfront-invalidation-lifecycle cloudfront-connection-function-publish-lifecycle cloudfront-distribution-tenant-create-and-invalidate
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|CreateInvalidation|2xx->2xx|$headers.location
Issue-Key: cloudfront|CreateConnectionFunction|2xx->2xx|$headers.location
Issue-Key: cloudfront|CreateInvalidationForDistributionTenant|2xx->2xx|$headers.location
Issue-Commit: 94bcd943a3c4e0346859674a6a27263b9bdf544a

# CloudFront create responses announce a Location URI on the emulator's own listener instead of the service endpoint

> _Note: this issue was identified against the code base at commit `94bcd943a3c4e0346859674a6a27263b9bdf544a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInvalidation

## Description
Three net-new causes, one shared fault: three CloudFront create operations build the `Location` response header from the socket the emulator happens to be bound to instead of from the CloudFront service endpoint the request was addressed to, so the URI they hand back does not address the resource they just created. In all three the HTTP status (201) and the whole response payload match; the path portion of the URI is correct (the modelled create URI extended with the new resource's identifier); only the URI's AUTHORITY is wrong, and it is wrong the same way in all three, which places the fault in the shared Location-URI derivation rather than in any one handler. The announced port is not even the port the emulator is reached on: this replay's data listener accepts on :36211, while the three causes announce :36245, :40865 and :35749 respectively, so the URI is unaddressable even inside the emulator.

The concrete hosts, ports and generated identifiers quoted below are per-run values and are NOT themselves requirements; the requirement is the FORM of the member -- an https URI whose authority is a resolvable service-endpoint host (no port) and whose last path segment is the created resource's own identifier.

Related open issues that do NOT cover these three (the replay reports all three clusters unowned, so none of them carries these replay keys): 20261114T142904Z-cloudfront-reopens-generated-identifiers-and-published-domain-names.md reports this authority fault on CreateResponseHeadersPolicy only, and 20261109T120741Z-cloudfront-create-responses-announce-a-location-uri-on.md covers the Location of ten OTHER create operations. CreateInvalidation, CreateConnectionFunction and CreateInvalidationForDistributionTenant are in neither.

Cause 1 of 3: CreateInvalidation's Location does not address the new invalidation.
AWS: Smithy (cloudfront 2020-05-31) CreateInvalidation -- DistributionId @required @httpLabel, InvalidationBatch @required @httpPayload, http {method POST, uri /2020-05-31/distribution/{DistributionId}/invalidation, code 201}; output member Location is bound to the `Location` httpHeader and Invalidation.Id is @required ('The identifier for the invalidation request. For example: IDFDVBD632BHDS5'). Recorded behavior: the 201 carries Location = 'https://cloudfront.amazonaws.com/2020-05-31/distribution/<distribution id>/invalidation/<invalidation id>' -- scheme https, authority the CloudFront service endpoint host with no port, path the modelled create URI plus the new invalidation's id.
Emulator: emits 'https://127.0.0.1:<ephemeral port>/2020-05-31/distribution/<distribution id>/invalidation/<invalidation id>' and the step diverges with verdict resource_not_identified on $headers.location. The authority is a bare IPv4 literal with a port appended; it is not a host a client can resolve, and the port is not the one the data listener answers on.
Why: a client that follows the Location header to poll GetInvalidation until Status=Completed (the documented create-then-poll pattern for invalidations) builds a request against an address AWS would never have returned and that the emulator itself does not answer on, so the invalidation can never be observed to settle; a client that merely validates or stores the URI rejects the response.
Fix: CreateInvalidation's response construction in the cloudfront operation handler, and the shared Location-URI builder it calls -- derive the authority from the service endpoint the request was addressed to (the request's Host/endpoint resolution), never from the bound socket address.
Traces: cfdeep-invalidation-pagination-walk#task#2 (one cluster, 12 affected steps across 12 fixtures, incl. cloudfront-invalidation-lifecycle#task#2 and cloudfront-icc-reference-invalidation-settles-completed#task#2).

Cause 2 of 3: CreateConnectionFunction's Location does not address the new connection function.
AWS: Smithy CreateConnectionFunction -- Name @required (@length 1-64, pattern ^[a-zA-Z0-9-_]{1,64}$), ConnectionFunctionConfig @required, ConnectionFunctionCode @required, http {method POST, uri /2020-05-31/connection-function, code 201}; output members Location (`Location` httpHeader), ETag (`ETag` httpHeader) and ConnectionFunctionSummary (@httpPayload, with Id and ConnectionFunctionArn both @required). Recorded behavior: Location = 'https://cloudfront.amazonaws.com/2020-05-31/connection-function/<connection function ARN>' -- same authority form, with the new function's ARN as the trailing path segment.
Emulator: emits 'https://127.0.0.1:<ephemeral port>/2020-05-31/connection-function/<ARN>'; verdict resource_not_identified on $headers.location.
Why: the connection-function publish lifecycle is create -> poll DescribeConnectionFunction -> publish; a caller that uses the returned Location as the resource's address cannot perform any of those follow-ups, and an SDK that normalises the URI to extract the resource reference gets a host it cannot connect to.
Fix: as cause 1, on CreateConnectionFunction's response construction via the same shared Location-URI builder.
Traces: cloudfront-connection-function-publish-lifecycle#task#1.

Cause 3 of 3: CreateInvalidationForDistributionTenant's Location does not address the new tenant invalidation.
AWS: Smithy CreateInvalidationForDistributionTenant -- Id @required @httpLabel, InvalidationBatch @required @httpPayload, http {method POST, uri /2020-05-31/distribution-tenant/{Id}/invalidation, code 201}; output member Location bound to the `Location` httpHeader. Recorded behavior: Location = 'https://cloudfront.amazonaws.com/2020-05-31/distribution-tenant/<tenant id>/invalidation/<invalidation id>'.
Emulator: emits 'https://127.0.0.1:<ephemeral port>/2020-05-31/distribution-tenant/<tenant id>/invalidation/<invalidation id>'; verdict resource_not_identified on $headers.location. Note that the tenant id in the path matches the recording exactly here (it is echoed from the request label), which isolates the failing part of the value to the authority rather than to any generated id.
Why: as cause 1, on the multi-tenant invalidation path -- the tenant-scoped invalidation cannot be polled through the address its own create returned.
Fix: as cause 1, on CreateInvalidationForDistributionTenant's response construction. Fixing the shared builder once should close all three; re-replay all three fixtures, because the three ops reach it from different URI templates (nested path, ARN segment, tenant-scoped nested path).
Traces: cloudfront-distribution-tenant-create-and-invalidate#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, /__admin/health answered 200, data listener accepts on :36211; 202 fixtures replayed, 1411 steps, 1329 passed, score 94.2%, 31 divergence clusters. These three clusters are reported with no blocking stub (stub_path empty) and no owning open issue (owned_by empty).

Cause 1 -- fixture cfdeep-invalidation-pagination-walk, step task#2, CreateInvalidation (201 -> 201, 12 affected steps: cfdeep-invalidation-pagination-walk#task#2, cfdeep-invalidation-pagination-walk#task#3, cloudfront-distribution-create-invalidate-monitor-webacl#task#3, cloudfront-icc-caller-reference-retry-and-conflict#task#2, cloudfront-icc-distribution-identifier-mistakes#task#5, cloudfront-icc-path-format-mistakes#task#5, cloudfront-icc-per-object-batch-too-large#task#3, cloudfront-icc-readback-identifier-and-paging-mistakes#task#2, cloudfront-icc-reference-invalidation-settles-completed#task#2, cloudfront-icc-sdk-batch-construction-mistakes#task#6, cloudfront-invalidation-create-get-list#task#2, cloudfront-invalidation-lifecycle#task#2):
  expected: $headers.location = 'https://cloudfront.amazonaws.com/2020-05-31/distribution/E7IYCGPKTANSF3/invalidation/IBM5B2ULLUSKHZAOLO538EHQTE'
  actual:   $headers.location = 'https://127.0.0.1:36245/2020-05-31/distribution/E7IYCGPKTANSF3/invalidation/IJZ50Z92T9DP3GZNI4PND7H2SV'  (verdict resource_not_identified)
  divergence_key: service=cloudfront|CreateInvalidation|status=201->201|keys=$headers.location

Cause 2 -- fixture cloudfront-connection-function-publish-lifecycle, step task#1, CreateConnectionFunction (201 -> 201, 1 affected step):
  expected: $headers.location = 'https://cloudfront.amazonaws.com/2020-05-31/connection-function/arn:aws:cloudfront::000000000000:connection-function/cf_3Lao2PJV4a8S4gzBsty1rri5cTR'
  actual:   $headers.location = 'https://127.0.0.1:40865/2020-05-31/connection-function/arn:aws:cloudfront::000000000000:connection-function/cf_lwnQgf8efPuSTisCCC2UXNPjc43'  (verdict resource_not_identified)
  divergence_key: service=cloudfront|CreateConnectionFunction|status=201->201|keys=$headers.location

Cause 3 -- fixture cloudfront-distribution-tenant-create-and-invalidate, step task#5, CreateInvalidationForDistributionTenant (201 -> 201, 1 affected step):
  expected: $headers.location = 'https://cloudfront.amazonaws.com/2020-05-31/distribution-tenant/dt_YnhSdmG6Nxz9M2BnSLnOc4C12MA/invalidation/I7C8ZPV5CUORWZJ8H49T7EODL8'
  actual:   $headers.location = 'https://127.0.0.1:35749/2020-05-31/distribution-tenant/dt_YnhSdmG6Nxz9M2BnSLnOc4C12MA/invalidation/I7THQC25A0YN0L00PA318JRIWE'  (verdict resource_not_identified)
  divergence_key: service=cloudfront|CreateInvalidationForDistributionTenant|status=201->201|keys=$headers.location

Model evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json):
  CreateInvalidation -- POST /2020-05-31/distribution/{DistributionId}/invalidation, code 201; output {Location -> httpHeader 'Location', Invalidation -> httpPayload}; Invalidation.Id @required.
  CreateConnectionFunction -- POST /2020-05-31/connection-function, code 201; output {ConnectionFunctionSummary -> httpPayload, Location -> httpHeader 'Location', ETag -> httpHeader 'ETag'}; ConnectionFunctionSummary.Id and .ConnectionFunctionArn @required.
  CreateInvalidationForDistributionTenant -- POST /2020-05-31/distribution-tenant/{Id}/invalidation, code 201; output {Location -> httpHeader 'Location', Invalidation -> httpPayload}.

Fixed vs variable parts of the value: the fixed text is the scheme, the '/2020-05-31/...' path template and the separators; the variable parts are the service-endpoint authority (resolved per environment) and the resource identifiers / ARN taken from the request label and from the record just created. The recorded hosts, ports, invalidation ids and connection-function ids are per-run values and are not part of the requirement -- the emitted authority form is. In cause 3 the tenant id matches byte for byte, which demonstrates that the id segments are bound by the replay and that the surviving failure is the authority.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout; replay the wire corpus (replay_wire_corpus, trace set simparity-wire-aws-v1). Note the data listener address reported in the summary (here :36211).
2. Cause 1: replay fixture cfdeep-invalidation-pagination-walk and observe step task#2 -- POST /2020-05-31/distribution/{DistributionId}/invalidation, then read the Location response header. Its authority is '127.0.0.1:<port>' instead of the CloudFront service endpoint host, and the port is not the port the emulator accepts on. 11 further steps reproduce it, e.g. cloudfront-invalidation-lifecycle#task#2.
3. Cause 2: replay fixture cloudfront-connection-function-publish-lifecycle and observe step task#1 -- POST /2020-05-31/connection-function; the Location header has the same IP-literal:port authority ahead of the correct '/2020-05-31/connection-function/<ARN>' path.
4. Cause 3: replay fixture cloudfront-distribution-tenant-create-and-invalidate and observe step task#5 -- POST /2020-05-31/distribution-tenant/{Id}/invalidation; same authority fault, with the tenant id segment matching the recording exactly.
5. For each, compare only the authority of $headers.location: the status, payload and path template already match, so the fix is in the Location-URI authority derivation. Confirm by taking the Location the emulator returns and issuing the corresponding GET against it -- it must be an address the emulator serves the resource on.
