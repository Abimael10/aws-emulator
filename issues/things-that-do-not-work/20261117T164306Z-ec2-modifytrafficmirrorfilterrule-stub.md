Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-traffic-mirror-filter-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-traffic-mirror-filter-lifecycle
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|stub:ModifyTrafficMirrorFilterRule
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# Implement ec2 ModifyTrafficMirrorFilterRule: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyTrafficMirrorFilterRule

## Description
`ModifyTrafficMirrorFilterRule` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-ec2-amazonec2/src/ops/modify_traffic_mirror_filter_rule.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `ModifyTrafficMirrorFilterRule` call, so each one unblocks only when `ModifyTrafficMirrorFilterRule` works: `ec2-traffic-mirror-filter-lifecycle#task#4`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-ec2-amazonec2/src/ops/modify_traffic_mirror_filter_rule.rs`. Replaying the corpus, the emulator refused the `ModifyTrafficMirrorFilterRule` call: status: expected 200, got 501
$.ModifyTrafficMirrorFilterRuleResponse: missing (expected {'requestId': '<shaped-not-asserted>', 'trafficMirrorFilterRule': {'description': 'probe accept rule', 'destinationCidrBlock': '10.2.0.0/16', 'protocol': '6', 'ruleAction': 'reject', 'ruleNumber': '100', 'sourceCidrBlock': '10.1.0.0/16', 'trafficDirection': 'ingress', 'trafficMirrorFilterId': 'tmf-0000000000000008d', 'trafficMirrorFilterRuleId': 'tmfr-0000000000000008e'}}, got None)

## Steps to Reproduce
Boot the ec2 emulator
Call ModifyTrafficMirrorFilterRule and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
