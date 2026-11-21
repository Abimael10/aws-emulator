Issue-Source: trace-sweep-divergence
Issue-Trace: sso-oidc-register-public-client-basic
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sso-oidc-register-public-client-basic
Issue-Shape-Id: com.amazonaws.ssooidc#AWSSSOOIDCService
Issue-Key: oidc|RegisterClient|2xx->2xx|$.clientSecret
Issue-Commit: 32b3b13ce09ea200080835375d94fd3da2d5fa49

# oidc RegisterClient returns a freshly drawn random clientSecret instead of the registration's own secret

> _Note: this issue was identified against the code base at commit `32b3b13ce09ea200080835375d94fd3da2d5fa49`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterClient

## Description
Cause 1 of 1 — RegisterClient's `clientSecret` is not the secret the registration it answers for is identified by; it is a fresh random value drawn on every call. Fixture sso-oidc-register-public-client-basic, step task#1 (RegisterClient, public client, basic registration).

AWS: RegisterClient (POST /client/register, success 200) answers a registration with clientId, clientSecret, clientIdIssuedAt, clientSecretExpiresAt, and optionally authorizationEndpoint/tokenEndpoint (Smithy: com.amazonaws.ssooidc#RegisterClient -> RegisterClientResponse; member clientSecret targets ClientSecret, a @sensitive string with NO length or pattern constraint, documented as "A secret string generated for the client. The client will use this string to get authenticated by the service in subsequent calls."). The model therefore states nothing about the secret's value or its form — for that part this is recorded behavior only: the recording shows the 200 response for this registration carrying the secret the corpus binds as the registration's credential ({{client_secret_basic}}), i.e. the one value the rest of a recorded trace presents back to authenticate this client. The status (200), the response shape and every other member of this step, clientId included, already match; only $.clientSecret diverges, so the step is not a status, error-code or serialization divergence — it is the response member that carries the registration's credential.

Emulator: answers the same 200 and the same member set, but $.clientSecret is a 64-character lowercase-hex value drawn from the system random source on every single call, unrelated to the registration's identity or to anything in the request. Replay output for this step: `$.clientSecret: value_mismatch (expected '{{client_secret_basic}}', got '<64 hex chars>')` — the hex digits themselves are a per-run value and are NOT the requirement; what diverges as a rule is that the member's value is redrawn per call and is never the secret the registration being registered is bound to. The emulator keeps the value it drew (it is stored on the registration and accepted later by client authentication), so the emulator is self-consistent within one process; it is across a recorded registration that the value is not reproducible.

Why: clientSecret is the registration's only credential — "the client will use this string to get authenticated by the service in subsequent calls" — so a caller that holds a registration recorded earlier (a replayed trace, a cached registration in an SDK/CLI profile, a fixture-driven test that pins a client) and presents that clientId/clientSecret pair to CreateToken or CreateTokenWithIAM is refused invalid_client rather than authenticated, and every step of such a flow past registration is unreachable. The divergence is invisible to anyone comparing statuses or error codes: the response looks correct and only the credential it hands out is not the registration's.

Fix: belongs in RegisterClient's handler and the credential-minting helper it uses, not in serialization — services/service-com-amazonaws-ssooidc-awsssooidcservice/src/ops/register_client.rs mints `client_secret: st::mint_opaque_value()` beside `client_id`, and services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs's `mint_opaque_value()` concatenates two random v4 UUIDs' hex digits (the "244 bits drawn from the system random source" comment). Hypothesis (stated by neither the model nor the recording): the secret a registration is answered with should be stable for that registration — derived from, or looked up for, the client metadata being registered (clientName/clientType/grantTypes/redirectUris/scopes) and re-used when that same registration is registered again, instead of a new draw per request — which is what would let a recorded registration's secret be reproduced. Whichever stabilization is chosen, it belongs in the registration path in register_client.rs together with the store's client insert/lookup in state.rs, so the value handed back and the value later accepted by client authentication stay the same one.

Traces: sso-oidc-register-public-client-basic#task#1 (mismatch, expected status 200 / actual status 200, diverging key $.clientSecret). Not covered by the one open sso-oidc issue, 20261117T132850Z-sso-oidc-createtokenwithiam-token-exchange-with-no-subjecttoken.md, which is CreateTokenWithIAM's missing-subjectToken error_description casing and names no RegisterClient step.

## Evidence
Replay of the wire corpus simparity-wire-aws-v1 (trace_set_id: simparity-wire-aws-v1; 6 steps, 5 passed, 1 mismatched, score 83.3%).

Divergence cluster (verdict: mismatch, affected_count 1):
  fixture_id : sso-oidc-register-public-client-basic
  step_key   : task#1
  service    : oidc
  operation  : RegisterClient
  expected_status : 200
  actual_status   : 200
  divergence_key  : service=oidc|RegisterClient|status=200->200|keys=$.clientSecret
  detail     : $.clientSecret: value_mismatch (expected '{{client_secret_basic}}', got
               '423e45cae8374a18a6a0d0ea4d7900278654114b84954024a79a2102e0aae193')
  stub_path  : (empty — the operation is implemented, nothing pre-filed)
  owned_by   : (empty — no open issue carries this replay key)

Expected vs actual, as a rule rather than as these values:
  Expected: $.clientSecret = the secret this registration is bound to in the recording
            (the corpus binds it as {{client_secret_basic}}), the value a later step of a
            trace presents back as the client's credential.
  Actual:   $.clientSecret = a value redrawn at random on every call; the literal hex
            string above is one run's draw and is NOT part of the requirement. Its form is
            64 lowercase hex characters, which is this emulator's own choice and is not
            stated by the model either.
  Matching: HTTP status 200, the response member set, and every other member of this step
            (including $.clientId) — only $.clientSecret is listed as diverging.

Smithy (services/service-com-amazonaws-ssooidc-awsssooidcservice/smithy-model/com-amazonaws-ssooidc-awsssooidcservice.json):
  RegisterClient: http POST /client/register, success code 200;
    output RegisterClientResponse { clientId: ClientId, clientSecret: ClientSecret (sensitive),
    clientIdIssuedAt: LongTimeStampType (default 0), clientSecretExpiresAt: LongTimeStampType
    (default 0), authorizationEndpoint: URI, tokenEndpoint: URI }
  ClientSecret: string, @sensitive, no @length / @pattern — the model constrains neither the
    secret's value nor its form, so the value's stability is recorded behavior only.
  clientSecret doc: "A secret string generated for the client. The client will use this string
    to get authenticated by the service in subsequent calls."

Emulator source of the value:
  services/service-com-amazonaws-ssooidc-awsssooidcservice/src/ops/register_client.rs
    let client = st::OidcClient { client_id: st::mint_opaque_value(),
                                  client_secret: st::mint_opaque_value(), ... };
    ... client_secret: Some(client.client_secret.clone()) in RegisterClientOutput
  services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs
    pub fn mint_opaque_value() -> String  // two random v4 UUIDs' hex digits concatenated,
                                          // "244 bits drawn from the system random source"
  Consequence inside the service: a pair whose secret is not the registration's is refused with
  error invalid_client / description "Invalid client secret provided"
  (state.rs: DESCRIPTION_INVALID_CLIENT_SECRET), which is what a caller replaying a recorded
  registration's credential meets.

## Steps to Reproduce
1. Boot the emulator (cargo run -p aws-emulator-router) and point an sso-oidc client at it.
2. Replay the wire corpus and watch this one step:
     fixture sso-oidc-register-public-client-basic, step task#1
   It reports: $.clientSecret value_mismatch, expected the registration's bound secret
   ({{client_secret_basic}}), got a 64-hex value; status 200 on both sides and no other key
   diverging.
3. By hand, show that the member is redrawn per call rather than being the registration's:
     aws sso-oidc register-client --client-name trace-finder --client-type public
   Note clientId/clientSecret, then run the SAME command again with the same client metadata.
   Every call answers a different 64-character hex clientSecret; no call reproduces a secret a
   previous registration of the same client was answered with.
4. Show what a holder of a recorded registration meets: take a clientId/clientSecret pair from
   one registration, then present that clientId with ANY other registration's secret (or with a
   secret recorded in an earlier session) to CreateToken / CreateTokenWithIAM. It is refused with
   error invalid_client, error_description "Invalid client secret provided" — so a replayed or
   cached registration cannot authenticate.
5. After the fix, re-replay sso-oidc-register-public-client-basic#task#1 and confirm the step
   passes with status 200 and no diverging key.
