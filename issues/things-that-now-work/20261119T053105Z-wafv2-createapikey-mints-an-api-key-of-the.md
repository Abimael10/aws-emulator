Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-api-key-lifecycle
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-api-key-lifecycle wafv2-delete-apikey-nonexistent-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|CreateAPIKey|2xx->2xx|$.APIKey
Issue-Commit: a544e4ce699bdf0e8a989a341780b3fdd62801b0

# wafv2 CreateAPIKey mints an API key of the wrong width

> _Note: this issue was identified against the code base at commit `a544e4ce699bdf0e8a989a341780b3fdd62801b0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAPIKey

## Description
Cause 1 of 1 (fixture wafv2-api-key-lifecycle, step task#1): CreateAPIKey answers 200, but the $.APIKey value it mints is not of the form WAF mints an API key in — the replayed step's shape assertion on the key value fails. The alphabet/padding dimension reported by the earlier (now resolved) issue on this surface has been corrected — the value is now standard-base64 with '=' padding — but the key's WIDTH still diverges: the emulator renders a ~1864-character token where WAF's recorded key is a far shorter fixed-width envelope.

AWS: recorded behavior only for the value's syntax. Smithy (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json): CreateAPIKey -> CreateAPIKeyResponse$APIKey, target APIKey, @length(min 1, max 2048), @pattern("\\S"), documented as "The generated, encrypted API key. You can copy this for use in your JavaScript CAPTCHA integration."; CreateAPIKeyRequest$TokenDomains -> APIKeyTokenDomains (@length min 1) of TokenDomain (@length 1..253, @pattern ^[\\w\\.\\-/]+$), and the operation's own doc states a single key can be used for up to 5 domains. The model therefore fixes no further syntax and the requirement is the recorded one: the step asserts the key by SHAPE (the recorded secret itself is not compared — it is reported as '<shaped-not-asserted>'), and the recorded WAF key is an encrypted envelope rendered as padded standard base64 at ONE width that is a property of the key and not of the definition it was minted over. Hypothesis: the dimension still failing is that width — the emulator's value is correct in alphabet and padding and is inside @length's 2048 ceiling, so what remains to diverge is how many characters the envelope renders as, and WAF's key is around two orders of magnitude of payload smaller than the emulator's because WAF does not carry the widest permissible definition inside every key.

Emulator: the replayed step returns 200 with an APIKey of 1864 padded standard-base64 characters (ending '=='), which the replay reports as $.APIKey value_mismatch against the recorded shape. The width follows directly from the state module's constants: the plaintext of EVERY key is padded to API_KEY_SEALED_PAYLOAD_LEN = API_KEY_FIELDS_MAX (layout + the longer Scope word + a 20-digit creation instant + a 64-hex-character key id + 3 separators = 98) + MAX_TOKEN_DOMAINS_PER_API_KEY (5) * (1 + MAX_TOKEN_DOMAIN_LENGTH (253)) = 1368 bytes, plus a 12-byte nonce and a 16-byte AES-GCM tag = a 1396-byte envelope, which base64 renders as 1864 characters. Holding every key to one width is right; drawing that one width from the widest definition the model permits is what makes it the wrong width.

Why: a client that treats the API key as an opaque token WAF minted still receives a value of the wrong syntactic class for its width. Anything that validates or transports the key by the form WAF publishes — a fixture or SDK-level assertion on the key's width, a consumer that stores the key in a field sized for WAF's key, or JavaScript CAPTCHA integration code that embeds the key in a page or a header budgeted for WAF's token — succeeds against AWS and fails against the emulator. Because the width is the same for every mint, no request shape avoids it.

Fix: the APIKey minting in the wafv2 service state's API-key module — the width the sealed payload is padded to and the rendered-width check that reads it back, not the CreateAPIKey handler's validation; the handler already answers whatever the minter produced, and GetDecryptedAPIKey/DeleteAPIKey/ListAPIKeys read the same value back through the same form check, so the padded payload width, API_KEY_RENDERED_LEN and is_api_key_shaped must move together. Seal the envelope at the width WAF's recorded key renders at rather than at the widest definition the model allows, keeping the width independent of the token domains the request named.

Traces: wafv2-api-key-lifecycle#task#1 (representative) and wafv2-delete-apikey-nonexistent-rejected#task#4, both CreateAPIKey, both expected 200 / got 200 with $.APIKey diverging in shape.

## Evidence
Replay cluster divergence_key = service=wafv2|CreateAPIKey|status=200->200|keys=$.APIKey, verdict mismatch, affected_count 2, owned_by [] and stub_path "" (nothing already filed owns it).
Expected: status 200, $.APIKey matching the recorded WAF key's shape — reported as '<shaped-not-asserted>' because the literal secret is not compared.
Actual: status 200, $.APIKey = a padded standard-base64 string of 1864 characters ending in '==' (the run's value began 'hd00VJKE5cBX7psexxmC1xyi2QDIPrlBuyZTeO9lvV6qParDZ/c775z48BVJazwzxJHBl56ZFmMGnhEXjWnGle2j2epbUKAdW8qMZyVejilDPw7/NQXtqk5DkWIv...'). That value is a per-run secret and is quoted only to show the FORM and the width; it is not itself a requirement.
Source of the emulator's width: services/service-com-amazonaws-wafv2-awswaf-20190729/src/state/api_keys.rs lines 28-33 (API_KEY_NONCE_LEN 12, API_KEY_TAG_LEN 16), lines 48-86 (MAX_TOKEN_DOMAIN_LENGTH 253, API_KEY_FIELDS_MAX, API_KEY_SEALED_PAYLOAD_LEN = fields + 5 * 254 = 1368), lines 98-110 (API_KEY_ENVELOPE_LEN 1396, API_KEY_RENDERED_LEN = ceil(1396/3)*4 = 1864, asserted only against the model's 2048 ceiling), lines 131-137 (api_key_coder -> base64 STANDARD, i.e. the alphabet/padding is now correct), lines 149-180 (api_key_payload pads to the fixed width), lines 206-226 (new_api_key), lines 245-265 (api_key_envelope / is_api_key_shaped read the same width back). MAX_TOKEN_DOMAINS_PER_API_KEY is 5, per services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/create_api_key.rs lines 207-237. The value is returned unchanged by ops/create_api_key.rs lines 267-301.
Smithy evidence: CreateAPIKeyResponse$APIKey -> APIKey, @length(1..2048), @pattern(\S); no further syntax modeled, hence "recorded behavior only" for the rendered width.
Related already-filed issues, none of which owns this cluster: 20261108T045108Z-wafv2-createapikey-mints-an-api-key-value-in.md is RESOLVED (it covered the alphabet/padding and the definition-dependent width, both now gone, and a resolved issue covers no failure); the open 20261109T140923Z-wafv2-listapikeys-applicationintegrationurl-carries-a-32-character-integration.md covers ListAPIKeys' ApplicationIntegrationURL, and the open 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md covers missing refusals — neither mentions the minted key's width.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Fixture wafv2-api-key-lifecycle, step task#1: POST the wafv2 JSON-RPC target AWSWAF_20190729.CreateAPIKey with Scope=REGIONAL and a single-element TokenDomains list.
3. Read $.APIKey from the 200 response and measure it: it is 1864 padded standard-base64 characters regardless of how many token domains were named, where WAF's recorded key is a much shorter fixed-width envelope, so the step's shape assertion on the key fails.
4. Repeat with five token domains: the key is the same 1864 characters, confirming the width comes from the padded maximum definition rather than the request.
5. The same divergence reproduces at wafv2-delete-apikey-nonexistent-rejected#task#4, the other member of the cluster.
