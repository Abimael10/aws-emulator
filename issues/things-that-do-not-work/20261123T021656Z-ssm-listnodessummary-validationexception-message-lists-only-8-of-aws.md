Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.list-nodes-summary-invalid-aggregator-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.list-nodes-summary-invalid-aggregator-rejected|run#15|AssertionError|assert malformed.get("Message") == expected_malformed_message, (
Issue-Commit: 42819415083a81c3cc8c8415cafe0862fc134a09

# ListNodesSummary ValidationException message lists only 8 of AWS's 11 NodeAttributeName enum values

> _Note: this issue was identified against the code base at commit `42819415083a81c3cc8c8415cafe0862fc134a09`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListNodesSummary

## Description
Cause 1 of 1: the enum-value set named in the ValidationException for an out-of-enum NodeAggregator.AttributeName is short by three members and in a different order than AWS's.

AWS: ListNodesSummary takes NodeAggregator.AttributeName as a required closed enum (Smithy: NodeAggregator.AttributeName -> NodeAttributeName, required; ListNodesSummary models InvalidAggregatorException/InvalidFilter/InvalidNextToken/ResourceDataSyncNotFoundException/UnsupportedOperationException, and the out-of-enum refusal is the protocol's own ValidationException). The recorded AWS refusal is HTTP 400 with error code ValidationException and the message template "1 validation error detected: Value '<supplied value>' at '<member path>' failed to satisfy constraint: Member must satisfy enum value set: [PlatformVersion, AvailabilityZone, ResourceType, PlatformType, ExtensionStatus, AgentVersion, Region, SourceType, ExtensionVersion, ExtensionId, PlatformName]". Fixed text: everything except the two variable slots; the variable parts taken from the request are the rejected value and the member path (for a single aggregator, 'aggregators.1.member.attributeName'). The bracketed value set is NOT a per-run value: it is AWS's current enum for this member — 11 values, in exactly that order. Recorded behavior only for the three Extension* members: the Smithy model shipped in this checkout (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json, shape NodeAttributeName) declares only 8 values (AgentVersion, PlatformName, PlatformType, PlatformVersion, Region, ResourceType, SourceType, AvailabilityZone) and so predates ExtensionStatus, ExtensionVersion and ExtensionId, which AWS now reports in this message. The same recording shows the sibling assertions already match: a well-formed aggregator is refused UnsupportedOperationException with the account-enablement message, so only the value set in the validation message diverges.

Emulator: answers HTTP 400 ValidationException with "1 validation error detected: Value 'TotallyBogusAttribute' at 'aggregators.1.member.attributeName' failed to satisfy constraint: Member must satisfy enum value set: [PlatformVersion, AvailabilityZone, ResourceType, PlatformType, AgentVersion, Region, SourceType, PlatformName]" — the status, the code, the fixed text and both variable slots are right, but the value set is the model's 8 members instead of AWS's 11, with ExtensionStatus, ExtensionVersion and ExtensionId absent and the ordering therefore different. Because the refusal is produced by the centralized request walk from that model enum (list_nodes_summary.rs documents: "A member outside the closed enum the model declares for it is refused by the centralized request walk before this handler runs"), the emulator will also REFUSE AttributeName=ExtensionStatus / ExtensionVersion / ExtensionId, which AWS accepts.

Why: a client that parses this refusal to learn which node attributes it may aggregate on is told three attributes do not exist, and a client that legitimately aggregates by ExtensionStatus, ExtensionVersion or ExtensionId is refused ValidationException for a request AWS accepts. Any assertion on the refusal message (as AWS words it) fails against the emulator.

Fix: the enum value set and message used for NodeAttributeName in the centralized request-walk enum validation for this service (so the message lists AWS's 11 members in AWS's order), together with accepting the three Extension* values as AttributeName in the ListNodesSummary handler's aggregation path.

Traces: ssm.list-nodes-summary-invalid-aggregator-rejected (the malformed-AttributeName assertion; the UnsupportedOperationException leg of the same check already passes). Code citations: services/service-com-amazonaws-ssm-amazonssm/src/ops/list_nodes_summary.rs lines 19-23 and 219-226; smithy-model shape NodeAttributeName.

## Evidence
Expected (AWS, recorded): HTTP 400, Code=ValidationException, Message="1 validation error detected: Value 'TotallyBogusAttribute' at 'aggregators.1.member.attributeName' failed to satisfy constraint: Member must satisfy enum value set: [PlatformVersion, AvailabilityZone, ResourceType, PlatformType, ExtensionStatus, AgentVersion, Region, SourceType, ExtensionVersion, ExtensionId, PlatformName]"

Actual (emulator): HTTP 400, Code=ValidationException, Message="1 validation error detected: Value 'TotallyBogusAttribute' at 'aggregators.1.member.attributeName' failed to satisfy constraint: Member must satisfy enum value set: [PlatformVersion, AvailabilityZone, ResourceType, PlatformType, AgentVersion, Region, SourceType, PlatformName]"

Diff: ExtensionStatus, ExtensionVersion and ExtensionId are missing and the member order differs. Replay assertion: check_ssm_list_nodes_summary_invalid_aggregator_rejected.py line 65 (`assert malformed.get("Message") == expected_malformed_message`).

On-disk model evidence: smithy_query(shape="NodeAttributeName") -> values [AgentVersion, PlatformName, PlatformType, PlatformVersion, Region, ResourceType, SourceType, AvailabilityZone] — 8 values, i.e. exactly what the emulator emits, confirming the message is derived from a model enum that predates AWS's Extension* members.

## Steps to Reproduce
1. Boot the emulator and call ssm:DescribeInstanceInformation once (positive control; it answers).
2. Call ssm:ListNodesSummary with Aggregators=[{AggregatorType: "Count", TypeName: "Instance", AttributeName: "TotallyBogusAttribute"}].
3. Inspect the ClientError: code is ValidationException (correct) but the "enum value set: [...]" list in the message omits ExtensionStatus, ExtensionVersion and ExtensionId and is in a different order than AWS's.
4. Also call ListNodesSummary with AttributeName="ExtensionStatus" (a value AWS accepts): the emulator refuses it with the same ValidationException instead of proceeding to the account-enablement gate.
