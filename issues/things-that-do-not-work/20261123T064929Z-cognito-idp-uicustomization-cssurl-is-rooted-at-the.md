Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-ui-customization-crud
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-ui-customization-crud cognito-idp-ui-customization-global-and-client cognito-idp-ui-customization-lifecycle cognito-idp-ui-customization-round-trip
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|SetUICustomization|2xx->2xx|$.UICustomization.CSSUrl
Issue-Key: cognito-idp|GetUICustomization|2xx->2xx|$.UICustomization.CSSUrl
Issue-Commit: d320d15df1e8a4ddf926cc8f8c86b65a79910efa

# cognito-idp UICustomization CSSUrl is rooted at the emulator listener instead of the pool's hosted-UI asset domain

> _Note: this issue was identified against the code base at commit `d320d15df1e8a4ddf926cc8f8c86b65a79910efa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetUICustomization

## Description
Both hosted-UI customization paths return a UICustomization.CSSUrl that is not a hosted-UI asset URL at all: the emulator builds it from its own data-listener socket rather than from the user pool domain's CloudFront distribution. 2 causes, one per read/write path.

Cause 1 of 2 - SetUICustomization response (fixture cognito-idp-ui-customization-crud, step task#4).
AWS: recorded behavior only - the on-disk Smithy model's UICustomizationType (members UserPoolId, ClientId, ImageUrl, CSS, CSSVersion, LastModifiedDate, CreationDate) does not model CSSUrl, so the requirement comes from the recording. SetUICustomization returns 200 with UICustomization.CSSUrl formed as https://<the pool domain's CloudFront distribution domain>/<UserPoolId>/<ClientId, or the literal ALL for the pool-wide customization>/<CSSVersion>/assets/CSS/custom-css.css. The fixed text of that template is the scheme, the path segments 'assets/CSS/custom-css.css' and the literal 'ALL' for a client-less customization; the variable parts are the distribution domain (the same value DescribeUserPoolDomain reports as DomainDescription.CloudFrontDistribution for that pool), the UserPoolId and ClientId from the request, and the CSSVersion the service assigned. The recording's concrete host and version digits are one run's values, not requirements.
Emulator: returns 200, and the path segments for pool id, client id and 'assets/CSS/custom-css.css' are correct, but the authority is the emulator's own data listener (an address:port of the running process) instead of a hosted-UI asset domain derived from the pool's domain record. Replay reports '$.UICustomization.CSSUrl: resource_not_identified' - the expected CloudFront-distribution resource is identified nowhere in the returned URL.
Why: a client that stores or follows CSSUrl to serve/verify the pool's hosted-UI stylesheet is handed a URL that names no Cognito asset resource and is scoped to one emulator process's socket, so anything that correlates CSSUrl with the pool's hosted-UI domain (console-style diagnosis flows, infrastructure that records the asset distribution) mismatches or breaks, and the URL cannot be resolved by any consumer outside that process.
Fix: the SetUICustomization handler's response construction in the cognito-idp service crate - the CSSUrl/ImageUrl builder must take its authority from the user pool domain's CloudFront-distribution record held in service state (the same source DescribeUserPoolDomain must serve) rather than from the inbound request's Host header or the listener's bind address. Note the related open issues 20261102T111936Z-cognito-idp-response-payloads-diverge-read-paths-omit.md and 20261122T211230Z-cognito-idp-hosted-ui-read-paths-serve-incomplete.md, which own DescribeUserPoolDomain omitting CloudFrontDistribution entirely; that missing state is the likely upstream of this URL, but neither issue covers CSSUrl and neither carries these steps' replay keys.
Traces: cognito-idp-ui-customization-crud#task#4 (representative), plus the same cluster at cognito-idp-ui-customization-global-and-client#task#4, cognito-idp-ui-customization-global-and-client#task#6, cognito-idp-ui-customization-lifecycle#task#5, cognito-idp-ui-customization-lifecycle#task#7, cognito-idp-ui-customization-round-trip#task#5.

Cause 2 of 2 - GetUICustomization read-back (fixture cognito-idp-ui-customization-lifecycle, step task#6).
AWS: recorded behavior only, same template as Cause 1: GetUICustomization returns 200 with the stored customization and a CSSUrl under the pool's hosted-UI asset distribution, with 'ALL' in the client-id position when the customization is the pool-wide one.
Emulator: returns 200 with the stored CSS, but again reports CSSUrl under its own listener address instead of the pool's asset distribution ('$.UICustomization.CSSUrl: resource_not_identified'). The read path therefore reproduces the write path's defect rather than echoing a stored, distribution-rooted URL.
Why: the defect is not confined to the create response - a client that reads the customization later, or that compares the Set response with a subsequent Get, never observes a valid asset URL, so the hosted-UI asset location is unavailable through either path.
Fix: the GetUICustomization handler in the cognito-idp service crate must serve the CSSUrl persisted/derived from the pool's domain record (shared with the Set path above), not recompute it from the serving socket.
Traces: cognito-idp-ui-customization-lifecycle#task#6, cognito-idp-ui-customization-lifecycle#task#8.

## Evidence
Replay of trace set simparity-wire-aws-v1 (200 fixtures, 1815 steps, 97.4% pass) reports two unowned mismatch clusters, both keyed on $.UICustomization.CSSUrl:

Cluster 1 - service=cognito-idp|SetUICustomization|status=200->200|keys=$.UICustomization.CSSUrl, fixture cognito-idp-ui-customization-crud step task#4, 6 affected steps:
  expected 'https://<cloudfront-distribution-domain>/us-west-2_dedae0c09/4786bc53e42f4f81b3ef0713f5/{{css_version}}/assets/CSS/custom-css.css'
  actual   'https://127.0.0.1:42721/us-west-2_dedae0c09/4786bc53e42f4f81b3ef0713f5/20261123064710/assets/CSS/custom-css.css'
  verdict: resource_not_identified (status matched at 200; the version segment is templated/per-run and is NOT the finding)

Cluster 2 - service=cognito-idp|GetUICustomization|status=200->200|keys=$.UICustomization.CSSUrl, fixture cognito-idp-ui-customization-lifecycle step task#6, 2 affected steps:
  expected 'https://<cloudfront-distribution-domain>/us-west-2_089732543/ALL/<css-version>/assets/CSS/custom-css.css'
  actual   'https://127.0.0.1:45881/us-west-2_089732543/ALL/20261123064651/assets/CSS/custom-css.css'
  verdict: resource_not_identified

In both, the pool-id, client-id ('ALL' for the pool-wide customization) and 'assets/CSS/custom-css.css' segments match; only the authority diverges, and it diverges in kind - a process socket where a hosted-UI asset distribution domain belongs. Corroboration that the underlying state is absent: the same replay shows DescribeUserPoolDomain returning DomainDescription with CloudFrontDistribution missing (fixture cognito-idp-osc-dashboards-client-callback-diagnosis step task#7), already owned by open issues.

Smithy (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice): SetUICustomization/GetUICustomization both output a required UICustomizationType; UICustomizationType models UserPoolId, ClientId, ImageUrl, CSS, CSSVersion, LastModifiedDate, CreationDate - no CSSUrl member, hence 'recorded behavior only' for this field's content.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture cognito-idp-ui-customization-crud step task#4 - SetUICustomization for a user pool with a hosted-UI domain, passing UserPoolId, ClientId and a CSS body. Inspect UICustomization.CSSUrl in the 200 response: its authority is the emulator's data-listener address:port, not the pool domain's CloudFront distribution domain. The same cluster reproduces at cognito-idp-ui-customization-global-and-client#task#4 and #task#6, cognito-idp-ui-customization-lifecycle#task#5 and #task#7, and cognito-idp-ui-customization-round-trip#task#5.
3. Cause 2: replay fixture cognito-idp-ui-customization-lifecycle step task#6 - GetUICustomization for the pool-wide ('ALL') customization set earlier in that fixture. The returned UICustomization.CSSUrl again carries the emulator's listener authority instead of the pool's asset distribution domain. Reproduces again at cognito-idp-ui-customization-lifecycle#task#8.
4. For both, cross-check DescribeUserPoolDomain for the same pool: no CloudFrontDistribution is returned, so no distribution domain exists in state for the URL builder to use.
5. Ignore the version path segment and the specific port when confirming: those are per-run values. The fix is confirmed when the authority is the pool domain's CloudFront-distribution domain from service state, consistent between the Set response and the Get read-back.
