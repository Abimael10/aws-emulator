Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-api-key-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-api-key-lifecycle wafv2-delete-apikey-nonexistent-rejected
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|ListAPIKeys|2xx->2xx|$.ApplicationIntegrationURL
Issue-Commit: 1ca24a91a90c9de292a3da3027bdc18df2a22899

# wafv2 ListAPIKeys publishes an ApplicationIntegrationURL on a host the emulator does not serve

> _Note: this issue was identified against the code base at commit `1ca24a91a90c9de292a3da3027bdc18df2a22899`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAPIKeys

## Description
Cause 1 of 1 (fixture wafv2-api-key-lifecycle, step task#2): ListAPIKeys answers 200, but the $.ApplicationIntegrationURL it reports names a host that is not the address the emulator is reached on, so the CAPTCHA JavaScript integration the URL advertises is unreachable against the emulator. The replay classifies this as host_not_own (an announced address the responder does not own), NOT as a plain value_mismatch.

AWS: recorded behavior only for this member's syntax. Smithy (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json): ListAPIKeys -> ListAPIKeysResponse$ApplicationIntegrationURL, target OutputUrl, a plain string with no @length and no @pattern, documented as "The CAPTCHA application integration URL, for use in your JavaScript implementation." The model therefore fixes no syntax and the requirement is the recorded one: WAF answers https://<id>.<region>.captcha-sdk.awswaf.com/<id>/ — the fixed text is the https:// scheme, the .captcha-sdk.awswaf.com suffix, the trailing / and the repetition of the SAME id in the leading host label and in the single path segment; the variable parts are the endpoint region and the account's integration id, and the real service ACTUALLY SERVES the JavaScript integration at the host it names. The key property of AWS's answer is that the URL it publishes resolves to an endpoint that serves the integration for the caller. The recorded id literal is an account-specific value from the recording's account and is NOT itself a requirement — only the URL's shape and the fact that the named host is served are.

Emulator: the replayed step returns 200 with $.ApplicationIntegrationURL = https://<12-hex-id>.<region>.captcha-sdk.awswaf.com/<same 12-hex-id>/ , where the id is derived from a SHA-256 digest over the literal CAPTCHA domain plus the emulator's account id. The scheme, the domain suffix, the region placement, the id repetition and the id's 12-lowercase-hex width are all now correct (an earlier, now-RESOLVED issue, 20261109T140923Z-wafv2-listapikeys-applicationintegrationurl-carries-a-32-character-integration.md, closed the width dimension; a resolved issue covers no failure, and replay reports this cluster with owned_by [] and stub_path ""). What still diverges is the ADDRESS: the host the emulator announces is a public AWS CAPTCHA-SDK domain the emulator neither owns nor serves, instead of the endpoint the ListAPIKeys request itself arrived on, so the URL names nothing the client can load from the emulator and the replay reports $.ApplicationIntegrationURL host_not_own.

Why: a client that follows the integration URL WAF published — a browser or build step that embeds it in a script tag, a test that fetches the CAPTCHA/challenge JS SDK from it, or code that derives a CSP/host allow-list entry and then loads from it — succeeds against AWS, where the named host serves the integration, and against the emulator either leaves the sandbox entirely or loads nothing, because the emulator published an address other than the one it answers on. Because the id and the domain are constants of the account, every ListAPIKeys answer is wrong the same way and no request shape avoids it.

Fix: the wafv2 service state's CAPTCHA integration helper that composes this URL (services/service-com-amazonaws-wafv2-awswaf-20190729/src/state/api_keys.rs, application_integration_url / captcha_integration_id / CAPTCHA_SDK_DOMAIN), not the ListAPIKeys handler — the handler returns whatever the helper renders, and the same helper feeds every other operation reporting this URL (e.g. GetWebACL's ApplicationIntegrationURL). Compose the URL from the endpoint the emulator is actually reached on (the request's host/authority), preserving WAF's published form — the repeated integration-id label/path segment and the trailing slash — so the address announced is one the emulator serves and a client can load the integration from it.

Traces: wafv2-api-key-lifecycle#task#2 (representative), plus the cluster's other members wafv2-delete-apikey-nonexistent-rejected#task#1 and wafv2-delete-apikey-nonexistent-rejected#task#3 — all three ListAPIKeys, all expected 200 / got 200 with $.ApplicationIntegrationURL diverging.

## Evidence
Replay cluster divergence_key = service=wafv2|ListAPIKeys|status=200->200|keys=$.ApplicationIntegrationURL, verdict mismatch, affected_count 3, owned_by [] and stub_path "" (nothing already filed owns it; the one prior issue on this replay key is in the resolved folder).
Expected: status 200, $.ApplicationIntegrationURL of WAF's published form 'https://<id>.<region>.captcha-sdk.awswaf.com/<id>/' at a host that actually serves the CAPTCHA integration — the recording's value was 'https://d1bdd7b0ad06.us-west-2.captcha-sdk.awswaf.com/d1bdd7b0ad06/', quoted only to show the FORM; that 12-hex id is the recording account's own value and is not a requirement.
Actual: status 200, $.ApplicationIntegrationURL = 'https://aa730e0b0523.us-west-2.captcha-sdk.awswaf.com/aa730e0b0523/' — correct shape and width, but the host is a public awswaf.com name the emulator does not serve; the replay reports the key as host_not_own rather than value_mismatch, i.e. the announced address is not one the responder owns.
Source of the emulator's value: services/service-com-amazonaws-wafv2-awswaf-20190729/src/state/api_keys.rs line 313 (CAPTCHA_SDK_DOMAIN = "captcha-sdk.awswaf.com"), lines 315-327 (CAPTCHA_INTEGRATION_ID_LENGTH = 12, CAPTCHA_INTEGRATION_ID_BYTES), lines 351-359 (captcha_integration_id: SHA-256 over the domain plus engine::emulator_account_id(), hex-rendered), lines 375-378 (application_integration_url formats https://{integration}.{region}.{CAPTCHA_SDK_DOMAIN}/{integration}/ — the serving endpoint is never consulted).
Smithy evidence: ListAPIKeysResponse$ApplicationIntegrationURL -> OutputUrl, string shape with no length or pattern trait, doc "The CAPTCHA application integration URL, for use in your JavaScript implementation." — hence "recorded behavior only" for the URL's syntax.
Related open wafv2 issues, none of which owns this cluster or mentions the announced host: 20261119T053105Z-wafv2-createapikey-mints-an-api-key-of-the.md (minted $.APIKey width), 20261029T192556Z-wafv2-request-validation-never-refuses-ten-operations-answer.md (missing refusals), 20261117T233522Z-wafv2-web-acl-subordinate-state-diverges-logging-configuration.md (web-ACL subordinate state).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Fixture wafv2-api-key-lifecycle, step task#2: POST the wafv2 JSON-RPC target AWSWAF_20190729.ListAPIKeys with Scope=REGIONAL against the emulator's us-west-2 data endpoint.
3. Read $.ApplicationIntegrationURL from the 200 response: it is https://<12 hex chars>.us-west-2.captcha-sdk.awswaf.com/<same 12 hex chars>/ — a host the emulator does not serve and that does not resolve to the endpoint the request was just answered on.
4. Try to load the CAPTCHA JavaScript integration from that URL from inside the same sandbox the SDK call was made in: nothing is served, where against AWS the published host serves the integration.
5. The same divergence reproduces at wafv2-delete-apikey-nonexistent-rejected#task#1 and #task#3, the other members of the cluster.
