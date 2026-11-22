Issue-Source: trace-sweep-divergence
Issue-Trace: dax-error-paths
Issue-Step: task#11
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dax-error-paths
Issue-Shape-Id: com.amazonaws.dax#AmazonDAXV3
Issue-Key: dax|UpdateSubnetGroup|4xx->4xx|$.message
Issue-Commit: da608e42fe102919167610cba86bb047754e5c08

# dax UpdateSubnetGroup returns wrong SubnetGroupNotFoundFault message wording

> _Note: this issue was identified against the code base at commit `da608e42fe102919167610cba86bb047754e5c08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateSubnetGroup

## Description
Cause 1 of 1: DAX UpdateSubnetGroup against a subnet group that does not exist returns the correct HTTP status (400) and the correct error shape, but the error message text does not match AWS.

AWS: Smithy model services/service-com-amazonaws-dax-amazondaxv3/smithy-model/com-amazonaws-dax-amazondaxv3.json, operation UpdateSubnetGroup, error SubnetGroupNotFoundFault (documented: "The requested subnet group name does not refer to an existing subnet group."). Recorded behavior shows DAX answering the lookup failure with HTTP 400 and the message template "Subnet Group <SubnetGroupName> does not exist." — the fixed text is "Subnet Group " + " does not exist." (note the capital 'G' in "Group" and the trailing period), and the single variable part is the SubnetGroupName taken verbatim from the request. In the recording that variable slot happened to be filled with the probe-generated name sim2real-probe-dax-missing-sng-23bbaef0; the name itself is per-run and is not part of the requirement.

Emulator: emits, at the same status 400, the message "Subnet group <SubnetGroupName> not found." — i.e. the fixed text is "Subnet group " + " not found.", lower-casing "group" and substituting "not found." for "does not exist.". Only the $.message member diverges ($.message: value_mismatch); status and error shape agree.

Why: a client relying on AWS's behavior gets the wrong human-facing text and, more importantly, any consumer that matches or asserts on the DAX fault message (error-handling branches, log scrapers, test suites, SDK retry/classification helpers keyed off message text, and golden-file comparisons) fails to recognise the emulator's response as the AWS "subnet group does not exist" condition, even though the status and code are right.

Fix: the error-construction path for the missing-subnet-group lookup in the dax service crate's UpdateSubnetGroup handler (the place that builds SubnetGroupNotFoundFault when the named subnet group is absent from state) must use AWS's message template "Subnet Group {SubnetGroupName} does not exist.", interpolating the request's SubnetGroupName. Any shared subnet-group-lookup helper used by the other DAX subnet-group operations should be corrected in the same place so the wording is consistent.

Traces: dax-error-paths#task#11 (UpdateSubnetGroup, expected 400 / actual 400, divergence key service=dax|UpdateSubnetGroup|status=400->400|keys=$.message).

## Evidence
Fixture dax-error-paths, step task#11, operation UpdateSubnetGroup, service dax.
verdict: mismatch; expected_status: 400; actual_status: 400.
detail: $.message: value_mismatch (expected 'Subnet Group sim2real-probe-dax-missing-sng-23bbaef0 does not exist.', got 'Subnet group sim2real-probe-dax-missing-sng-23bbaef0 not found.').
Expected (AWS) message template: "Subnet Group {SubnetGroupName} does not exist." — fixed text "Subnet Group "/" does not exist.", variable part = request SubnetGroupName.
Actual (emulator) message template: "Subnet group {SubnetGroupName} not found.".
Error shape per Smithy: SubnetGroupNotFoundFault (client error), doc "The requested subnet group name does not refer to an existing subnet group."; recorded wire status for this path is 400.
No other member diverged on this step (keys=$.message only).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), or reproduce by hand:
2. Call dax UpdateSubnetGroup with a SubnetGroupName that has never been created in the emulator's DAX state (e.g. any fresh unique name), optionally with a Description and/or SubnetIds.
3. Observe the response: HTTP 400 with SubnetGroupNotFoundFault.
4. Compare the $.message field. The emulator answers "Subnet group <name> not found." where AWS answers "Subnet Group <name> does not exist.".
5. Exact failing unit: fixture dax-error-paths, step task#11.
