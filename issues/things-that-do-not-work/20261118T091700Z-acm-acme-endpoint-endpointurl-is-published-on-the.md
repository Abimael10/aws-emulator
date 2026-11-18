Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm.acme-endpoint-directory-url-immutable-server-generated acm.acme-endpoint-directory-url-managed-by-service acm.acme-endpoint-server-managed-directory-url acm.describe-acme-endpoint-returns-server-managed-urls
Issue-Shape-Id: com.amazonaws.acm#CertificateManager
Issue-Key: acm.acme-endpoint-directory-url-immutable-server-generated|_assert_directory_url#7|AssertionError|assert labels[:2] == ["acm-acme-enroll", region] and len(labels) > 2 and all(labels[2:]), (
Issue-Key: acm.acme-endpoint-directory-url-managed-by-service|_assert_directory_url#6|AssertionError|assert labels[:2] == ["acm-acme-enroll", region] and len(labels) > 2 and all(labels[2:]), (
Issue-Key: acm.acme-endpoint-server-managed-directory-url|_assert_directory_url#12|AssertionError|assert labels[:2] == ["acm-acme-enroll", region] and len(labels) > 2 and all(labels[2:]), (
Issue-Key: acm.describe-acme-endpoint-returns-server-managed-urls|_assert_directory_url#11|AssertionError|assert labels[:2] == ["acm-acme-enroll", region] and len(labels) > 2 and all(labels[2:]), (
Issue-Commit: 79dfdcc6eb9143691c8a5d9dab7f4566d735ec1d

# ACM ACME endpoint: EndpointUrl is published on the emulator's bare listener authority (https://127.0.0.1:<port>/<id>/directory) instead of the acm-acme-enroll.<region>.<domain> host (1 cause)

> _Note: this issue was identified against the code base at commit `79dfdcc6eb9143691c8a5d9dab7f4566d735ec1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAcmeEndpoint

## Description
Cause 1 of 1: the ACME directory URL the service assigns (AcmeEndpoint.EndpointUrl, minted by CreateAcmeEndpoint and echoed by DescribeAcmeEndpoint/ListAcmeEndpoints) is published with the emulator's raw listener authority as its host, so the URL carries no service label and no region label at all. Failing checks: acm.acme-endpoint-directory-url-immutable-server-generated, acm.acme-endpoint-directory-url-managed-by-service, acm.acme-endpoint-server-managed-directory-url, acm.describe-acme-endpoint-returns-server-managed-urls.

AWS: recorded behavior only as to the exact value. Smithy (com.amazonaws.acm#AcmeEndpoint$EndpointUrl -> String, doc "<p>The URL of the ACME endpoint.</p>", no pattern) and the request shapes establish only that the value is wholly service-assigned: neither CreateAcmeEndpointRequest (IdempotencyToken, AuthorizationBehavior required, Contact default REQUIRED, CertificateAuthority required, Tags, CertificateTags) nor UpdateAcmeEndpointRequest exposes an EndpointUrl member, and CreateAcmeEndpointResponse returns only AcmeEndpointArn, so the caller never supplies any part of the URL. What the recordings show is the template the service assigns: scheme https, host whose first DNS label is literally acm-acme-enroll, whose second label is the request's region, followed by at least one further non-empty domain label, and path exactly /<endpoint-id>/directory with no query and no fragment. The fixed parts of that template are the scheme, the leading label acm-acme-enroll and the trailing path segment /directory; the variable parts are the region (taken from the request), the remaining domain labels (the target's own domain), and <endpoint-id> (a server-generated opaque id, distinct from the ARN suffix). It is stable across repeated DescribeAcmeEndpoint calls and distinct for two endpoints created with byte-identical input.

Emulator: EndpointUrl is returned as https://<listener-host>:<port>/<endpoint-id>/directory — the bare address the emulator is reached on, with no acm-acme-enroll label and no region label. The four checks all fail on the same host assertion, e.g. "AssertionError: EndpointUrl 'https://127.0.0.1:43287/5ee80b28-9536-4ea3-9921-9d8be3f4ca0a/directory' must be on acm-acme-enroll.us-west-2.<domain>" (the particular IP, port and endpoint uuid are per-run values and are not the requirement; the missing service/region labels are). The rest of the object is correct in these runs: the control-plane calls return 200, the endpoint reaches ACTIVE, the ARN matches ^arn:aws[a-z-]*:acm:[a-z0-9-]+:[0-9]{12}:acme-endpoint/[a-zA-Z0-9-]+$, the path is /<id>/directory, the URL is stable across re-describes and distinct per endpoint — only the authority is wrong, so each check fails solely at _assert_directory_url.

Why: a client reads EndpointUrl as the service's published enrollment authority. Anything that derives the ACME host from it — pinning or allow-listing the enrollment hostname, SNI/TLS verification against acm-acme-enroll.<region>.<domain>, splitting the region out of the authority to pick a regional endpoint, or storing the directory URL in config and resolving it later from another host — gets a bare address that is meaningless outside the one process and region-less, so it either refuses the URL or silently binds to a transient host:port. The caller cannot repair it: the host is the service's own assigned value.

Note this is the counterpart of open issue 20261118T015007Z-acm-acme-endpoint-the-published-endpointurl-host-acm.md, which reports the AWS-shaped host being announced but not served (and which itself reopened fixed issue 20261106T080855Z-acm-acme-endpoint-endpointurl-is-published-as-a.md). The two constraints must be satisfied together, not traded against each other: publishing the reachable bare authority fixes reachability and breaks representation, which is the state these four checks now observe. A correct fix announces the acm-acme-enroll.<region>.<domain> authority AND binds the ACME data-plane routes on it, so both the shape checks and the protocol checks hold.

Fix: the ACME endpoint URL minting in the acm crate — whatever composes EndpointUrl when CreateAcmeEndpoint creates the endpoint and stores it in endpoint state for DescribeAcmeEndpoint/ListAcmeEndpoints to serve — must build the host as acm-acme-enroll.<region>.<own domain> rather than the listener's address, keeping the /<endpoint-id>/directory path, and the per-resource listener/route binding must accept that host so the published URL remains reachable.

Traces: acm.acme-endpoint-directory-url-immutable-server-generated, acm.acme-endpoint-directory-url-managed-by-service, acm.acme-endpoint-server-managed-directory-url, acm.describe-acme-endpoint-returns-server-managed-urls.

## Evidence
Expected (AWS, per the recordings): DescribeAcmeEndpoint returns AcmeEndpoint.EndpointUrl = https://acm-acme-enroll.<region>.<domain>/<endpoint-id>/directory — scheme https; first host label exactly "acm-acme-enroll"; second host label the request's region; at least one further non-empty domain label; path exactly /<endpoint-id>/directory; no query, no fragment. Stable across repeated describes; distinct for two endpoints created with identical input.

Actual (emulator), all four checks, same assertion in _assert_directory_url:
  AssertionError: EndpointUrl 'https://127.0.0.1:43287/5ee80b28-9536-4ea3-9921-9d8be3f4ca0a/directory' must be on acm-acme-enroll.us-west-2.<domain>   (acm.acme-endpoint-directory-url-immutable-server-generated)
  AssertionError: EndpointUrl 'https://127.0.0.1:33575/15d18c80-a5a0-4b66-aead-fcfb805e3ecd/directory' must be on acm-acme-enroll.us-west-2.<domain>   (acm.acme-endpoint-directory-url-managed-by-service)
  AssertionError: EndpointUrl 'https://127.0.0.1:40377/6946dbb0-b340-4e75-86fd-6d19db699969/directory' must be on acm-acme-enroll.us-west-2.<domain>   (acm.acme-endpoint-server-managed-directory-url)
  AssertionError: EndpointUrl 'https://127.0.0.1:34641/1a2e4aae-88e8-4da5-9355-02f735352ca9/directory' must be on acm-acme-enroll.us-west-2.<domain>   (acm.describe-acme-endpoint-returns-server-managed-urls)

In those messages the fixed text is the required template "acm-acme-enroll.<region>.<domain>" with the path /<id>/directory; the IP address, the port and the endpoint uuid are per-run values taken from the emulator's own published EndpointUrl and are NOT part of the requirement. The divergence is that the host carries neither the acm-acme-enroll label nor the region label.

Everything else in these runs is correct: CreateAcmeEndpoint and DescribeAcmeEndpoint answer 200, Status reaches ACTIVE, AcmeEndpointArn matches the modeled pattern, CreatedAt is a timestamp, the two endpoints created from identical input get distinct ARNs and distinct URLs, and re-describes return the identical URL — so the failures isolate the authority alone.

Smithy: com.amazonaws.acm#AcmeEndpoint$EndpointUrl -> String (no pattern); CreateAcmeEndpointRequest / UpdateAcmeEndpointRequest expose no EndpointUrl member; CreateAcmeEndpointResponse returns only AcmeEndpointArn — the URL is entirely service-assigned.

## Steps to Reproduce
1. acm.create_acme_endpoint(AuthorizationBehavior="PRE_APPROVED", Contact="NOT_REQUIRED", CertificateAuthority={"PublicCertificateAuthority": {}}, Tags=[{"Key": "Name", "Value": "<unique>"}]) -> AcmeEndpointArn (200).
2. Poll acm.describe_acme_endpoint(AcmeEndpointArn=arn)["AcmeEndpoint"] until Status == "ACTIVE" and EndpointUrl is populated.
3. Read EndpointUrl and split its authority: the emulator returns https://<listener-ip>:<port>/<endpoint-id>/directory, so hostname.split(".")[:2] is not ["acm-acme-enroll", "<region>"] — AWS returns https://acm-acme-enroll.<region>.<domain>/<endpoint-id>/directory.
4. Create a second endpoint with byte-identical input and describe it: both URLs are distinct and stable (correct), but both carry the same wrong authority, confirming the defect is in how the host is composed and not in the per-endpoint id.
5. Re-replay the four checks in trace_ids; each fails only at the host assertion in _assert_directory_url.
