Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__eks.cfn-eks-private-cluster-in-place-update-serves-oidc-discovery-then-not-found
Issue-Key: cloudformation__eks.cfn-eks-private-cluster-in-place-update-serves-oidc-discovery-then-not-found|_assert_discovery#25|TimeoutError|raise TimeoutError(f"run budget ended before {url} served")
Issue-Commit: 7feef7c3a108bb63e3851ac69c48ec064beaa56b

# cloudformation__eks: nothing serves the OIDC discovery document at the issuer URL EKS now publishes, so <issuer>/.well-known/openid-configuration never answers

> _Note: this issue was identified against the code base at commit `7feef7c3a108bb63e3851ac69c48ec064beaa56b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster

## Description
Cause 1 of 1: an EKS cluster's Identity.Oidc.Issuer is now published on one of the emulator's own hosts, but no listener serves the OpenID discovery document under it — a GET of <issuer>/.well-known/openid-configuration never returns, so the check's poll exhausts the run budget. Failing check: cloudformation__eks.cfn-eks-private-cluster-in-place-update-serves-oidc-discovery-then-not-found.

AWS:      recorded behavior only (the document's contents are not in the EKS model). An EKS cluster's Identity.Oidc.Issuer is an HTTPS URL that actually serves OIDC provider metadata: GET <issuer>/.well-known/openid-configuration answers 200 with a JSON document whose issuer member equals the issuer URL itself and which carries the provider metadata fields (jwks_uri under the same issuer, response_types_supported, subject_types_supported, id_token_signing_alg_values_supported, claims). On AWS the issuer is https://oidc.eks.<region>.amazonaws.com/id/<cluster-oidc-id>; everything an emulator publishes must instead be an address it is reached on, with the same path shape /id/<oidc-id>/.well-known/openid-configuration. The requirement is that the published issuer is fetchable and self-describing, not any particular host.
Emulator: DescribeCluster/the stack's Issuer output now names the emulator's own host — 'https://<emulator-host>:<port>/id/<OIDC-ID>' — but the discovery path under it is never served: the check polls https://<emulator-host>:<port>/id/<OIDC-ID>/.well-known/openid-configuration until the run budget ends and raises TimeoutError 'run budget ended before <url> served'. No response (not even a 404 with an AWS-shaped body) is produced on that route, so the cluster's own OIDC identity is unusable and the check cannot reach its later assertions (the in-place tag update, and the post-delete refusal ResourceNotFoundException / 'No cluster found for name: <cluster-name>.').
Why:      every IRSA-style flow a client runs against a cluster follows the issuer: registering the cluster's OIDC provider with IAM (which fetches the discovery document and the JWKS), validating a service-account token, or any `aws eks describe-cluster` + OIDC bootstrap script. With the route unserved the client hangs on the fetch instead of getting provider metadata, so nothing downstream of the issuer can be exercised; the cluster looks healthy (CREATE_COMPLETE, an issuer URL present) while its OIDC identity does not exist.
Fix:      the EKS cluster's per-resource OIDC listener — the handler that must serve GET /id/<oidc-id>/.well-known/openid-configuration (and the jwks_uri it advertises) for every cluster whose Identity.Oidc.Issuer it publishes, alongside the CreateCluster/DescribeCluster issuer construction. The open issue 20261111T111216Z-cloudformation-eks-eks-publishes-an-oidc-issuer-url-on-a.md covers the previous cause on this same check (the issuer naming oidc.eks.<region>.amazonaws.com, a host the emulator does not serve) and carries that replay key (PublishedUrlRejected); the host is now the emulator's own, and this is the distinct, remaining cause: the announced path is unserved.
Traces:   cloudformation__eks.cfn-eks-private-cluster-in-place-update-serves-oidc-discovery-then-not-found (TimeoutError in _assert_discovery at line 250, reached from run() line 149).

## Evidence
Check cloudformation__eks.cfn-eks-private-cluster-in-place-update-serves-oidc-discovery-then-not-found [error]:
  TimeoutError: run budget ended before https://<emulator-host>:<port>/id/<OIDC-ID>/.well-known/openid-configuration served
Expected (AWS): the GET answers 200 with the provider metadata JSON whose "issuer" equals the published issuer URL and whose "jwks_uri" is under the same issuer; the fetch succeeds promptly after the cluster reports CREATE_COMPLETE.
Actual: the published issuer host is the emulator's own (so this is not a published-host refusal), but the discovery route under it never answers at all, within the whole run budget.
The host, port and OIDC id above are per-run values; the requirement is that the path the published issuer implies is served.

## Steps to Reproduce
1. CreateStack a template with an AWS::IAM::Role for EKS and a private-endpoint-only AWS::EKS::Cluster; wait for CREATE_COMPLETE.
2. Read the cluster's Identity.Oidc.Issuer (via eks DescribeCluster or the stack Output).
3. GET <issuer>/.well-known/openid-configuration: the host is the emulator's own host and port, but nothing answers on that path — the poll runs until the budget ends instead of returning the OIDC discovery document.
