Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks.eks-oidc-discovery-and-jwks-verify-a-projected-token-offline eks.ekscc-create-cluster-applies-documented-defaults
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks.eks-oidc-discovery-and-jwks-verify-a-projected-token-offline|run#15|AssertionError|assert disc_status == 200, (
Issue-Key: eks.ekscc-create-cluster-applies-documented-defaults|run#14|AssertionError|assert net["serviceIpv4Cidr"] == want_cidr, (
Issue-Commit: ba9169dcb3a6b6883b8d60843b0b5f765354b232

# eks: 2 net-new root causes — the cluster's advertised OIDC issuer serves no discovery document (router answers 404 UnknownService), and CreateCluster's default serviceIpv4Cidr ignores the VPC CIDR

> _Note: this issue was identified against the code base at commit `ba9169dcb3a6b6883b8d60843b0b5f765354b232`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster

## Description
Two net-new root causes in the eks service, both grounded in failing behavioral checks.

Cause 1 of 2: the OIDC issuer URL an EKS cluster advertises is not served — a GET of <issuer>/.well-known/openid-configuration is answered by the emulator's generic service router with 404 UnknownService instead of the OpenID provider metadata. Failing check: eks.eks-oidc-discovery-and-jwks-verify-a-projected-token-offline.
AWS:      recorded behavior only (the document's contents are not modelled in the EKS Smithy model; DescribeCluster.cluster.identity.oidc.issuer is modelled only as a String). A cluster's identity.oidc.issuer is an HTTPS URL that actually serves OIDC provider metadata on the public path shape /id/<oidc-id>/.well-known/openid-configuration: 200 with a JSON document whose "issuer" equals the advertised issuer URL, whose "jwks_uri" is a URL under that same issuer, with "id_token_signing_alg_values_supported": ["RS256"] and "response_types_supported": ["id_token"]; the jwks_uri itself answers 200 anonymously (no Authorization header) with a JWKS containing an RSA/RS256/sig key carrying n and e, and the kid of a projected ServiceAccount token selects exactly one of those keys so the token's signature verifies offline. On AWS the issuer is https://oidc.eks.<region>.amazonaws.com/id/<oidc-id>; an emulator must publish an address it is itself reached on, keeping that path shape. The host, port and oidc-id are per-run values; the requirement is that the advertised issuer is fetchable and self-describing.
Emulator: the GET of the advertised discovery URL returns HTTP 404 with body {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"} — i.e. the request falls through to the generic request router, which has no route for the cluster's OIDC path and reports it as an unresolvable service rather than serving provider metadata. The check fails at its first discovery assertion, so the JWKS, the kid selection and the offline signature verification are never reached.
Why:      every IRSA flow a client runs follows the issuer: registering the cluster's OIDC provider with IAM (which fetches the discovery document and the JWKS), validating a projected ServiceAccount token, or any describe-cluster + OIDC bootstrap script. A client gets a 404 with a non-OIDC, AWS-error-shaped body where it expects provider metadata, so it cannot establish the cluster's trust anchor at all, while DescribeCluster reports a healthy cluster with an issuer URL.
Fix:      the per-cluster OIDC listener/route for EKS — the handler that must serve GET /id/<oidc-id>/.well-known/openid-configuration and the jwks_uri it advertises for every cluster whose identity.oidc.issuer the CreateCluster/DescribeCluster path publishes, plus the router entry that stops such anonymous path-routed GETs from being resolved as an unknown AWS service. Note: the open issue 20261121T200646Z-cloudformation-eks-nothing-serves-the-oidc-discovery-document.md describes the same unserved-issuer behavior for the cloudformation__eks check (it carries that check's replay key and there the route never answers at all); it does not cover this eks check, whose route now answers 404 UnknownService.
Traces:   eks.eks-oidc-discovery-and-jwks-verify-a-projected-token-offline (AssertionError at line 380: "the advertised OIDC issuer does not serve a discovery document").

Cause 2 of 2: CreateCluster's default Kubernetes service CIDR is a fixed 172.20.0.0/16 instead of being chosen so it does not overlap the cluster VPC's address space. Failing check: eks.ekscc-create-cluster-applies-documented-defaults.
AWS:      Smithy: KubernetesNetworkConfigRequest.serviceIpv4Cidr documents that when no block is specified "Kubernetes assigns addresses from either the 10.100.0.0/16 or 172.20.0.0/16 CIDR blocks" and that the block "Doesn't overlap with any CIDR block assigned to the VPC that you selected for VPC". So for a cluster created with only name, version, role and subnets, DescribeCluster's kubernetesNetworkConfig.serviceIpv4Cidr is 172.20.0.0/16 when the VPC CIDR overlaps 10.0.0.0/8, and 10.100.0.0/16 otherwise (e.g. the default VPC's 172.31.0.0/16).
Emulator: serviceIpv4Cidr is always 172.20.0.0/16 regardless of the subnets' VPC CIDR. With the default VPC at 172.31.0.0/16 (no overlap with 10.0.0.0/8) the emulator returned '172.20.0.0/16' where AWS returns '10.100.0.0/16'; the chosen default does not consider the VPC range at all.
Why:      a client that sizes or peers networks from the reported service CIDR gets a range in the same /12 as its VPC: CNI/kube-proxy configuration, route tables and peering checks derived from DescribeCluster are computed against a block AWS would have avoided, and any assertion that the service CIDR does not overlap the VPC fails.
Fix:      the CreateCluster handler's default-materialisation for kubernetesNetworkConfig (the code that fills serviceIpv4Cidr when the request omits it), which must select between 10.100.0.0/16 and 172.20.0.0/16 by overlap with the resolved VPC's CIDR before the value is persisted and echoed by DescribeCluster.
Traces:   eks.ekscc-create-cluster-applies-documented-defaults (AssertionError at line 44: "VPC 172.31.0.0/16 -> default service CIDR 10.100.0.0/16; got '172.20.0.0/16'").

## Evidence
Check eks.eks-oidc-discovery-and-jwks-verify-a-projected-token-offline [mismatch]:
  AssertionError: GET https://<emulator-host>:<port>/id/<OIDC-ID>/.well-known/openid-configuration returned 404, so the advertised OIDC issuer does not serve a discovery document: b'{"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}'
  Expected (AWS): 200 with provider metadata whose "issuer" equals the advertised issuer, "jwks_uri" under that issuer, id_token_signing_alg_values_supported == ["RS256"], response_types_supported == ["id_token"]; the jwks_uri then answers 200 anonymously with an RSA/RS256/sig key (n, e) whose kid matches the projected token's header kid.
  Actual: 404 with the generic router's UnknownService error body. Fixed text of that body is the router's own; the host, port and OIDC id are per-run values and are not part of the requirement.

Check eks.ekscc-create-cluster-applies-documented-defaults [mismatch]:
  AssertionError: VPC 172.31.0.0/16 -> default service CIDR 10.100.0.0/16; got '172.20.0.0/16'
  Expected (AWS): kubernetesNetworkConfig.serviceIpv4Cidr defaults to the one of 10.100.0.0/16 / 172.20.0.0/16 that does not overlap the cluster VPC's CIDR (10.100.0.0/16 for a 172.31.0.0/16 VPC).
  Actual: 172.20.0.0/16 unconditionally. The specific VPC CIDR shown is this run's default VPC; the requirement is the overlap rule, not that value.

## Steps to Reproduce
Cause 1:
1. Create an IAM role trusting eks.amazonaws.com with AmazonEKSClusterPolicy, then eks CreateCluster with default-VPC subnets; wait for ACTIVE.
2. Read cluster.identity.oidc.issuer from DescribeCluster.
3. GET <issuer>/.well-known/openid-configuration with no Authorization header: the emulator answers 404 {"__type":"UnknownService",...} instead of the OIDC provider metadata document (and therefore no jwks_uri / JWKS is reachable to verify a projected ServiceAccount token offline).

Cause 2:
1. eks CreateCluster with only name, version 1.33, the cluster service role and default-VPC subnets (VPC CIDR 172.31.0.0/16 — i.e. not overlapping 10.0.0.0/8), omitting kubernetesNetworkConfig.
2. DescribeCluster and read kubernetesNetworkConfig.serviceIpv4Cidr: the emulator reports 172.20.0.0/16; AWS reports 10.100.0.0/16 because 172.20.0.0/16 is in the same private /12 family as the VPC and the non-overlapping block is chosen.
