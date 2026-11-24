Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.get-open-id-connect-provider-contract iam.op-contract-create-openid-connect-provider
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.get-open-id-connect-provider-contract|run#9|AssertionError|assert resp["Url"] == expected_url, f"expected scheme-stripped Url {expected_url!r}, got {resp['Url']!r}"
Issue-Key: iam.op-contract-create-openid-connect-provider|run#7|AssertionError|assert got["Url"] == oidc_url.removeprefix("https://"), (
Issue-Commit: 4e68013acb17007a4b50a9cc9c0932294de10e0a

# iam: GetOpenIDConnectProvider reports the emulator's own serving authority as the provider Url instead of the caller-supplied, scheme-stripped issuer URL (1 cause)

> _Note: this issue was identified against the code base at commit `4e68013acb17007a4b50a9cc9c0932294de10e0a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetOpenIDConnectProvider

## Description
Cause 1 of 1: the Url an OIDC provider is registered with is replaced, on read, by the address the emulator is reached on, so the provider no longer identifies the issuer the caller registered.

AWS: Smithy (services/service-com-amazonaws-iam-awsidentitymanagementv20100508) models CreateOpenIDConnectProvider.Url as required, target OpenIDConnectProviderUrlType @length 1..255, and GetOpenIDConnectProvider.Url as the same shape; the member documentation says "The URL that the IAM OIDC provider resource object is associated with" and points at CreateOpenIDConnectProvider, whose documentation names the input as "The URL of the OIDC identity provider (IdP) to trust". AWS stores exactly the URL the request carried, with only the scheme removed, and that scheme-stripped URL is what GetOpenIDConnectProvider returns and what the provider's ARN encodes (arn:<partition>:iam::<account>:oidc-provider/<host>[:<port>]/<path>). The template for the returned value is therefore: the request's Url with a leading "https://" removed, verbatim — host, optional port and path all taken from the request; nothing about the serving endpoint appears in it. The other graded facts in the same response (ClientIDList echoing the request, at least one 40-hex SHA-1 thumbprint IAM fetched when ThumbprintList was omitted, CreateDate, and the EntityAlreadyExists refusal "Provider with url <url> already exists." for a duplicate) are AWS's as well; only the Url diverges here.

Emulator: GetOpenIDConnectProvider answers 200 with a Url built from the emulator's own listening authority plus the issuer's path. For an issuer registered as https://cognito-idp.<region>.amazonaws.com/<user-pool-id> the response carried '<emulator-host>:<emulator-port>/<user-pool-id>' where AWS answers 'cognito-idp.<region>.amazonaws.com/<user-pool-id>'. The rewrite is deliberate in the handler: CreateOpenIDConnectProvider stores a `served_endpoint` field computed by `crate::state::published_endpoint_authority(ctx.parts(), ctx.endpoint())` alongside the url, and the read path publishes the issuer path under it (`crate::state::published_open_id_connect_url`). Note this is NOT merely a per-run host/port value differing: the host component of the requested issuer is discarded entirely and substituted, so no value of host/port makes the response equal the request's URL.

Why: the provider Url is the provider's identity. A client that registers an issuer and reads it back to confirm (or to compute the ARN, or to render the trust relationship) sees a different issuer than it asked for, so it concludes the provider is wrong and re-creates it — which then collides with EntityAlreadyExists, because the ARN is keyed off the real URL. Trust policies written against `arn:...:oidc-provider/<issuer-host>/<path>` and condition keys keyed on the issuer no longer line up with what the account reports, and tooling that diffs desired-vs-actual state (CDK, eksctl, Terraform) never converges.

Fix: the GetOpenIDConnectProvider read path and the provider record it reads, in the IAM service crate — the stored provider must report the caller-supplied Url with only the scheme stripped. If a locally reachable address is needed to serve the discovery document, it belongs in a side channel, not in the modeled `Url` member. The same record is read by ListOpenIDConnectProviders/CreateOpenIDConnectProvider's ARN derivation, so the stored URL and the published URL must be the one value.

Traces: iam.get-open-id-connect-provider-contract (asserts the scheme-stripped Url), iam.op-contract-create-openid-connect-provider (asserts the same Url after a create that omits ThumbprintList). Code citations: services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/ops/create_open_id_connect_provider.rs lines 408-425 (stores `served_endpoint` from `published_endpoint_authority`).

## Evidence
Expected (AWS): GetOpenIDConnectProvider(OpenIDConnectProviderArn=<arn>) returns Url equal to the registered issuer with the scheme stripped — for Url="https://cognito-idp.<region>.amazonaws.com/<pool-id>", Url == "cognito-idp.<region>.amazonaws.com/<pool-id>".

Actual (emulator), iam.get-open-id-connect-provider-contract:
  AssertionError: expected scheme-stripped Url 'cognito-idp.us-west-2.amazonaws.com/us-west-2_37a42f8af', got '127.0.0.1:33335/us-west-2_37a42f8af'
(check_iam_get_open_id_connect_provider_contract.py line 32)

Actual (emulator), iam.op-contract-create-openid-connect-provider:
  AssertionError: expected Url 'https://cognito-idp.us-west-2.amazonaws.com/us-west-2_02c57f49d' (scheme stripped), got '127.0.0.1:34177/us-west-2_02c57f49d'
(check_iam_op_contract_create_openid_connect_provider.py line 27)

The host:port in the two actuals is the per-run serving address and is not itself the requirement; the defect is that the issuer host from the request is replaced at all. Both checks fail on the first Url assertion, so the thumbprint, CreateDate, duplicate-EntityAlreadyExists and NoSuchEntity assertions in the same checks were not reached and are not claimed here either way.

## Steps to Reproduce
1. Create an OIDC issuer URL the target serves, e.g. cognito-idp:CreateUserPool then build url = "https://cognito-idp.<region>.amazonaws.com/<UserPool.Id>".
2. iam:CreateOpenIDConnectProvider(Url=url, ClientIDList=["sts.amazonaws.com"]) — note the returned OpenIDConnectProviderArn.
3. iam:GetOpenIDConnectProvider(OpenIDConnectProviderArn=<arn>).
4. Compare response Url with url.removeprefix("https://"): the emulator returns "<emulator-host>:<emulator-port>/<pool-id>" instead of "cognito-idp.<region>.amazonaws.com/<pool-id>".
5. Repeat step 2 omitting ThumbprintList — the same rewritten Url is reported by GetOpenIDConnectProvider.
