Issue-Source: trace-sweep-divergence
Issue-Trace: api-account-settings-and-authorization-token
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: api-account-settings-and-authorization-token
Issue-Shape-Id: com.amazonaws.ecr#AmazonEC2ContainerRegistry_V20150921
Issue-Key: ecr|GetAuthorizationToken|2xx->2xx|$.authorizationData[*]
Issue-Commit: 17b8d6926fcb04a314774f8dcf7dba28eb7ebe93

# ecr GetAuthorizationToken announces a proxyEndpoint that is not an ECR registry URL

> _Note: this issue was identified against the code base at commit `17b8d6926fcb04a314774f8dcf7dba28eb7ebe93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAuthorizationToken

## Description
Cause 1 of 1: the authorizationData element GetAuthorizationToken returns does not conform to the shape AWS returns, because its proxyEndpoint is a bare loopback host:port instead of an ECR registry URL.

AWS: GetAuthorizationToken returns authorizationData[] of AuthorizationData{authorizationToken, expiresAt, proxyEndpoint} (Smithy: ECR GetAuthorizationToken -> GetAuthorizationTokenResponse.authorizationData : AuthorizationDataList; errors InvalidParameterException 400, ServerException 500). The model documents AuthorizationData$proxyEndpoint as "The registry URL to use for this authorization token in a `docker login` command. The Amazon ECR registry URL format is https://aws_account_id.dkr.ecr.region.amazonaws.com" — i.e. the value is always `https://` + the REGISTRY AUTHORITY for the caller's registry id and region, with no port, and it is the SAME authority that the service reports in Repository$repositoryUri (`<registryId>.dkr.ecr.<region>.amazonaws.com/<repositoryName>`). The registry id and the region in it are values taken from the caller's account and the serving region; the `<id>.dkr.ecr.<region>` authority shape is the fixed part of the template. Status 200 on success.

Emulator: the step is answered 200, but the replayed authorizationData[0] does not match the recorded element shape ($.authorizationData[0] reported both missing and extra — the actual element matches no expected element). The member that deviates is proxyEndpoint: the emulator publishes `https://127.0.0.1:32825`, a literal loopback IP with a port and no registry authority at all, where AWS publishes `https://<registryId>.dkr.ecr.<region>.amazonaws.com`. authorizationToken (base64 of `AWS:<password>`) and expiresAt (epoch seconds, ~12 h ahead) are shaped as recorded; the published endpoint is not. The emulator is also internally inconsistent here: the very same replay has it reporting repositoryUri as `000000000000.dkr.ecr.us-west-2.amazonaws.com/<repo>` (the DescribeRepositories / CreateRepository clusters in this run), so the registry the minted credential names is not the registry the repository records name.

Why: a client that does what AWS documents is broken two ways. `docker login`-style flows take proxyEndpoint verbatim as the registry to authenticate against, then push/pull `<repositoryUri>:<tag>`; because the authority in proxyEndpoint (`127.0.0.1:32825`) is not the authority in repositoryUri (`<id>.dkr.ecr.<region>.amazonaws.com`), the credential is scoped to a registry name no image reference ever uses. Clients that parse proxyEndpoint to recover the registry id or the region (a standard way to derive the registry account from a token, and to assemble image references) get no registry id and no region out of a bare IP and fail or mis-address the registry.

Fix: the GetAuthorizationToken handler and the registry-endpoint composer it calls — the announced-endpoint helper in the ECR service crate's state module that get_authorization_token.rs uses to build the login URL. It must compose proxyEndpoint as `https://<registryId>.dkr.ecr.<region>.amazonaws.com` for the registry the token is issued for (the same authority already used for Repository$repositoryUri), rather than echoing the emulator's listener address, and the registry v2 data plane must answer for that authority.

Related open issues that do NOT cover this step: 20261118T005003Z-ecr-getauthorizationtoken-publishes-a-proxyendpoint-on-a-host.md is a BEHAVIORAL finding about the same member when it carried an `<account>.dkr.ecr.<region>.localhost:<port>` host (a different value, and it asks for an emulator-served address rather than the AWS-shaped authority this wire recording requires); 20261116T084727Z-ecr-announces-a-repositoryuri-host-the-emulator-does.md owns the repositoryUri clusters of this same replay, a different operation and member. Neither carries this fixture step's replay key.

Traces: api-account-settings-and-authorization-token#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture api-account-settings-and-authorization-token, step task#6 (service ecr, operation GetAuthorizationToken), verdict mismatch, expected_status 200 / actual_status 200, affected_count 1 (api-account-settings-and-authorization-token#task#6).

Divergence key: service=ecr|GetAuthorizationToken|status=200->200|keys=$.authorizationData[0]

Detail:
  $.authorizationData[0]: missing (expected {'authorizationToken': '<shaped-not-asserted>', 'expiresAt': '<shaped-not-asserted>', 'proxyEndpoint': '<shaped-not-asserted>'}, got {'authorizationToken': 'QVdTOkpGMWVIeWdiK3NsVFpIQ2ZWbzB6Uk5aOTN4dC9RQmhPc3Rw...', 'expiresAt': 1795293217.869, 'proxyEndpoint': 'https://127.0.0.1:32825'})
  $.authorizationData[0]: extra (same pair, reported in the other direction)

Expected: 200 with authorizationData[0].proxyEndpoint shaped as AWS's registry URL, `https://<registryId>.dkr.ecr.<region>.amazonaws.com` (fixed text: the scheme, the `.dkr.ecr.` and `.amazonaws.com` labels, no port; variable parts: the registry id from the caller's account and the serving region), matching the registry authority the same emulator reports in Repository$repositoryUri.

Actual: 200 with authorizationData[0].proxyEndpoint = `https://127.0.0.1:32825` — the emulator's own listener address, carrying neither a registry id nor a region, and a different authority from the `000000000000.dkr.ecr.us-west-2.amazonaws.com/...` repositoryUri values returned elsewhere in the same replay. (The specific IP and port are per-environment values and are not the requirement; the absence of a registry authority is.)

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay fixture api-account-settings-and-authorization-token, step task#6 (ECR GetAuthorizationToken) from trace set simparity-wire-aws-v1.
3. Or by hand: call ECR GetAuthorizationToken with no registryIds and read authorizationData[0].proxyEndpoint. Observe it is `https://<emulator-host>:<port>` rather than `https://<registryId>.dkr.ecr.<region>.amazonaws.com`.
4. Call CreateRepository / DescribeRepositories in the same account and compare repository.repositoryUri: its authority is `<registryId>.dkr.ecr.<region>.amazonaws.com`, which does not match the authority the token was issued for.
