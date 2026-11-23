Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: grafana.grafana-http-api-native-round-trip grafana.workspace-api-key-lifecycle-round-trip
Issue-Shape-Id: com.amazonaws.grafana#AWSGrafanaControlPlane
Issue-Key: grafana.grafana-http-api-native-round-trip|run#9|PublishedUrlRejected|org = _poll_grafana_api(f"{ctx.endpoint_url(endpoint)}/api/org", key, timeout_s=180.0)
Issue-Key: grafana.workspace-api-key-lifecycle-round-trip|run#11|PublishedUrlRejected|org = _poll_grafana_api(f"{ctx.endpoint_url(endpoint)}/api/org", secret, timeout_s=180.0)
Issue-Commit: 6e06957da8d63bf47f50e8aab29b9e886ef2aec8

# Grafana workspace endpoint is published as an unreachable amazonaws.com host, so no native Grafana HTTP API flow can run

> _Note: this issue was identified against the code base at commit `6e06957da8d63bf47f50e8aab29b9e886ef2aec8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeWorkspace

## Description
There is 1 cause in this issue.

Cause 1 of 1: the workspace endpoint the emulator publishes names a host the emulator is not reached on, so every data-plane flow that follows the published endpoint leaves the emulator. Failing checks: grafana.grafana-http-api-native-round-trip, grafana.workspace-api-key-lifecycle-round-trip.

AWS: Smithy (com.amazonaws.grafana#AWSGrafanaControlPlane): DescribeWorkspace @http(GET /workspaces/{workspaceId}, 200), @readonly; output member workspace: WorkspaceDescription (required); WorkspaceDescription$endpoint is documented as "The URL that users can use to access the Grafana console in the workspace". The modeled requirement is therefore that `endpoint` carries the ADDRESS at which that workspace's Grafana console and native Grafana HTTP API actually answer — a client splices it into requests of its own (https://<endpoint>/api/org) or uses it where only a host name will do (DNS, SNI, Host header). CreateWorkspace stores and reports the same member. On real AWS that address happens to take the form <workspaceId>.grafana-workspace.<region>.amazonaws.com; that literal host is the value the address takes in a real account, not part of the contract — what the model requires is that the value be the workspace's serving address.

Emulator: DescribeWorkspace returns 200 with workspace.endpoint set to a fixed public AWS name composed from the workspace id and region (observed: g-0000000001.grafana-workspace.us-west-2.amazonaws.com), with no scheme and no port, which is not an address this emulator answers on. The emulator is reached on its own data listener (the two checks reached it on 127.0.0.1:45629 and 127.0.0.1:35543 respectively). When a check takes the published endpoint and tries to address the workspace's own Grafana API at it, the attempt is refused before any request is made: `simparity.published.PublishedUrlRejected: endpoint_url was given https://g-0000000001.grafana-workspace.us-west-2.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts`. The control-plane calls preceding this (CreateWorkspace, DescribeWorkspace to ACTIVE, CreateWorkspaceServiceAccount/Token, CreateWorkspaceApiKey) all succeed, so the only divergence exercised is the announced host.

Why: a client that does what the member's documentation tells it to do — read `endpoint` and talk to the workspace's Grafana HTTP API at it — is sent off the emulator entirely: it either resolves a real AWS name (reaching production or nothing) or fails name resolution, and never reaches the emulator's own workspace API. Every workspace-console / native-Grafana-API flow is therefore unusable while the control-plane response looks successful, and the failure surfaces far from the call that caused it. It also hides the rest of both checks: whether GET /api/org returns an org payload with id and name for a service-account token and for a CreateWorkspaceApiKey secret, whether a deleted API key stops authenticating there (401/403), and whether a second DeleteWorkspaceApiKey raises ResourceNotFoundException with message "API key <keyName> not found." (fixed text "API key " + the keyName from the request + " not found.") cannot be exercised at all until the endpoint is reachable.

Fix: the state projection that composes the announced endpoint — the workspace-endpoint builder in the grafana crate's state module and the CreateWorkspace path that stores Workspace.endpoint from it (echoed by to_description/to_summary and returned by describe_workspace) — must publish the address the emulator is actually reached on, i.e. the authority of the inbound request including its port, so the host the control-plane call arrived at is the host the workspace API is offered at. The emulator's native workspace API handler must then answer that host for /api/org and for bearer-token auth with service-account tokens and workspace API keys.

Note: open issue 20261123T024042Z-grafana-workspace-endpoint-announces-an-unreachable-amazonaws-com.md ("Grafana workspace endpoint announces an unreachable amazonaws.com host") describes the same published-host defect from the WIRE corpus (key grafana|DescribeWorkspace|2xx->2xx|$.workspace.endpoint); it does not carry these behavioral check ids, so this issue records them as the behavioral evidence to re-replay.

Traces: grafana.grafana-http-api-native-round-trip, grafana.workspace-api-key-lifecycle-round-trip.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 17/19 passed; both failures are this cause.

[mismatch] grafana.grafana-http-api-native-round-trip — after CreateWorkspace -> ACTIVE, CreateWorkspaceServiceAccount and CreateWorkspaceServiceAccountToken all succeed, the check resolves ctx.endpoint_url(workspace['endpoint']) to call GET <endpoint>/api/org and is refused: "simparity.published.PublishedUrlRejected: endpoint_url was given https://g-0000000001.grafana-workspace.us-west-2.amazonaws.com (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:45629/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener".

[mismatch] grafana.workspace-api-key-lifecycle-round-trip — identical refusal, reached on the emulator's listener 127.0.0.1:35543: "endpoint_url was given https://g-0000000001.grafana-workspace.us-west-2.amazonaws.com ... its host is not one of the emulator's own hosts".

Expected: DescribeWorkspace/CreateWorkspace publish workspace.endpoint as a host the emulator serves (the authority the request arrived on, with its port), so https://<endpoint>/api/org reaches the emulated workspace API and returns 200 with a JSON org payload containing id and name.
Actual: workspace.endpoint is the fixed public host <workspaceId>.grafana-workspace.<region>.amazonaws.com (no port), which the emulator does not answer on; the data-plane leg is unreachable. The ports and the generated workspace id above are per-run values, not requirements — the requirement is that the announced host be one of the emulator's own.

## Steps to Reproduce
1. Boot the emulator from this checkout and run the behavioral suite (simparity-behavioral-aws-v1), or run the two checks grafana-http-api-native-round-trip and workspace-api-key-lifecycle-round-trip directly.
2. Create an IAM role trusted by grafana.amazonaws.com, then grafana.create_workspace(accountAccessType='CURRENT_ACCOUNT', permissionType='SERVICE_MANAGED', authenticationProviders=['SAML'], workspaceRoleArn=<role arn>, workspaceName=<name>).
3. Poll grafana.describe_workspace(workspaceId=<id>) until status == 'ACTIVE' and read workspace['endpoint'].
4. Observe the endpoint is <workspaceId>.grafana-workspace.<region>.amazonaws.com — not the host:port the describe_workspace call was sent to.
5. Issue GET https://<endpoint>/api/org with Authorization: Bearer <service-account token or CreateWorkspaceApiKey secret>. The request does not reach the emulator; the harness rejects the published URL with PublishedUrlRejected (host is not one of the emulator's own hosts).
6. With a reachable endpoint, the remaining assertions would then be exercised: /api/org returns 200 with id and name; after delete_workspace_api_key the same secret is refused there with 401/403; a second delete_workspace_api_key raises ResourceNotFoundException with message "API key <keyName> not found."
