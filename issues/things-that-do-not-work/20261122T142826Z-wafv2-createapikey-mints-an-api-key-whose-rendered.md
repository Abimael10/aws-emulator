Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-api-key-lifecycle
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-api-key-lifecycle wafv2-delete-apikey-nonexistent-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|CreateAPIKey|2xx->2xx|$.APIKey
Issue-Commit: 4d8b014dbcb3833e6a7aad4896bf06c6aff71e41

# wafv2 CreateAPIKey mints an API key whose rendered form still fails WAF's recorded key shape

> _Note: this issue was identified against the code base at commit `4d8b014dbcb3833e6a7aad4896bf06c6aff71e41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAPIKey

## Description
Cause 1 of 1 (fixture wafv2-api-key-lifecycle, step task#1): CreateAPIKey answers 200, but the $.APIKey value it mints is not of the form WAF mints an API key in, so the replayed step's SHAPE assertion on the key value fails and the replay reports $.APIKey value_mismatch against '<shaped-not-asserted>'.

AWS: recorded behavior only for the value's syntax. Smithy (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json): CreateAPIKey -> CreateAPIKeyResponse$APIKey, target APIKey, @length(min 1, max 2048), @pattern("\\S"), documented as "The generated, encrypted API key. You can copy this for use in your JavaScript CAPTCHA integration."; APIKeySummary$APIKey targets the same shape, and GetDecryptedAPIKey "returns your API key in decrypted form" so a caller can read back the token domains the key was defined over. The model therefore fixes NO further syntax beyond non-whitespace and a 2048 ceiling, and the requirement is the recorded one: the recorded step does not compare the secret itself (it is reported as '<shaped-not-asserted>'), it asserts the key by the SHAPE WAF renders every key in — an encrypted envelope, rendered in one alphabet, at a width that is a property of WAF's own envelope and that a client only ever copies verbatim into its JavaScript CAPTCHA integration and hands back. The requirement is that a minted key be a value of that shape; no literal key value, which is a per-run secret, is part of it.

Emulator: the replayed step returns 200 with $.APIKey = a padded standard-base64 string of exactly 96 characters (the run's value ended '=='), which the replay reports as $.APIKey value_mismatch. The width follows from the state module's constants: the sealed plaintext is API_KEY_SEALED_PAYLOAD_LEN = 1 (layout marker) + 1 (scope code) + 8 (big-endian i64 creation instant) + 32 (key identifier) = 42 bytes, so API_KEY_ENVELOPE_LEN = 12 (nonce) + 42 + 16 (AES-GCM tag) = 70 bytes, and API_KEY_RENDERED_LEN = ceil(70/3)*4 = 96 characters, asserted only against the model's 2048 ceiling and against being narrower than the widest permissible token-domain list. Two earlier dimensions of this same surface have been corrected and are now in the RESOLVED folder (20261108T045108Z-wafv2-createapikey-mints-an-api-key-value-in.md, the alphabet/padding; 20261119T053105Z-wafv2-createapikey-mints-an-api-key-of-the.md, the previous 1864-character width padded out from the widest definition the model allows) — a resolved issue covers no failure, and replay reports this cluster with owned_by [] and stub_path "". So the alphabet is now standard base64 with '=' padding and the width no longer varies with the request, yet the recorded shape assertion still fails. Hypothesis: the dimension still diverging is the rendered WIDTH — with alphabet, padding and definition-independence all correct and the value well inside @length, what is left for the shape check to reject is how many characters the envelope renders as, and the emulator's 96-character token (a 70-byte envelope carrying only a layout byte, a scope code, a timestamp and a 32-byte id) is far shorter than WAF's recorded encrypted key, which seals enough state for WAF itself to decrypt a key back to its token domains.

Why: a client that treats the API key as an opaque token WAF minted receives a value of the wrong syntactic class. Anything that validates or transports the key by the form WAF publishes — an SDK-level or fixture assertion on the key's shape/width, a store or header budgeted for WAF's token, or JavaScript CAPTCHA integration code that embeds the key and expects WAF to accept and decrypt it — succeeds against AWS and diverges against the emulator. Because the width is the same for every mint, every CreateAPIKey answer is wrong the same way and no request shape avoids it.

Fix: the APIKey minting in the wafv2 service state's API-key module — the width the sealed envelope is composed at and the rendered-width check that reads it back — not the CreateAPIKey handler's validation; the handler answers whatever the minter produced unchanged, and ListAPIKeys, GetDecryptedAPIKey and DeleteAPIKey all judge a key's form through the same check, so the payload layout, the envelope width, API_KEY_RENDERED_LEN and is_api_key_shaped must move together. Seal and render the envelope at the width WAF's recorded key renders at, keeping the width independent of the token domains the request named.

Traces: wafv2-api-key-lifecycle#task#1 (representative) and wafv2-delete-apikey-nonexistent-rejected#task#4 — both CreateAPIKey, both expected 200 / got 200 with $.APIKey diverging in shape.

## Evidence
Replay cluster divergence_key = service=wafv2|CreateAPIKey|status=200->200|keys=$.APIKey, verdict mismatch, affected_count 2, owned_by [] and stub_path "" (nothing already filed owns this replay key; the two prior issues on this surface are both in the resolved folder).
Expected: status 200 with $.APIKey matching the recorded WAF key's shape — the recording reports it as '<shaped-not-asserted>' because the literal secret is not compared.
Actual: status 200 with $.APIKey = 'SODTvozXyPgTpzIsF7pz1uwOMfkzPsoXtnkA7uGOSrifZDYFQubQV7pvkuG+w57XVRjfOwobVr3Y/EPAWbzy7VMN/ao+uw==' — 96 characters of padded standard base64. That value is a per-run secret quoted ONLY to show the form and the width (96 characters, standard alphabet A-Za-z0-9+/, '==' padding); it is not itself a requirement.
Status and error code: none diverge here — the operation answers the modeled 200 and no error class is involved; the divergence is entirely in the response member $.APIKey.
Source of the emulator's width: services/service-com-amazonaws-wafv2-awswaf-20190729/src/state/api_keys.rs lines 31-36 (API_KEY_NONCE_LEN 12, API_KEY_TAG_LEN 16), lines 40-77 (layout marker, 1-byte scope code, 8-byte creation instant, 32-byte identifier and their offsets), lines 91-92 (API_KEY_SEALED_PAYLOAD_LEN = 42), lines 101-106 (API_KEY_ENVELOPE_LEN = 70, API_KEY_RENDERED_LEN = ceil(70/3)*4 = 96), lines 108-124 (the only assertions on that width are <= 2048 and < 5*253), lines 145-151 (api_key_coder -> base64 STANDARD, i.e. alphabet and padding are correct), lines 183-191 (api_key_payload), lines 218-234 (new_api_key renders the envelope), lines 253-273 (api_key_envelope / is_api_key_shaped read the same 96-character width back). The value is returned unchanged by services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/create_api_key.rs lines 268-301.
Smithy evidence: CreateAPIKeyResponse$APIKey -> APIKey, @length(1..2048), @pattern(\S); APIKeySummary$APIKey the same shape, doc "The generated, encrypted API key..."; no alphabet, width or structure is modeled, hence "recorded behavior only" for the rendered form.
Related already-filed OPEN wafv2 issues, none of which owns this cluster or mentions the minted key's rendered form: 20261120T200504Z-wafv2-listapikeys-publishes-an-applicationintegrationurl-on-a-host.md (ListAPIKeys' ApplicationIntegrationURL host), 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md (missing 400 refusals), 20261117T233522Z-wafv2-web-acl-subordinate-state-diverges-logging-configuration.md (web-ACL subordinate state).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Fixture wafv2-api-key-lifecycle, step task#1: POST the wafv2 JSON-RPC target AWSWAF_20190729.CreateAPIKey with Scope=REGIONAL and a single-element TokenDomains list against the emulator's us-west-2 data endpoint.
3. Read $.APIKey from the 200 response and measure it: it is exactly 96 characters of padded standard base64, where the recorded WAF key is an encrypted envelope of a different rendered width, so the step's shape assertion on the key value fails ($.APIKey value_mismatch against '<shaped-not-asserted>').
4. Repeat with five token domains, and repeat in Scope=CLOUDFRONT: the key is still 96 characters, confirming the width is a build constant (API_KEY_RENDERED_LEN) rather than anything drawn from the request — so every mint diverges identically.
5. The same divergence reproduces at wafv2-delete-apikey-nonexistent-rejected#task#4, the other member of the cluster.
