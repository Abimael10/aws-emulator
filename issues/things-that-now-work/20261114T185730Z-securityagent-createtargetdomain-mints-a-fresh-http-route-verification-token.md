Issue-Source: trace-sweep-divergence
Issue-Trace: securityagent-target-domain-tagging-and-errors
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: securityagent-target-domain-tagging-and-errors
Issue-Shape-Id: com.amazonaws.securityagent#SecurityAgent
Issue-Key: securityagent|CreateTargetDomain|2xx->2xx|$.verificationDetails.httpRoute.token
Issue-Commit: 0c97311e0491e6ff934cad5e77765048b7e1abe2

# CreateTargetDomain mints a fresh HTTP-route verification token per call instead of the registered domain's own stable challenge

> _Note: this issue was identified against the code base at commit `0c97311e0491e6ff934cad5e77765048b7e1abe2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTargetDomain

## Description
ONE net-new cause. The replay reports it as an unowned mismatch (owned_by empty, no stub_path): status and body shape are correct, only the HTTP-route ownership-verification challenge value diverges.

Cause 1 of 1 (fixture securityagent-target-domain-tagging-and-errors, step setup#1, CreateTargetDomain): the challenge token the create response publishes for an HTTP_ROUTE target domain is not the token the corpus has already bound for that domain — the emulator draws a brand-new secret on every CreateTargetDomain call, so the challenge value a registered domain is told to publish is per-CALL rather than per-DOMAIN.

AWS: Smithy (services/service-com-amazonaws-securityagent-securityagent/smithy-model/com-amazonaws-securityagent-securityagent.json): CreateTargetDomain POST /CreateTargetDomain, success 200, input targetDomainName/verificationMethod (@required, DomainVerificationMethod DNS_TXT|HTTP_ROUTE|PRIVATE_VPC) + tags; output targetDomainId (@required), domainName (@required), verificationStatus (@required, TargetDomainStatus), verificationStatusReason, verificationDetails (VerificationDetails{method, dnsTxt: DnsVerification, httpRoute: HttpVerification}), createdAt, verifiedAt. HttpVerification.token is an unconstrained String documented 'The verification token to serve at the specified route path' (DnsVerification.token: 'The verification token to include in the DNS record value'), so the model pins no format — recorded behavior only for the value itself. What the recording pins is the token's IDENTITY, not its bytes: the replay's expectation at this step is the corpus-bound token for this domain ({{http_route_token_2}}), a value the corpus has already established before this step runs, so the recorded service answers a registration of that domain name with the challenge value that domain already has rather than with a newly drawn one. Hypothesis: the real service keys the proof-of-control challenge to the (account, domainName) pair, so re-registering a domain re-issues the same token and a token the owner already published at /.well-known/aws/securityagent-domain-verification.json stays valid across registrations; the recorded targetDomainId at this same step IS a fresh value (it is bound as its own placeholder and the emulator's td-<uuid> satisfies it), which is what shows the stability is the token's property and not the record's.

Emulator: HTTP 200 with the right members, but $.verificationDetails.httpRoute.token is a freshly drawn 64-character lowercase hex SHA-256 digest ('9b1e8b7bd29a7f51cbff83e388fc95862994c29746215218a2937d9f4c63dd08' in this run) where the step requires the already-bound {{http_route_token_2}}. The value is unique and well-formed — the earlier constant-token defect is gone — but it is drawn per call: src/state.rs ComAmazonawsSecurityagentSecurityagentState::mint_target_domain_identity calls IdentifierMint::next_secret(VERIFICATION_TOKEN_CONTEXT), which digests the context, the mint's seeded namespace, a never-reused counter AND SystemTime::now() in nanoseconds, so no two calls can ever agree on a token, not even two calls naming the same domain. src/ops/create_target_domain.rs states the choice outright (lines 235-240: 'A fresh identity per call ... the domain name is not an alternate key'), and src/state.rs TargetDomainRecord::verification_details then serves that per-call secret as httpRoute.token.

Why: the token is a proof-of-control challenge the client must publish on the domain before verification can succeed. Against the real service a client can register (or re-register) a domain and keep serving the challenge document it already published, because the domain's token does not change; against the emulator every CreateTargetDomain hands out a different value, so a previously published .well-known document (or DNS TXT record, which is minted the same way) silently stops matching and ownership verification can never be made to pass for a domain that was registered more than once. A client that caches the token per domain name, or that compares the token it holds with the token a fresh create reports, diverges on every call.

Fix: the CreateTargetDomain handler's identity minting and the target-domain state that backs it — the verification token must be a property of the registered domain (resolved from, or stored against, the account's existing record for that domain name) rather than a fresh IdentifierMint::next_secret draw per request, and TargetDomainRecord::verification_details must project that stable per-domain token into both dnsTxt.token and httpRoute.token. The now-resolved issue 20261030T004326Z-securityagent-created-resource-identifiers-and-verification-tokens-are-fixed.md previously covered this same key by making the token unique instead of constant; it is closed and does not cover this step any more, and the replay reports this cluster as unowned. The targetDomainId half of that old cause is likewise closed (20261112T091121Z-securityagent-createsecurityrequirementpack-and-createtargetdomain-mint-opaque-hex-identifiers-instead.md) and the id now passes, so only the token remains.

Traces: securityagent-target-domain-tagging-and-errors#setup#1.

## Evidence
Replay of wire trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 105 steps: 76 passed, 29 mismatched, 0 errored, score 72.4%, 9 divergence clusters). Eight of the nine clusters carry owned_by; this one is the only net-new cluster (stub_path empty, owned_by empty):

Fixture securityagent-target-domain-tagging-and-errors, step setup#1, operation CreateTargetDomain, verdict mismatch, expected status 200 / actual status 200:
  $.verificationDetails.httpRoute.token: value_mismatch (expected '{{http_route_token_2}}', got '9b1e8b7bd29a7f51cbff83e388fc95862994c29746215218a2937d9f4c63dd08')

divergence_key: service=securityagent|CreateTargetDomain|status=200->200|keys=$.verificationDetails.httpRoute.token

Every other member of the same response matches, including $.targetDomainId (bound as its own placeholder and satisfied by the emulator's td-<uuid> value), $.verificationStatus, $.verificationStatusReason, $.verificationDetails.method and $.verificationDetails.httpRoute.routePath — so the divergence is confined to the challenge token.

The quoted hex literal is this run's drawn value and is NOT the requirement: the requirement is that the token this step reports is the token already bound for that domain in the corpus (one stable challenge value per registered domain name), not a per-call draw. Historical corroboration that the token is not merely a per-run value the harness tolerates: the same fixture/step/key was reported when the token was the constant '0778a98ec3c7bfd032e1266778cc3d244a3db4cc51b7638568e2a01afaffd780' (issue 20261030T004326Z, now resolved); the token was made unique per call and the step still mismatches, while the sibling targetDomainId placeholder at the same step now passes.

Model basis (smithy_query, restJson1): CreateTargetDomain -> CreateTargetDomainOutput.verificationDetails -> VerificationDetails.httpRoute -> HttpVerification{token: String, routePath: String}; no @pattern/@length on token, no modeled errors on the operation.

Code citations: services/service-com-amazonaws-securityagent-securityagent/src/ops/create_target_domain.rs:235-247 ('A fresh identity per call ... the domain name is not an alternate key'; state.mint_target_domain_identity()); services/service-com-amazonaws-securityagent-securityagent/src/state.rs:1793-1801 (mint_target_domain_identity -> identifiers.next_secret(VERIFICATION_TOKEN_CONTEXT)); src/state.rs:1556-1567 (next_secret digests the context, namespace, a never-reused counter and SystemTime::now() nanos, so every call yields a different 64-hex value); src/state.rs:1264-1289 (TargetDomainRecord::verification_details serves that value as httpRoute.token / dnsTxt.token).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus, trace set simparity-wire-aws-v1) and inspect fixture securityagent-target-domain-tagging-and-errors, step setup#1 (CreateTargetDomain): $.verificationDetails.httpRoute.token mismatches the corpus-bound {{http_route_token_2}} while every other member of the response matches.
3. Equivalently by hand against the emulator's data listener (restJson1, POST with sigv4):
   a. POST /CreateTargetDomain {"targetDomainName":"probe-one.example.com","verificationMethod":"HTTP_ROUTE"} -> note verificationDetails.httpRoute.token (64 lowercase hex).
   b. POST /CreateTargetDomain with the SAME targetDomainName and verificationMethod again -> observe a completely different httpRoute.token (and, for DNS_TXT, a different dnsTxt.token), i.e. the challenge the owner is told to publish for that domain changed.
4. Expected: the account's registration of a given domain name reports one stable ownership-verification token, so a challenge already published at /.well-known/aws/securityagent-domain-verification.json (or as the TXT record) keeps matching and the value the corpus binds for that domain is reported again. Observed: a freshly drawn per-call secret every time.
5. Re-replay securityagent-target-domain-tagging-and-errors#setup#1 to confirm the fix.
