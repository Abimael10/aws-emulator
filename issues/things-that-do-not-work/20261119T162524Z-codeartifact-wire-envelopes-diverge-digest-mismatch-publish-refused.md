Issue-Source: trace-sweep-divergence
Issue-Trace: codeartifact-generic-publish-validation-envelopes
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: codeartifact-generic-publish-validation-envelopes codeartifact-authorization-token-and-repository-endpoint
Issue-Shape-Id: com.amazonaws.codeartifact#CodeArtifactControlPlaneService
Issue-Key: codeartifact|PublishPackageVersion|4xx->4xx|$.message
Issue-Key: codeartifact|PublishPackageVersion|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: codeartifact|PublishPackageVersion|4xx->4xx|status
Issue-Key: codeartifact|GetRepositoryEndpoint|2xx->2xx|$.repositoryEndpoint
Issue-Commit: 1a6df7b32e4c8350e561c708bcd111a16150a036

# codeartifact wire envelopes diverge: digest-mismatch publish refused as 403 SignatureDoesNotMatch and GetRepositoryEndpoint returns a non-AWS URL shape

> _Note: this issue was identified against the code base at commit `1a6df7b32e4c8350e561c708bcd111a16150a036`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PublishPackageVersion

## Description
Wire replay of simparity-wire-aws-v1 against the emulator booted from this checkout (34 fixtures, 344 steps, 312 passed, 90.7%) leaves two codeartifact divergence clusters that no open issue owns by replay key. Both are response-shaping defects on the codeartifact control plane: an operation-level validation failure is answered by the generic request-signing layer instead of the operation, and an operation whose whole purpose is to publish a usable URL publishes one in a shape no AWS client parses. THIS ISSUE HAS 2 CAUSES; both must be closed before submitting.

Cause 1 of 2: PublishPackageVersion with an assetSHA256 that disagrees with the uploaded bytes is refused 403 SignatureDoesNotMatch instead of 400 ValidationException (fixture codeartifact-generic-publish-validation-envelopes, step task#5).
AWS: Smithy PublishPackageVersion (com.amazonaws.codeartifact#CodeArtifactControlPlaneService) is POST /v1/package/version/publish, success code 200. It binds assetSHA256 -> SHA256 to the HTTP header x-amz-content-sha256, @required, @length(min 64, max 64), @pattern ^[0-9a-f]+$, alongside the @required streaming @httpPayload member assetContent -> Asset. Its modeled errors are AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ServiceQuotaExceededException 402, ThrottlingException 429, ValidationException 400 — SignatureDoesNotMatch is not modeled on this operation at all, and 403 is reserved for AccessDeniedException. A declared digest that disagrees with the body is therefore an application-layer refusal: the request authenticates, the service hashes the bytes it received, compares them with the header value, stores nothing and refuses with ValidationException on HTTP 400. The recorded response gives the message: fixed text, exactly, "The provided content hashes do not match the calculated content hashes. Hash algorithm 'SHA-256', Expected hash '" + <declared> + "', Actual hash '" + <computed> + "'" (no trailing period). The ONLY variable parts are the two lowercase-hex digests: <declared> is the assetSHA256 the request sent on x-amz-content-sha256, <computed> is the SHA-256 the service computed over the uploaded bytes. The error type is carried in the x-amzn-errortype header as ValidationException with the control-plane Coral shape suffix the recording shows. The two digests quoted under Evidence are one run's values filling that template, not part of the requirement.
Emulator: HTTP 403, x-amzn-errortype: SignatureDoesNotMatch, message 'The request signature does not match'. No content-hash comparison happens and the computed digest is never named; the refusal is issued before the operation is dispatched.
Why: SDKs and build tools classify SignatureDoesNotMatch as a credential or clock problem — they re-sign, refresh credentials, or alarm on an authorization failure. A publisher whose upload was corrupted is told its signing is broken, never reaches the 400/ValidationException branch where it would recompute the digest and retry the upload, and loses the only evidence (the computed digest) that the service hashed the payload instead of trusting the header.
Fix: the generic SigV4 payload-hash verification in the request pipeline must treat a member-bound 64-char lowercase-hex x-amz-content-sha256 as the precomputed signed payload hash for this operation rather than re-deriving it from the streaming body, so the request authenticates; the digest comparison and the modeled ValidationException with the sentence above then belong in the PublishPackageVersion handler's validation in the codeartifact service crate (services/service-com-amazonaws-codeartifact-codeartifactcontrolplaneservice/src/ops/publish_package_version.rs per earlier analysis — verify against the current tree). A rejected publish must also store nothing.
Related open issues that do NOT own this wire step (both are behavioral-corpus findings keyed on check ids, and this replay reported this cluster unowned): 20261101T084500Z-codeartifact-publishpackageversion-digest-mismatch-refused-as-signaturedoesnotmatch.md and 20261103T235912Z-codeartifact-reopens-publishpackageversion-with-a-mismatched-assetsha256-is-still.md. The second records that a previous fix left the call refused, so verify end to end by replaying rather than by reading the handler.

Cause 2 of 2: GetRepositoryEndpoint returns a repositoryEndpoint whose path is a synthetic emulator route instead of the AWS endpoint shape (fixture codeartifact-authorization-token-and-repository-endpoint, step task#3; the same cluster also fails at task#4).
AWS: Smithy GetRepositoryEndpoint is GET /v1/repository/endpoint, success 200, with @required httpQuery inputs domain (DomainName, length 2-50, pattern ^[a-z][a-z0-9\-]{0,48}[a-z0-9]$), repository (RepositoryName, length 2-100, pattern ^[A-Za-z0-9][A-Za-z0-9._\-]{1,99}$) and format (PackageFormat enum), optional domain-owner (AccountId, ^[0-9]{12}$) and endpointType (EndpointType enum); output GetRepositoryEndpointResult$repositoryEndpoint (String), documented as the URL of the returned endpoint. The returned URL is the address a package manager publishes to and installs from, so its STRUCTURE is the contract: the domain, the 12-digit domain owner and the region qualify the authority, and the path is exactly /<format>/<repository>/ — in the recorded response https://<domain>-<domainOwner>.d.codeartifact.<region>.amazonaws.com/<format>/<repository>/, with trailing slash. domain, domainOwner, region, format and repository are the variable parts, taken from the request and the domain's state; a concrete host name and port are environment-specific and are NOT the requirement.
Emulator: 200 with a URL carrying extra synthetic path segments ahead of the format: '<scheme>://<host>:<port>/_aws/codeartifact/<region>/ipv4/<domain>-<domainOwner>/<format>/<repository>/' — the domain/owner/region qualification has been moved out of the authority into an /_aws/codeartifact/.../ipv4/ path prefix that is not part of the API.
Why: the endpoint is consumed by npm, pip and twine configuration, not just by assertions. A client that takes repositoryEndpoint and appends the package-plane paths AWS documents (\"<endpoint><package>\", \"<endpoint>simple/\") builds URLs against a prefix no AWS-shaped client or recorded fixture expects, and any consumer that parses the endpoint to recover the domain, domain owner or region from the authority reads the wrong values. The correct direction is for the authority to be one the emulator actually serves while the modeled path shape /<format>/<repository>/ and the domain/owner/region qualification are preserved — the emulator currently diverges on the path, so neither the recorded shape nor the published-URL requirement is met.
Fix: the endpoint/URL synthesis in the GetRepositoryEndpoint handler's response construction in the codeartifact service crate, plus the repository data-plane routing that must answer on whatever authority is announced (so the /_aws/... prefix is not needed to route), honouring endpointType.
Related open issue that does NOT own this wire step (behavioral corpus, keyed on check ids; this cluster was reported unowned): 20261109T164005Z-codeartifact-getrepositoryendpoint-publishes-a-real-aws-d-codeartifact.md, which requires the announced authority to be the emulator's own host and port while keeping the /<format>/<repository>/ path shape.

Traces: see Evidence; the fixture#step bindings are codeartifact-generic-publish-validation-envelopes#task#5 (Cause 1) and codeartifact-authorization-token-and-repository-endpoint#task#3 (Cause 2, cluster also covering task#4).

## Evidence
Wire replay summary: trace_set simparity-wire-aws-v1, emulator_up true, 34 fixtures replayed, 344 steps, 312 passed, 32 mismatched, 0 errored, 0 blocked, score 90.7%, 10 divergence clusters (8 already owned by open issues or filed as stubs; the 2 below are not).

Cause 1 — [mismatch] codeartifact PublishPackageVersion, fixture codeartifact-generic-publish-validation-envelopes, step task#5, cluster key service=codeartifact|PublishPackageVersion|status=400->403|keys=$.message,$headers.x-amzn-errortype,status:
  expected: HTTP 400, $headers.x-amzn-errortype = 'ValidationException:http://internal.amazon.com/coral/com.amazonaws.codeartifact.controlplane/', $.message = "The provided content hashes do not match the calculated content hashes. Hash algorithm 'SHA-256', Expected hash '0feaf5309dafd2a0254c72595bfe9a4f7ca44106ace6fcd771ac0556ed4c68e2', Actual hash '8b4b7c18a4c6d84ee8d5b349495483c0a38ab030e5afa3a33cceb8ba35e87f96'"
  actual:   HTTP 403, $headers.x-amzn-errortype = 'SignatureDoesNotMatch', $.message = 'The request signature does not match'
  The two hex digests above are that run's values: <declared> = the request's x-amz-content-sha256, <computed> = the SHA-256 of the bytes actually uploaded. Everything else in the sentence is fixed text.

Cause 2 — [mismatch] codeartifact GetRepositoryEndpoint, fixture codeartifact-authorization-token-and-repository-endpoint, step task#3 (cluster also affects task#4), cluster key service=codeartifact|GetRepositoryEndpoint|status=200->200|keys=$.repositoryEndpoint:
  status matches (200 both sides); $.repositoryEndpoint value_mismatch
  expected: 'https://<domain>-<domainOwner>.d.codeartifact.<region>.amazonaws.com/<format>/<repository>/' — recorded as 'https://sim2real-probe-ca-dom14-fbfc55f2-000000000000.d.codeartifact.us-west-2.amazonaws.com/generic/sim2real-probe-ca-repo14-fbfc55f2/'
  actual:   '<scheme>://<host>:<port>/_aws/codeartifact/<region>/ipv4/<domain>-<domainOwner>/<format>/<repository>/' — recorded as 'https://127.0.0.1:36153/_aws/codeartifact/us-west-2/ipv4/sim2real-probe-ca-dom14-fbfc55f2-000000000000/generic/sim2real-probe-ca-repo14-fbfc55f2/'
  The host 127.0.0.1, the port 36153 and the generated domain/repository names are per-run values and are not requirements; the divergence that is a requirement is the path shape — AWS's path is exactly /<format>/<repository>/ with the domain, domain owner and region qualifying the authority, and the emulator inserts /_aws/codeartifact/<region>/ipv4/<domain>-<domainOwner> ahead of it.

## Steps to Reproduce
Boot the emulator from this checkout and replay the simparity-wire-aws-v1 corpus; both clusters reproduce. By hand with a codeartifact client pointed at the emulator:

Cause 1 (fixture codeartifact-generic-publish-validation-envelopes, step task#5):
1. create_domain(domain=D); create_repository(domain=D, repository=R).
2. payload = b'<bytes actually sent>'; other = b'<different bytes>'.
3. publish_package_version(domain=D, repository=R, format='generic', namespace=NS, package=P, packageVersion='1.0.0', assetName='payload.bin', assetContent=payload, assetSHA256=sha256(payload).hexdigest()) -> 200 (positive control: the handler serves the operation).
4. publish_package_version(... packageVersion='2.0.0', assetContent=payload, assetSHA256=sha256(other).hexdigest()) -> expect HTTP 400, x-amzn-errortype ValidationException (control-plane Coral suffix), message "The provided content hashes do not match the calculated content hashes. Hash algorithm 'SHA-256', Expected hash '<sha256(other)>', Actual hash '<sha256(payload)>'"; observe HTTP 403 SignatureDoesNotMatch / 'The request signature does not match'.
5. list_package_versions(domain=D, repository=R, format='generic', namespace=NS, package=P) -> only '1.0.0': the refused publish must store nothing.

Cause 2 (fixture codeartifact-authorization-token-and-repository-endpoint, steps task#3 and task#4):
1. create_domain(domain=D); create_repository(domain=D, repository=R).
2. get_authorization_token(domain=D).
3. get_repository_endpoint(domain=D, repository=R, format='generic') -> 200; inspect repositoryEndpoint.
4. Observe the path begins /_aws/codeartifact/<region>/ipv4/<domain>-<domainOwner>/ before /generic/<repository>/, where AWS's URL path is exactly /<format>/<repository>/ with domain, domain owner and region qualifying the authority. Repeat with format='npm' and format='pypi' and with an explicit domain-owner.
