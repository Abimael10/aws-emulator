Issue-Source: trace-sweep-divergence
Issue-Trace: route53-key-signing-key-guardrails
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-key-signing-key-guardrails
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|DeactivateKeySigningKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: route53|DeleteKeySigningKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 332deeb05333b69e850545a66f3071a5605eafe8

# Route 53 key-signing-key paths on an unknown hosted zone report a KSK-not-found message instead of AWS's hosted-zone-not-found message

> _Note: this issue was identified against the code base at commit `332deeb05333b69e850545a66f3071a5605eafe8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeactivateKeySigningKey

## Description
TWO causes in this bundle: both key-signing-key operations addressed at a hosted zone that does not exist refuse with the wrong Error.Message — they explain the failure as a missing KSK instead of a missing hosted zone. A fixer must close both before submitting.

Cause 1 of 2 — DeactivateKeySigningKey on a non-existent hosted zone (fixture route53-key-signing-key-guardrails, step task#3).
AWS: Smithy: DeactivateKeySigningKey, POST /2013-04-01/keysigningkey/{HostedZoneId}/{Name}/deactivate, input HostedZoneId (ResourceId, @httpLabel, required) and Name (SigningKeyName, @httpLabel, required); modeled errors include NoSuchKeySigningKey (client, 404, 'The specified key-signing key (KSK) doesn't exist') and InvalidInput/InvalidKeySigningKeyStatus/ConcurrentModification (400). The route53 model also carries NoSuchHostedZone (client, 404, 'No hosted zone exists with the ID that you specified'). Recorded behavior: the request is refused 404 with Error.Message 'No hosted zone found with ID: <HostedZoneId>' — i.e. AWS resolves the path's hosted zone FIRST and reports the zone as the thing that is missing, never the KSK, when the zone itself does not exist. Message template: fixed text 'No hosted zone found with ID: ' followed by the HostedZoneId taken verbatim from the request path (the recording's Z00000000000000NOTREAL is that request's value, not part of the template).
Emulator: status 404 (matching), but Error.Message is "The Key Signing Key with name 'sim2real_probe_ksk', could not be found." — a KSK-not-found explanation naming the request's Name instead of the hosted-zone-not-found explanation naming its HostedZoneId (replay: '$.ErrorResponse.Error.Message: value_mismatch').
Why: a client cannot tell which input it got wrong. Error-handling that branches on the refusal (re-create the zone vs. create the KSK, or surface 'unknown hosted zone' to the operator) takes the wrong branch, and a DNSSEC bootstrap retry loop keeps re-attempting KSK creation against a zone that will never exist. Message text is part of the contract for this error class.
Fix: the DeactivateKeySigningKey handler's validation order — resolve and validate HostedZoneId before looking up the KSK, and emit the hosted-zone-not-found refusal (NoSuchHostedZone, 404, 'No hosted zone found with ID: <HostedZoneId>') when the zone is absent; only fall through to the KSK-not-found refusal when the zone exists.
Traces: route53-key-signing-key-guardrails#task#3.

Cause 2 of 2 — DeleteKeySigningKey on a non-existent hosted zone (fixture route53-key-signing-key-guardrails, step task#4).
AWS: Smithy: DeleteKeySigningKey, DELETE /2013-04-01/keysigningkey/{HostedZoneId}/{Name}, same required @httpLabel inputs, modeled errors NoSuchKeySigningKey (404), InvalidInput/InvalidKeySigningKeyStatus/InvalidKMSArn/InvalidSigningStatus/ConcurrentModification (400); NoSuchHostedZone (404) is the zone-level class. Recorded behavior: 404 with Error.Message 'No hosted zone found with ID: <HostedZoneId>', same fixed text plus the request's HostedZoneId.
Emulator: status 404 (matching) with Error.Message "The Key Signing Key with name '<Name>', could not be found." (recorded run: 'sim2real_probe_ksk').
Why: as Cause 1 — the refusal misattributes the missing resource, so cleanup/teardown code that treats 'KSK not found' as benign (already deleted) silently swallows the fact that the hosted zone id it is operating on is wrong.
Fix: as Cause 1, in the DeleteKeySigningKey handler's validation order.
Traces: route53-key-signing-key-guardrails#task#4.

Related open issues that do NOT carry these steps' replay keys (named for the fixer, they do not cover these causes): 20261113T162104Z-route53-route-53-dnssec-ksk-activation-deactivation-state-machine.md and 20261113T100545Z-route53-deactivatekeysigningkey-stub.md — those describe 501 NotImplemented answers; here both operations DO answer 404 and diverge only in the refusal's message text.

## Evidence
Cause 1 — route53-key-signing-key-guardrails#task#3, DeactivateKeySigningKey: expected status 404; actual 404 (match). Expected $.ErrorResponse.Error.Message = 'No hosted zone found with ID: Z00000000000000NOTREAL' (fixed text 'No hosted zone found with ID: ' + the HostedZoneId from the request path); actual "The Key Signing Key with name 'sim2real_probe_ksk', could not be found." (value_mismatch).
Cause 2 — route53-key-signing-key-guardrails#task#4, DeleteKeySigningKey: expected status 404; actual 404 (match). Expected $.ErrorResponse.Error.Message = 'No hosted zone found with ID: Z00000000000000NOTREAL'; actual "The Key Signing Key with name 'sim2real_probe_ksk', could not be found." (value_mismatch).
Only Error.Message diverged in both clusters — the status and the rest of the ErrorResponse envelope matched, so the defect is the refusal's attribution/wording, not its transport.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay fixture route53-key-signing-key-guardrails to step task#3 — POST /2013-04-01/keysigningkey/<hosted-zone-id-that-does-not-exist>/<ksk-name>/deactivate. Observe 404 whose Error.Message names the KSK; AWS's message names the hosted zone: 'No hosted zone found with ID: <HostedZoneId>'.
3. Cause 2: continue to step task#4 — DELETE /2013-04-01/keysigningkey/<same-nonexistent-hosted-zone-id>/<ksk-name>. Same wrong message.
4. Repeat with an EXISTING hosted zone and an unknown KSK name to confirm the KSK-not-found message is only correct in that case.
