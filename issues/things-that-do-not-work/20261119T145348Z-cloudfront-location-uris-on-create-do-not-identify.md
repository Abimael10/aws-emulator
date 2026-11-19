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
Issue-Commit: a0a325cd7a8f100e0b15b3c52772a25e70aa00e8

# CloudFront Location URIs on create do not identify the resource the way AWS does

> _Note: this issue was identified against the code base at commit `a0a325cd7a8f100e0b15b3c52772a25e70aa00e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInvalidation

## Description
THREE (3) distinct net-new causes, one shared fault: the Location response header the CloudFront emulator builds on a create does not have AWS's form — its authority is the socket the request happened to arrive on rather than the service endpoint, and on one route the resource is named in the path by the wrong identifier. All three must be closed before this issue is submitted. Replay reports each cluster unowned (no stub_path, no owned_by). The nearest open issue, 20261114T142904Z-cloudfront-reopens-generated-identifiers-and-published-domain-names.md, carries the Location key for CreateResponseHeadersPolicy ONLY (plus S3CanonicalUserId and Distribution.DomainName) and does not carry any of the three keys below. Throughout: the concrete host, port, invalidation id and function id quoted are per-run values and are NOT the requirement; the requirement is the URI's form — a resolvable service-endpoint authority plus the identifier AWS uses for that resource type.

=== Cause 1 of 3: CreateInvalidation's Location announces an IP:port authority instead of the service endpoint (12 affected steps) ===
AWS: Smithy: CreateInvalidation http {method POST, uri /2020-05-31/distribution/{DistributionId}/invalidation, code 201}; output member Location is bound to the `Location` httpHeader and Invalidation is the httpPayload. AWS answers 201 with Location = <service endpoint>/2020-05-31/distribution/<DistributionId>/invalidation/<invalidation id>. Fixed text: the scheme+authority of the CloudFront service endpoint (the recording shows https://cloudfront.amazonaws.com) and the path literals /2020-05-31/distribution/ and /invalidation/. Variable parts: DistributionId taken from the request, and the invalidation id taken from the resource just created (the same value the body's Invalidation.Id carries).
Emulator: 201 whose Location is https://127.0.0.1:<port>/2020-05-31/distribution/<DistributionId>/invalidation/<id> — the authority is a bare IP literal with a port, not a resolvable service endpoint, and replay classifies $headers.location as resource_not_identified. The path literals and the DistributionId do match.
Why: a client that follows Location to GET or poll the new invalidation, or that stores/validates the URI, builds an address AWS would never have returned; an IP:port authority also carries no DNS form for a client that re-signs the follow-up request against the service endpoint.
Fix: CreateInvalidation's response construction in the cloudfront invalidation handler — derive the Location authority from the configured service endpoint the emulator publishes, not from the request's socket.
Traces: cfdeep-invalidation-pagination-walk#task#2 (representative); 11 further steps in the same cluster, incl. cloudfront-invalidation-lifecycle#task#2, cloudfront-icc-path-format-mistakes#task#5.

=== Cause 2 of 3: CreateConnectionFunction's Location names the bare function id where AWS names the function's ARN ===
AWS: Smithy: CreateConnectionFunction http {method POST, uri /2020-05-31/connection-function, code 201}; output Location bound to the `Location` httpHeader, ConnectionFunctionSummary the httpPayload. ConnectionFunctionSummary declares BOTH Id (required, "The connection function ID") and ConnectionFunctionArn (required, "The connection function Amazon Resource Name (ARN)"). The recording shows AWS identifying the new function in the Location path by its ARN: <service endpoint>/2020-05-31/connection-function/arn:aws:cloudfront::<account id>:connection-function/<function id>. Fixed text: the service-endpoint authority, /2020-05-31/connection-function/, and the arn:aws:cloudfront:: ... :connection-function/ ARN skeleton; variable parts: the account id and the function id of the created resource.
Emulator: 201 whose Location is https://127.0.0.1:<port>/2020-05-31/connection-function/<function id> — the ARN is absent entirely, so the last path segment is a different kind of identifier than AWS returns, on top of the same IP:port authority as cause 1.
Why: a client that recovers the function's ARN (and the owning account) by parsing Location, or that follows Location to address the function, gets neither — the URI names a resource identifier AWS does not use there, so ARN-based wiring (tagging, policies, cross-references) built from the create response is wrong.
Fix: CreateConnectionFunction's response construction — build the Location path from the connection function's ARN (the same value published as ConnectionFunctionSummary.ConnectionFunctionArn), plus the shared service-endpoint authority of cause 1.
Traces: cloudfront-connection-function-publish-lifecycle#task#1.

=== Cause 3 of 3: CreateInvalidationForDistributionTenant's Location announces the same IP:port authority ===
AWS: Smithy: CreateInvalidationForDistributionTenant http {method POST, uri /2020-05-31/distribution-tenant/{Id}/invalidation, code 201}; output Location bound to the `Location` httpHeader. AWS answers 201 with Location = <service endpoint>/2020-05-31/distribution-tenant/<Id>/invalidation/<invalidation id>, with the tenant Id taken from the request and the invalidation id from the created resource.
Emulator: 201 with https://127.0.0.1:<port>/2020-05-31/distribution-tenant/<Id>/invalidation/<id>; $headers.location is classified resource_not_identified. Path literals and the tenant id match.
Why: as cause 1, on the multi-tenant invalidation route — following or validating Location fails for tenant invalidations too. It is a separate operation with its own response-construction path, so it must be verified separately.
Fix: CreateInvalidationForDistributionTenant's response construction (share the Location-derivation helper with CreateInvalidation).
Traces: cloudfront-distribution-tenant-create-and-invalidate#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator_up=true, /__admin/health 200, data listener accepting; 1411 steps, 1283 passed, score 90.9%). Three unowned mismatch clusters, all with stub_path="" and owned_by=[]:

Cause 1 — CreateInvalidation 201->201, key $headers.location, verdict mismatch/resource_not_identified (cfdeep-invalidation-pagination-walk#task#2, 12 affected steps): expected 'https://cloudfront.amazonaws.com/2020-05-31/distribution/E7IYCGPKTANSF3/invalidation/IBM5B2ULLUSKHZAOLO538EHQTE', got 'https://127.0.0.1:43839/2020-05-31/distribution/E7IYCGPKTANSF3/invalidation/IJZ50Z92T9DP3GZNI4PND7H2SV'. The distribution id segment is identical; the authority is an IP:port instead of the service endpoint. (The differing invalidation id is a per-run generated value and both forms are I+25 uppercase alphanumerics, so the id is not the finding.) Other affected steps: cfdeep-invalidation-pagination-walk#task#3, cloudfront-distribution-create-invalidate-monitor-webacl#task#3, cloudfront-icc-caller-reference-retry-and-conflict#task#2, cloudfront-icc-distribution-identifier-mistakes#task#5, cloudfront-icc-path-format-mistakes#task#5, cloudfront-icc-per-object-batch-too-large#task#3, cloudfront-icc-readback-identifier-and-paging-mistakes#task#2, cloudfront-icc-reference-invalidation-settles-completed#task#2, cloudfront-icc-sdk-batch-construction-mistakes#task#6, cloudfront-invalidation-create-get-list#task#2, cloudfront-invalidation-lifecycle#task#2.

Cause 2 — CreateConnectionFunction 201->201, key $headers.location, verdict mismatch/resource_not_identified (cloudfront-connection-function-publish-lifecycle#task#1): expected 'https://cloudfront.amazonaws.com/2020-05-31/connection-function/arn:aws:cloudfront::000000000000:connection-function/cf_3Lao2PJV4a8S4gzBsty1rri5cTR', got 'https://127.0.0.1:42471/2020-05-31/connection-function/cf_lwnQgf8efPuSTisCCC2UXNPjc43'. Beyond the authority, the recorded path carries the function's ARN before the id segment and the emulator's carries only the bare id.

Cause 3 — CreateInvalidationForDistributionTenant 201->201, key $headers.location, verdict mismatch/resource_not_identified (cloudfront-distribution-tenant-create-and-invalidate#task#5): expected 'https://cloudfront.amazonaws.com/2020-05-31/distribution-tenant/dt_YnhSdmG6Nxz9M2BnSLnOc4C12MA/invalidation/I7C8ZPV5CUORWZJ8H49T7EODL8', got 'https://127.0.0.1:44161/2020-05-31/distribution-tenant/dt_YnhSdmG6Nxz9M2BnSLnOc4C12MA/invalidation/I7THQC25A0YN0L00PA318JRIWE'. Tenant id segment identical; authority is an IP:port.

Note the announced ports (43839, 42471, 44161) differ per fixture and none equals the emulator's data port reported by the replay summary (38379), so the announced authority is not even the one the emulator is reached on.

## Steps to Reproduce
1. git clone the repo, boot the emulator built from that checkout, and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Representative step: fixture cfdeep-invalidation-pagination-walk, step task#2 — POST /2020-05-31/distribution/<id>/invalidation and read the Location response header. Compare its scheme+authority against the recording's service endpoint and its path against /2020-05-31/distribution/<DistributionId>/invalidation/<Invalidation.Id>; compare the id in Location with Invalidation.Id in the body.
3. Fixture cloudfront-connection-function-publish-lifecycle, step task#1 — POST /2020-05-31/connection-function and read Location: the recording's path carries arn:aws:cloudfront::<account>:connection-function/<id>, the emulator's carries only <id>.
4. Fixture cloudfront-distribution-tenant-create-and-invalidate, step task#5 — POST /2020-05-31/distribution-tenant/<id>/invalidation and read Location: same IP:port authority.
5. Compare the FORM, not the literal strings: the hosts/ports and the generated invalidation/function ids are per-run; templating a recorded URI is not a fix.
