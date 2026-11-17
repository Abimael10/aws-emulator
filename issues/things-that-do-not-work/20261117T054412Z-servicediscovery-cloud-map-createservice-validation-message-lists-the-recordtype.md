Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ecscm-traj-private-dns-srv-reference-path
Issue-Step: task#8
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ecscm-traj-private-dns-srv-reference-path
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: servicediscovery|CreateService|4xx->4xx|$.Message
Issue-Commit: 1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61

# Cloud Map CreateService validation message lists the RecordType enum value set in model declaration order instead of AWS's order

> _Note: this issue was identified against the code base at commit `1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Cause 1 of 1: CreateService (fixture ecs-ecscm-traj-private-dns-srv-reference-path, step task#8).
AWS: a CreateService request whose DnsConfig.DnsRecords[n].Type is not a member of the RecordType enum (here a lower-case 'srv') is refused with HTTP 400 and the standard AWS framework validation message, rendered in the response's $.Message member: "1 validation error detected: Value '<supplied value>' at '<member path>' failed to satisfy constraint: Member must satisfy enum value set: [AAAA, A, CNAME, SRV]". Fixed text: '1 validation error detected: Value ', " at '", "' failed to satisfy constraint: Member must satisfy enum value set: " and the bracketed value set itself; variable parts taken from the request: the rejected value and the member path (e.g. dnsConfig.dnsRecords.1.member.type). The value set AWS renders for RecordType is, in order, [AAAA, A, CNAME, SRV] — recorded behavior only: the Smithy model (com.amazonaws.servicediscovery#RecordType) declares the members in a different order (SRV, A, AAAA, CNAME) and says nothing about the order used in the validation message.
Emulator: HTTP 400 with the correct fixed text and the correct value and member path, but the value set is rendered as '[SRV, A, AAAA, CNAME]' — the model's declaration order, not AWS's. Status and error classification match; only the message diverges.
Why: the validation message is part of the contract for this error class. A client (or test fixture, or log-scraping/assertion harness) that matches AWS's refusal text, or that parses the bracketed set to present the allowed values to a user, does not match or presents them in an order AWS never emits.
Fix: the constraint-violation message formatter used when serializing the framework ValidationException for servicediscovery CreateService — render the RecordType value set in AWS's documented/recorded order ([AAAA, A, CNAME, SRV]) rather than iterating the Smithy enum's declaration order. Note the general rule: the rendered set order is a per-enum property of AWS's message, not the model's member order, so the fix belongs in the enum-set rendering, not in one call site.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout:
- servicediscovery / CreateService, fixture ecs-ecscm-traj-private-dns-srv-reference-path step task#8 (verdict mismatch, divergence_key service=servicediscovery|CreateService|status=400->400|keys=$.Message): expected_status 400, actual_status 400; $.Message value_mismatch — expected "1 validation error detected: Value 'srv' at 'dnsConfig.dnsRecords.1.member.type' failed to satisfy constraint: Member must satisfy enum value set: [AAAA, A, CNAME, SRV]", got "1 validation error detected: Value 'srv' at 'dnsConfig.dnsRecords.1.member.type' failed to satisfy constraint: Member must satisfy enum value set: [SRV, A, AAAA, CNAME]".
Model support: smithy_query(shape=RecordType,DnsRecord) on services/service-com-amazonaws-servicediscovery-route53autonaming-v20170314/smithy-model/com-amazonaws-servicediscovery-route53autonaming-v20170314.json — RecordType values are declared [SRV, A, AAAA, CNAME] (the order the emulator emits) and DnsRecord.Type is @required with target RecordType; AWS's recorded message orders the same four values [AAAA, A, CNAME, SRV].

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Read the servicediscovery divergence cluster for fixture ecs-ecscm-traj-private-dns-srv-reference-path.
3. Call servicediscovery CreateService with DnsConfig.DnsRecords containing a second record whose Type is an invalid value (e.g. lower-case 'srv').
4. Compare the 400 response's $.Message: the emulator renders 'Member must satisfy enum value set: [SRV, A, AAAA, CNAME]' where AWS renders '[AAAA, A, CNAME, SRV]'.
5. Re-replay fixture ecs-ecscm-traj-private-dns-srv-reference-path step task#8 to confirm.
