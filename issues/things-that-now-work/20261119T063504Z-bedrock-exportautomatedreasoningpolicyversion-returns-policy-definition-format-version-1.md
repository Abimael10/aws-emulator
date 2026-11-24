Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-automated-reasoning-policy-definition-round-trip
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-automated-reasoning-policy-definition-round-trip
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|ExportAutomatedReasoningPolicyVersion|2xx->2xx|$.version
Issue-Commit: b836bb95981ef6ff5f4d5eb5d082d96ccdb6a9db

# bedrock ExportAutomatedReasoningPolicyVersion returns policy-definition format version "1.0" instead of "1"

> _Note: this issue was identified against the code base at commit `b836bb95981ef6ff5f4d5eb5d082d96ccdb6a9db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExportAutomatedReasoningPolicyVersion

## Description
Single cause (1 of 1): the exported Automated Reasoning policy definition carries the wrong literal in its `version` (policy-definition format version) member.

Cause 1 of 1: ExportAutomatedReasoningPolicyVersion serializes AutomatedReasoningPolicyDefinition.version as "1.0".

AWS: ExportAutomatedReasoningPolicyVersion (GET /automated-reasoning-policies/{policyArn}/export, HTTP 200) returns the AutomatedReasoningPolicyDefinition as the httpPayload. Smithy: AutomatedReasoningPolicyDefinition$version targets AutomatedReasoningPolicyFormatVersion (string) and is modeled with @default "1", documented as "The version of the policy definition format." The recorded AWS response for this fixture carries `"version": "1"` at the top level of the exported definition payload, i.e. the bare major format version with no minor component. The value is a fixed format-version literal, not a per-request or per-resource value: it does not depend on the policy ARN, the policy's own resource version, the account, the region, or the time of the call, so "1" is the requirement for every export of a format-1 definition.

Emulator: the same step returns HTTP 200 with `"version": "1.0"` in the exported definition payload. Replay reports `$.version: value_mismatch (expected '1', got '1.0')` and no other difference on this step — every other member of the exported definition (types, rules, variables) matched, so the defect is isolated to the format-version literal.

Why: a client that reads the definition's format version to decide how to parse or re-submit the document sees an unknown format ("1.0") where AWS always serves "1", and branches to an error or an unsupported-format path. The export/import round trip is the primary use of this payload: a definition exported from the emulator and handed back to a tool (or to CreateAutomatedReasoningPolicy / UpdateAutomatedReasoningPolicy) that validates the format version against the modeled default "1" is rejected, even though the rules and variables are correct. Any snapshot or golden-file comparison of an exported definition against an AWS-produced one also fails on this one field.

Fix: the ExportAutomatedReasoningPolicyVersion response serializer / the Automated Reasoning policy-definition model in the bedrock control-plane crate — wherever the policy definition's format-version member is populated. It should emit the modeled default for AutomatedReasoningPolicyDefinition$version ("1") rather than a dotted major.minor string, and the same constant must be used wherever a policy definition is serialized (GetAutomatedReasoningPolicy / Create / Update round trips) so exported and stored definitions agree.

Traces: bedrock-automated-reasoning-policy-definition-round-trip#task#2 (ExportAutomatedReasoningPolicyVersion).

Note: no currently open issue claims this step — the replay report lists this cluster with an empty `owned_by`. The open issue 20261029T172312Z-bedrock-control-plane-is-over-permissive-enforced-guardrail.md concerns over-permissive bedrock control-plane refusal paths and does not cover the exported definition's format-version literal.

## Evidence
Fixture bedrock-automated-reasoning-policy-definition-round-trip, step task#2, operation ExportAutomatedReasoningPolicyVersion.

Expected (recorded AWS): HTTP 200, exported AutomatedReasoningPolicyDefinition payload with `"version": "1"`.
Actual (emulator):      HTTP 200, exported AutomatedReasoningPolicyDefinition payload with `"version": "1.0"`.

Replay detail, verbatim:
  $.version: value_mismatch (expected '1', got '1.0')

Model evidence: Smithy com.amazonaws.bedrock#AutomatedReasoningPolicyDefinition member `version` -> AutomatedReasoningPolicyFormatVersion (string), default "1", doc "The version of the policy definition format."; ExportAutomatedReasoningPolicyVersion output member `policyDefinition` is @required and @httpPayload, http 200 GET /automated-reasoning-policies/{policyArn}/export.

Fixed vs variable parts: the string "1" is the fixed format-version literal for this definition format (no variable part); the policy ARN in the request URI and the definition's types/rules/variables are the request- and state-derived parts, and those already match.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Replay fixture bedrock-automated-reasoning-policy-definition-round-trip up to step task#2: create an Automated Reasoning policy, publish a version, then call GET /automated-reasoning-policies/{policyArn}/export (ExportAutomatedReasoningPolicyVersion) on the published policy version ARN.
3. Inspect the 200 response body (the AutomatedReasoningPolicyDefinition httpPayload) and read its top-level `version` member.
4. Observed: "1.0". Expected (AWS / Smithy default): "1".
