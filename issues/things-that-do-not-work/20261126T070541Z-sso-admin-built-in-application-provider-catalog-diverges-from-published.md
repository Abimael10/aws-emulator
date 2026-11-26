Issue-Source: trace-sweep-divergence
Issue-Trace: sso-admin-application-provider-describe-round-trip
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sso-admin-application-provider-describe-round-trip sso-admin-application-provider-not-found-describe-rejected sso-admin-catalog-list-application-providers
Issue-Shape-Id: com.amazonaws.ssoadmin#SWBExternalService
Issue-Key: sso-admin|DescribeApplicationProvider|2xx->2xx|$.ApplicationProvider.DisplayData.IconUrl
Issue-Key: sso-admin|DescribeApplicationProvider|2xx->2xx|$.DisplayData.IconUrl
Issue-Key: sso-admin|ListApplicationProviders|2xx->2xx|$.ApplicationProviders[*]
Issue-Key: sso-admin|ListApplicationProviders|2xx->2xx|$.ApplicationProviders[*].DisplayData.IconUrl
Issue-Commit: 49810198788545ec3e03734d27935af3b9a5c66f

# Built-in application provider catalog diverges from published Identity Center catalog

> _Note: this issue was identified against the code base at commit `49810198788545ec3e03734d27935af3b9a5c66f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeApplicationProvider

## Description
The emulator's built-in application-provider catalog (crate::state::BUILT_IN_APPLICATION_PROVIDERS, projected through to_shape by both DescribeApplicationProvider and ListApplicationProviders) is not the catalog IAM Identity Center publishes: the icon URLs it reports are emulator-local inventions, and the catalog's membership, ordering and scope text do not match the service's published data. Two causes, both read-only paths that answer 200 with the wrong body.

Cause 1 of 2: DescribeApplicationProvider reports DisplayData.IconUrl as an emulator-local URL instead of the catalog's published icon URL.
AWS: Smithy: DescribeApplicationProvider output DisplayData -> DisplayData.IconUrl (target IconUrl, @length 1..768, @pattern ^(http|https)://.*$), documented as "A URL that points to an icon that represents the application provider." The model constrains only the URL syntax, so for the URL's form this is recorded behavior only: Identity Center serves every catalog provider's icon from its own static asset host, as https://static.global.sso.amazonaws.com/app-<providerAssetId>/icons/default.png, where the fixed text is the static.global.sso.amazonaws.com host and the /icons/default.png suffix and the variable part is the per-provider asset id segment carried by the catalog entry (the recording shows app-d77638588eff9057 for the described provider; that literal value is the recording's data, not the requirement). The same value is reported both at $.DisplayData.IconUrl and, where the provider is nested, at $.ApplicationProvider.DisplayData.IconUrl.
Emulator: returns 200 with an IconUrl built from the emulator's own data listener and a synthesized path, of the form https://<emulator-host>:<port>/_aws/sso-application-provider-icons/<arn-suffix>/icon.png (the replay recorded https://127.0.0.1:40169/_aws/sso-application-provider-icons/catalog/Statuspage/icon.png). The divergence is the URL's host and path scheme, not its per-run host:port: the emulator announces a loopback address and a private /_aws/... route that exists nowhere in the service's data model, and it does so for every provider.
Why: a client that renders the portal icon, caches it, or stores the URL alongside the provider gets an address that resolves to the emulator's own loopback interface and a path no Identity Center asset host serves, so icon fetches fail or point at the wrong origin; code asserting the published static-asset form on this field cannot pass against the emulator even though the provider itself resolved.
Fix: the application-provider catalog seed in services/service-com-amazonaws-ssoadmin-swbexternalservice/src/state.rs (BUILT_IN_APPLICATION_PROVIDERS) and the shared to_shape projection it feeds, so DisplayData.IconUrl carries the catalog entry's published asset URL rather than a URL derived from the running listener's address.
Traces: sso-admin-application-provider-describe-round-trip#task#2 (representative); the same cluster also fails sso-admin-application-provider-not-found-describe-rejected#task#2.

Cause 2 of 2: ListApplicationProviders returns a different, much smaller catalog than the service publishes, with no IconUrl and with different resource-server scope text.
AWS: Smithy: ListApplicationProviders input MaxResults (@range 1..100) / NextToken (@length 0..2048, @pattern ^[-a-zA-Z0-9+=/_]*$), output ApplicationProviders (ApplicationProviderList) + NextToken, @paginated(items: ApplicationProviders). Each ApplicationProvider carries ApplicationProviderArn (@pattern ^arn:aws(-[a-z]{1,5}){0,3}:sso::aws:applicationProvider/[a-zA-Z0-9-/]+$), FederationProtocol, DisplayData and ResourceServerConfig. The catalog's contents are recorded behavior only: the recording shows the published catalog containing both the AWS-service providers and the third-party SAML catalog entries whose ARN suffix is catalog/<ProviderName> (e.g. catalog/AdobeConnect, catalog/CMNTY with FederationProtocol SAML), every entry carrying DisplayData.DisplayName, DisplayData.Description and DisplayData.IconUrl, in the catalog's own published order, and extending to index 393 of ApplicationProviders in the recorded page.
Emulator: answers 200 with a catalog that agrees only on the first few AWS-service entries and then diverges: every index from $.ApplicationProviders[3] through $.ApplicationProviders[393] is reported missing, so no catalog/<ProviderName> SAML provider appears at all; where an entry does exist the order differs (index 4 is expected catalog/AdobeConnect but the emulator reports .../athena, index 5 expected catalog/CMNTY but reports .../emr); DisplayData.IconUrl is absent on every entry it does return; and the resource-server scope text differs from the published text (for applicationProvider/glue scope glue:read_write the recording has DetailedTitle "Permission to propagate trusted user-identity to Glue" and LongDescription "Enable trusted-identity propagation support in Glue", while the emulator reports "Read and write AWS Glue data" / "Grants access to read and write data through AWS Glue").
Why: a client that enumerates providers to let a user pick one, or that looks a provider up by its catalog/<Name> ARN before creating an application, finds the whole third-party SAML catalog absent and will treat those providers as unavailable (or get ResourceNotFoundException from a follow-up Describe); pagination over the catalog terminates far too early; and a portal rendering DisplayData has no icon and shows scope titles and descriptions that differ from the consent text AWS presents.
Fix: the same catalog seed in services/service-com-amazonaws-ssoadmin-swbexternalservice/src/state.rs (BUILT_IN_APPLICATION_PROVIDERS) — it must hold the full published provider set, in the published order, each entry with its DisplayData.IconUrl and its published scope DetailedTitle/LongDescription; the handler in src/ops/list_application_providers.rs already pages over whatever that catalog holds.
Traces: sso-admin-catalog-list-application-providers#task#1.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator: 83 steps, 80 passed, 3 mismatched, 0 errored, score 96.4%; both mismatch clusters are sso-admin and neither carries a stub_path or an owned_by.

Cause 1 — DescribeApplicationProvider, expected_status 200, actual_status 200, verdict mismatch, divergence_key service=sso-admin|DescribeApplicationProvider|status=200->200|keys=$.ApplicationProvider.DisplayData.IconUrl,$.DisplayData.IconUrl:
  $.DisplayData.IconUrl: expected 'https://static.global.sso.amazonaws.com/app-d77638588eff9057/icons/default.png', got 'https://127.0.0.1:40169/_aws/sso-application-provider-icons/catalog/Statuspage/icon.png'
  $.ApplicationProvider.DisplayData.IconUrl: same expected/actual pair (reported resource_not_identified)
  affected: sso-admin-application-provider-describe-round-trip#task#2, sso-admin-application-provider-not-found-describe-rejected#task#2 (affected_count 2)

Cause 2 — ListApplicationProviders, expected_status 200, actual_status 200, verdict mismatch, fixture sso-admin-catalog-list-application-providers step task#1:
  $.ApplicationProviders[3]: missing — expected {'ApplicationProviderArn': 'arn:aws:sso::aws:applicationProvider/glue', 'DisplayData': {'Description': 'AWS Glue Application', 'DisplayName': 'AWS Glue', 'IconUrl': 'https://static.global.sso.amazonaws.com/app-f6f06139684c6ff5/icons/default.png'}, 'FederationProtocol': 'OAUTH', 'ResourceServerConfig': {'Scopes': {'glue:read_write': {'DetailedTitle': 'Permission to propagate trusted user-identity to Glue', 'LongDescription': 'Enable trusted-identity propagation support in Glue'}}}}, got the same ARN with no DisplayData.IconUrl and Scopes.glue:read_write = {'LongDescription': 'Grants access to read and write data through AWS Glue', 'DetailedTitle': 'Read and write AWS Glue data'}
  $.ApplicationProviders[4]: missing — expected arn:aws:sso::aws:applicationProvider/catalog/AdobeConnect (FederationProtocol SAML, DisplayData with IconUrl), got arn:aws:sso::aws:applicationProvider/athena (OAUTH, no IconUrl)
  $.ApplicationProviders[5]: missing — expected arn:aws:sso::aws:applicationProvider/catalog/CMNTY (SAML, DisplayData with IconUrl), got arn:aws:sso::aws:applicationProvider/emr (OAUTH, no IconUrl)
  divergence_key lists missing entries continuing through $.ApplicationProviders[393] and a missing DisplayData.IconUrl on indices 0,1,2 and every other entry the emulator does return
  affected: sso-admin-catalog-list-application-providers#task#1 (affected_count 1)

Model consulted: services/service-com-amazonaws-ssoadmin-swbexternalservice/smithy-model/com-amazonaws-ssoadmin-swbexternalservice.json — DisplayData.IconUrl (@length 1..768, @pattern ^(http|https)://.*$), ApplicationProvider, ResourceServerScopeDetails (LongDescription/DetailedTitle), ListApplicationProviders (@paginated, MaxResults @range 1..100).

No open issue covers either cause: list_existing_issues found no entry matching 'sso-admin', 'ssoadmin' or 'ApplicationProvider'.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it; confirm /__admin/health answers 200.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus) — 3 of 83 steps mismatch, both clusters on sso-admin.
3. Cause 1: replay fixture sso-admin-application-provider-describe-round-trip, step task#2 (and sso-admin-application-provider-not-found-describe-rejected, step task#2). Call DescribeApplicationProvider with a catalog provider ARN (arn:aws:sso::aws:applicationProvider/catalog/Statuspage). The response is 200 but DisplayData.IconUrl is an emulator-local https://<host>:<port>/_aws/sso-application-provider-icons/... URL instead of the provider's published static.global.sso.amazonaws.com/app-<assetId>/icons/default.png icon URL.
4. Cause 2: replay fixture sso-admin-catalog-list-application-providers, step task#1. Call ListApplicationProviders and compare the ApplicationProviders list against the recording: the emulator's list ends after a few AWS-service providers (every index from 3 to 393 missing), contains no arn:aws:sso::aws:applicationProvider/catalog/<Name> SAML provider, omits DisplayData.IconUrl on every entry, and reports different DetailedTitle/LongDescription text for applicationProvider/glue scope glue:read_write.
5. Both must pass before closing: re-replay sso-admin-application-provider-describe-round-trip#task#2 AND sso-admin-catalog-list-application-providers#task#1.
