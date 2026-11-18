Issue-Source: trace-sweep-divergence
Issue-Trace: appfabric-app-authorization-oauth2-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appfabric-app-authorization-oauth2-lifecycle appfabric-resource-not-found-and-conflict-errors
Issue-Shape-Id: com.amazonaws.appfabric#FabricFrontEndService
Issue-Key: appfabric|CreateAppAuthorization|2xx->2xx|$.appAuthorization.authUrl
Issue-Key: appfabric|GetAppAuthorization|2xx->2xx|$.appAuthorization.authUrl
Issue-Key: appfabric|UpdateAppAuthorization|2xx->2xx|$.appAuthorization.authUrl
Issue-Commit: b76ea3d380da1c44bf4c6dbddd19ccb858da307c

# AppFabric reports a diverging authUrl for oauth2 app authorizations on create, get and update

> _Note: this issue was identified against the code base at commit `b76ea3d380da1c44bf4c6dbddd19ccb858da307c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAppAuthorization

## Description
The `authUrl` member of `AppAuthorization` is wrong for every oauth2 app authorization, on each of the three operations that report the shape. The emulator does not report the URL AppFabric returns for the authorization: it SYNTHESIZES one locally in `crate::state::oauth_flow_url` (the emulator's own endpoint host for the owning region plus the `ConnectAppAuthorization` HTTP URI, `https://appfabric.<region>.amazonaws.com/appbundles/<bundleUuid>/appauthorizations/<authUuid>/connect`), and `AppAuthorizationRecord::auth_url` hands that string to `to_app_authorization`, which is the single builder Create, Get and Update all serialize from. So the create response carries a value the recording does not, and every later read of the same authorization carries that same non-matching value — the replay reports one divergence cluster per operation. THREE CAUSES, all one fault; all three must be closed.

Cause 1 of 3 — CreateAppAuthorization (fixture appfabric-app-authorization-oauth2-lifecycle, step task#2, HTTP 201 both sides).
AWS: Smithy: `CreateAppAuthorization` -> output `appAuthorization` (required, shape `AppAuthorization`); member `AppAuthorization$authUrl` (target `String`, optional) is documented as "The application URL for the OAuth flow" — the URL at which the consent step of the oauth2 grant is performed, whose outcome the caller then passes to `ConnectAppAuthorization$authRequest` ("Contains OAuth2 authorization information. This is required if the app authorization for the request is configured with an OAuth2 (oauth2) authorization type"). The model does not state the URL's composition, so for the value itself this is recorded behavior only: the recording establishes one authUrl for the created oauth2 authorization, which the fixture captures as its `auth_url_create` value and which every later step of the lifecycle re-asserts.
Emulator: for authType `oauth2` the handler returns the locally composed AppFabric connect-URI string described above instead of the recorded authUrl; the replay reports `$.appAuthorization.authUrl: value_mismatch (expected '{{auth_url_create}}', got 'https://appfabric.us-east-1.amazonaws.com/appbundles/<bundleUuid>/appauthorizations/<authUuid>/connect')`. The two uuids and the region in that string are per-run state values, not part of the requirement; the fault is that the member is computed from the emulator's own endpoint and URI rather than reported as the service's authUrl.
Why: a client that drives the oauth2 grant reads `authUrl` out of the create response and sends the user there to grant permission. Pointed at an AppFabric control-plane URI instead of the application's OAuth flow URL, the consent step never runs, no authorization code is ever produced, and the client can never supply the `authRequest` that `ConnectAppAuthorization` requires for an oauth2 authorization — the whole authorize -> connect -> ingest chain stalls at the first step.
Fix: `CreateAppAuthorization`'s response construction, i.e. the state-level builder it reads back from — `AppAuthorizationRecord::auth_url` / `oauth_flow_url` — must report the authorization's stored OAuth flow URL rather than deriving one from the emulator's endpoint host and the `ConnectAppAuthorization` URI; the value must be persisted on the record at create time so the same bytes are reported by every later read.
Traces: appfabric-app-authorization-oauth2-lifecycle#task#2.

Cause 2 of 3 — GetAppAuthorization (fixture appfabric-app-authorization-oauth2-lifecycle, step task#3, HTTP 200 both sides; cluster also covers appfabric-resource-not-found-and-conflict-errors#task#7).
AWS: Smithy: `GetAppAuthorization` (GET /appbundles/{appBundleIdentifier}/appauthorizations/{appAuthorizationIdentifier}) -> output `appAuthorization` (required). Recorded behavior only for the value: a read of an oauth2 authorization reports the SAME `authUrl` the create call reported for it — the recorded Get response carries exactly the value the recorded create established.
Emulator: the Get response's `$.appAuthorization.authUrl` does not compare equal to the authUrl the recorded create established for that authorization: `value_mismatch (expected 'https://appfabric.us-east-1.amazonaws.com/appbundles/<bundleUuid>/appauthorizations/<authUuid>/connect', got 'https://appfabric.us-east-1.amazonaws.com/appbundles/<bundleUuid>/appauthorizations/<authUuid>/connect')`. The two renderings print alike, so the composed string is not byte-identical to the required value. Hypothesis: the composition in `oauth_flow_url` introduces a character that does not render — stray surrounding whitespace, or a non-ASCII character in the host/URI template — in addition to being the wrong URL per cause 1.
Why: a client that stores the authUrl from the create response and re-reads the authorization (or that only ever reads it back, e.g. after a restart) gets a value it cannot match against the one it holds, and cannot resume an in-flight consent step. Any equality or signature check over the member fails even though the resource did not change.
Fix: the same builder as cause 1 — `GetAppAuthorization`'s read path must serialize the authorization's STORED authUrl verbatim, byte for byte, with no re-composition at read time.
Traces: appfabric-app-authorization-oauth2-lifecycle#task#3; appfabric-resource-not-found-and-conflict-errors#task#7.

Cause 3 of 3 — UpdateAppAuthorization (fixture appfabric-app-authorization-oauth2-lifecycle, step task#4, HTTP 200 both sides).
AWS: Smithy: `UpdateAppAuthorization` (PATCH /appbundles/{appBundleIdentifier}/appauthorizations/{appAuthorizationIdentifier}) -> output `appAuthorization` (required); the operation updates `credential`/`tenant` only and has no member that re-identifies the authorization. Recorded behavior only: the update response reports the SAME `authUrl` the authorization already had — rotating a credential does not move the OAuth flow URL.
Emulator: `$.appAuthorization.authUrl` in the update response again fails to compare equal to the authUrl established for the authorization, with the same two identical-looking renderings as cause 2, because the update response is built by the same `to_app_authorization` call.
Why: a client that updates the oauth2 credential and then re-drives the consent step is handed a URL that disagrees with the one AppFabric holds for the authorization, so re-authorizing after a credential rotation fails.
Fix: the same builder as cause 1, reached from `UpdateAppAuthorization`'s response construction; the member must be reported from stored state and must be unchanged by an update that does not touch it.
Traces: appfabric-app-authorization-oauth2-lifecycle#task#4.

Not covered by anything already filed: the two open AppFabric issues are about `ConnectAppAuthorization` not validating credentials (20261106T165607Z-appfabric-connectappauthorization-never-validates-the-credential-a-failing.md) and about PAGERDUTY being rejected as an unsupported application (20261113T034141Z-appfabric-rejects-the-supported-application-pagerduty-breaking-authorization.md, which owns the other three clusters of this replay). Neither mentions `authUrl`, and this lifecycle fixture succeeds on status for all three steps.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 77.4%, 8 mismatched steps, 6 clusters; 3 clusters owned by the open PAGERDUTY issue).

Cause 1 — fixture appfabric-app-authorization-oauth2-lifecycle, step task#2, CreateAppAuthorization, expected HTTP 201 / actual HTTP 201:
  $.appAuthorization.authUrl: value_mismatch
    expected: '{{auth_url_create}}'  (the fixture's captured authUrl of the created oauth2 authorization)
    actual:   'https://appfabric.us-east-1.amazonaws.com/appbundles/2f1be9f3-676c-45ef-b590-3dae0f6cc4d9/appauthorizations/7c58e8bc-4614-42c2-8bab-a842107c8e16/connect'
  divergence_key: service=appfabric|CreateAppAuthorization|status=201->201|keys=$.appAuthorization.authUrl

Cause 2 — fixture appfabric-app-authorization-oauth2-lifecycle, step task#3, GetAppAuthorization, expected HTTP 200 / actual HTTP 200 (cluster of 2; also appfabric-resource-not-found-and-conflict-errors#task#7):
  $.appAuthorization.authUrl: value_mismatch
    expected: 'https://appfabric.us-east-1.amazonaws.com/appbundles/2f1be9f3-676c-45ef-b590-3dae0f6cc4d9/appauthorizations/7c58e8bc-4614-42c2-8bab-a842107c8e16/connect'
    actual:   'https://appfabric.us-east-1.amazonaws.com/appbundles/2f1be9f3-676c-45ef-b590-3dae0f6cc4d9/appauthorizations/7c58e8bc-4614-42c2-8bab-a842107c8e16/connect'
    (the two renderings are indistinguishable in print, so the served string is not byte-identical to the required one)
  divergence_key: service=appfabric|GetAppAuthorization|status=200->200|keys=$.appAuthorization.authUrl

Cause 3 — fixture appfabric-app-authorization-oauth2-lifecycle, step task#4, UpdateAppAuthorization, expected HTTP 200 / actual HTTP 200:
  $.appAuthorization.authUrl: value_mismatch, same expected/actual pair as cause 2.
  divergence_key: service=appfabric|UpdateAppAuthorization|status=200->200|keys=$.appAuthorization.authUrl

Source of the served value (citations only, not a patch):
  services/service-com-amazonaws-appfabric-fabricfrontendservice/src/state.rs:1509-1514  oauth_flow_url(): format!("https://{host}/appbundles/{app_bundle_id}/appauthorizations/{authorization_id}/connect") over endpoint_host(region)
  services/service-com-amazonaws-appfabric-fabricfrontendservice/src/state.rs:606-612     AppAuthorizationRecord::auth_url() -> Some(oauth_flow_url(..)) when authType == oauth2, else None
  services/service-com-amazonaws-appfabric-fabricfrontendservice/src/state.rs:615-628     to_app_authorization(): auth_url: self.auth_url() — the one builder used by create (ops/create_app_authorization.rs:444), get (ops/get_app_authorization.rs:179-182) and update

Model facts used: smithy_query(operation="CreateAppAuthorization,GetAppAuthorization,UpdateAppAuthorization") and smithy_query(shape="AppAuthorization") on services/service-com-amazonaws-appfabric-fabricfrontendservice/smithy-model/com-amazonaws-appfabric-fabricfrontendservice.json — authUrl: String, optional, "The application URL for the OAuth flow"; all three operations return the required `appAuthorization` member; ConnectAppAuthorizationRequest$authRequest is required for an oauth2 authorization.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture appfabric-app-authorization-oauth2-lifecycle up to step task#2 — CreateAppAuthorization under the fixture's app bundle with authType=oauth2 and an oauth2Credential. Read $.appAuthorization.authUrl from the 201 response: it is the emulator-composed https://appfabric.<region>.amazonaws.com/appbundles/<bundleUuid>/appauthorizations/<authUuid>/connect, not the authUrl the recording establishes for the authorization.
3. Cause 2: continue to step task#3 — GET /appbundles/{appBundleIdentifier}/appauthorizations/{appAuthorizationIdentifier} for the authorization just created. Compare $.appAuthorization.authUrl byte for byte (e.g. hexdump it) against the value the recorded create established: the 200 response's value is unequal although it prints the same.
4. Cause 3: continue to step task#4 — PATCH the same authorization (UpdateAppAuthorization with a new credential/tenant) and compare $.appAuthorization.authUrl the same way; the 200 response diverges identically.
5. The same Get divergence reproduces in fixture appfabric-resource-not-found-and-conflict-errors at step task#7.
