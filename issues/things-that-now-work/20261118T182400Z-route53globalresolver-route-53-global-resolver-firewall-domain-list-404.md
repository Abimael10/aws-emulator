Issue-Source: trace-sweep-divergence
Issue-Trace: route53globalresolver-firewall-domain-list-not-found
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53globalresolver-firewall-domain-list-not-found
Issue-Shape-Id: com.amazonaws.route53globalresolver#EC2DNSGlobalResolverCustomerAPI
Issue-Key: route53globalresolver|CreateFirewallDomainList|4xx->4xx|$.resourceId
Issue-Key: route53globalresolver|CreateFirewallDomainList|4xx->4xx|$.resourceType
Issue-Key: route53globalresolver|GetFirewallDomainList|4xx->4xx|$.message
Issue-Key: route53globalresolver|GetFirewallDomainList|4xx->4xx|$.resourceId
Issue-Key: route53globalresolver|GetFirewallDomainList|4xx->4xx|$.resourceType
Issue-Commit: 63fcc002cb277cedf69e5b2a0dec0ae5f6263e2a

# Route 53 Global Resolver firewall-domain-list 404 refusal bodies serialize resourceId/resourceType and emulator-authored message

> _Note: this issue was identified against the code base at commit `63fcc002cb277cedf69e5b2a0dec0ae5f6263e2a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFirewallDomainList

## Description
The route53globalresolver firewall-domain-list operations refuse with the right HTTP status and the right error class (404 ResourceNotFoundException) but serialize a body the service does not serve: they populate `resourceId` and `resourceType` members that the recorded AWS 404 body omits, and on the read path the `message` is emulator prose with no `[GR-ERRnnnnn]` code at all. THIS IS TWO CAUSES — close both before submitting.

Cause 1 of 2: CreateFirewallDomainList against a globalResolverId that does not exist — 404 body carries two members AWS does not send.
AWS: Smithy CreateFirewallDomainList, http POST /firewall-domain-lists/{globalResolverId}, @idempotent; globalResolverId is a @required @httpLabel targeting ResourceId (@length 1..64, @pattern ^[-.a-zA-Z0-9]+$); modeled error ResourceNotFoundException @error(client) @httpError(404). Recorded behavior only for the wire shape of that error: although the model declares `resourceId` and a @required `resourceType` on ResourceNotFoundException, the recorded 404 body is message-only — those two members are absent. The message itself is already correct on this step; the divergence is purely the two extra members. The message template that does appear is "[GR-ERR00309] No GlobalResolver found with the ID: <globalResolverId>" (fixed text "[GR-ERR00309] No GlobalResolver found with the ID: ", the single variable part being the globalResolverId taken verbatim from the request path).
Emulator: returns 404 ResourceNotFoundException with the correct message, but additionally serializes resourceId=<the requested global-resolver id> and resourceType="global-resolver" (its internal resource-kind token).
Why: a client that round-trips or golden-compares the error body, or that asserts the SDK-deserialized error shape, sees two populated members real AWS never sends — assertions pass against AWS and fail against the emulator, and code that treats a present `resourceType` as meaningful branches on a value AWS never supplies.
Fix: this service's ResourceNotFoundException serializer / the CreateFirewallDomainList handler's not-found construction — build the refusal message-only and stop populating resourceId/resourceType.
Traces: route53globalresolver-firewall-domain-list-not-found#task#1.

Cause 2 of 2: GetFirewallDomainList against a firewallDomainListId that does not exist — message is emulator prose instead of the GR-ERR00309 template, plus the same two extra members.
AWS: Smithy GetFirewallDomainList, http GET /firewall-domain-lists/{firewallDomainListId}, @readonly; firewallDomainListId is a @required @httpLabel targeting ResourceId (@length 1..64, @pattern ^[-.a-zA-Z0-9]+$); modeled error ResourceNotFoundException @error(client) @httpError(404). Recorded behavior for the message: the body is exactly {"message": "[GR-ERR00309] No FirewallDomainList found with the ID: <firewallDomainListId>"} — fixed text "[GR-ERR00309] No FirewallDomainList found with the ID: " (note the API's spelling of the resource noun, FirewallDomainList, and the GR-ERR00309 code), with the request's firewallDomainListId as the only variable part. Recorded behavior only: no resourceId/resourceType members.
Emulator: 404 and the correct error class, but message "<resource-kind> <id> was not found" built from its internal tokens ("firewall-domain-list <id> was not found") with no GR code, and it also serializes resourceId=<the requested id> and resourceType="firewall-domain-list".
Why: the documented way to tell this refusal apart from the service's other not-found classes is the [GR-ERRnnnnn] code in the message; with no code present a client's handler falls through, and the resource noun it can read out of the message is the emulator's internal token rather than the API's spelling, so the resource kind is not recoverable either. The extra members break body comparisons as in cause 1.
Fix: the GetFirewallDomainList handler's not-found construction (the GR-ERR00309 FirewallDomainList template) and the shared ResourceNotFoundException serializer.
Traces: route53globalresolver-firewall-domain-list-not-found#task#2.

The open issue 20261109T193053Z-route53globalresolver-route-53-global-resolver-refusal-bodies-are-emulator.md describes the same family of defect for the DNS-view and global-resolver operations and carries their replay keys; it does NOT cover these two firewall-domain-list steps, which replay reports as unowned.

## Evidence
Replay of trace set simparity-wire-aws-v1 (8 fixtures, 35 steps, 3 passed, 8 divergence clusters). Both clusters below match on status (404) and error class and diverge only in the response body. Status/class expected 404 / actual 404 in both.

Cause 1 — fixture route53globalresolver-firewall-domain-list-not-found, step task#1, CreateFirewallDomainList, expected 404 / actual 404:
  $.resourceId: type_mismatch (expected None, got 'rslvr-sim2real-probe-missing-814193ff')
  $.resourceType: type_mismatch (expected None, got 'global-resolver')
  ($.message matched the recorded body on this step.)

Cause 2 — fixture route53globalresolver-firewall-domain-list-not-found, step task#2, GetFirewallDomainList, expected 404 / actual 404:
  $.message: value_mismatch (expected '[GR-ERR00309] No FirewallDomainList found with the ID: rslvr-fdl-sim2real-probe-missing-58f33808', got 'firewall-domain-list rslvr-fdl-sim2real-probe-missing-58f33808 was not found')
  $.resourceId: type_mismatch (expected None, got 'rslvr-fdl-sim2real-probe-missing-58f33808')
  $.resourceType: type_mismatch (expected None, got 'firewall-domain-list')

The ids above are one recording's values and only fill the templates; the requirement is the fixed text plus the request-derived substitution described per cause, and the absence of resourceId/resourceType.

Model evidence: smithy_query query=operation operation=CreateFirewallDomainList,GetFirewallDomainList and query=shape shape=ResourceNotFoundException,ResourceId on services/service-com-amazonaws-route53globalresolver-ec2dnsglobalresolvercustomerapi/smithy-model/com-amazonaws-route53globalresolver-ec2dnsglobalresolvercustomerapi.json — CreateFirewallDomainList POST /firewall-domain-lists/{globalResolverId}, GetFirewallDomainList GET /firewall-domain-lists/{firewallDomainListId}, both listing ResourceNotFoundException (@error client, @httpError 404; members message @required, resourceId, resourceType @required).

## Steps to Reproduce
Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1, or drive the steps directly against the route53globalresolver endpoint:

1. Cause 1: POST /firewall-domain-lists/{globalResolverId} with a globalResolverId that was never created (e.g. rslvr-sim2real-probe-missing-814193ff) and a valid body (name, optional description/clientToken). Expect 404 ResourceNotFoundException whose body is {"message":"[GR-ERR00309] No GlobalResolver found with the ID: <that id>"} and NOTHING else — assert that `resourceId` and `resourceType` are absent. Fixture route53globalresolver-firewall-domain-list-not-found step task#1.
2. Cause 2: GET /firewall-domain-lists/{firewallDomainListId} with a firewall-domain-list id that does not exist (e.g. rslvr-fdl-sim2real-probe-missing-58f33808). Expect 404 with body {"message":"[GR-ERR00309] No FirewallDomainList found with the ID: <that id>"} only — the GR-ERR00309 code present, the resource noun spelled FirewallDomainList, and no resourceId/resourceType members. Fixture route53globalresolver-firewall-domain-list-not-found step task#2.

In both cases assert the body members, not just the status and error class: the status and class already match and only the body diverges.
