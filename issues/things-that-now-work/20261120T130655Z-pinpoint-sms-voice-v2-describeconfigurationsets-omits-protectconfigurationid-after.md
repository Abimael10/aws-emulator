Issue-Source: trace-sweep-divergence
Issue-Trace: pinpoint-sms-voice-v2-protect-configuration-association-lifecycle
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: pinpoint-sms-voice-v2-protect-configuration-association-lifecycle
Issue-Shape-Id: com.amazonaws.pinpointsmsvoicev2#PinpointSMSVoiceV2
Issue-Key: pinpoint-sms-voice-v2|DescribeConfigurationSets|2xx->2xx|$.ConfigurationSets[*].ProtectConfigurationId
Issue-Commit: 452f77616b23bfeedd67de10b882316dc811a191

# pinpoint-sms-voice-v2 DescribeConfigurationSets omits ProtectConfigurationId after AssociateProtectConfiguration

> _Note: this issue was identified against the code base at commit `452f77616b23bfeedd67de10b882316dc811a191`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeConfigurationSets

## Description
Cause 1 of 1: the protect-configuration association made by AssociateProtectConfiguration is not projected into the configuration set's description, so DescribeConfigurationSets never reports ProtectConfigurationId.

AWS: Smithy (com.amazonaws.pinpointsmsvoicev2) models ConfigurationSetInformation.ProtectConfigurationId, target ProtectConfigurationIdOrArn, @length min 1 max 256, @pattern ^[A-Za-z0-9_:/-]+$, documented as "The unique identifier for the protect configuration." It is an optional member precisely because it is present only while a protect configuration is associated with the set. AssociateProtectConfiguration takes required ProtectConfigurationId + ConfigurationSetName and returns ConfigurationSetArn, ConfigurationSetName, ProtectConfigurationArn and ProtectConfigurationId (all required), i.e. the service records the association against the named configuration set. Consequently, once AssociateProtectConfiguration has succeeded for a set, every subsequent DescribeConfigurationSets (HTTP 200, DescribeConfigurationSetsResult) entry for that set MUST carry ProtectConfigurationId holding the identifier of the associated protect configuration, and the member must disappear again after DisassociateProtectConfiguration. The value itself is state-derived (the id generated when the protect configuration was created) and is never a fixed literal.

Emulator: in the replayed lifecycle the association call is accepted, but the following DescribeConfigurationSets answers 200 with $.ConfigurationSets[0].ProtectConfigurationId absent (replay detail: "$.ConfigurationSets[0].ProtectConfigurationId: missing (expected '{{protect_configuration_id}}', got None)" — the expectation is the fixture's placeholder for the id minted earlier in the same trace, not a literal). Status and the rest of the structure match; only the association projection is missing. Hypothesis: the emulator stores configuration sets without a protect-configuration link (or stores the link in the association record only) and its ConfigurationSetInformation serializer has no ProtectConfigurationId field to fill.

Why: a client that associates a protect configuration and then reads the configuration set back to confirm or to discover the current association sees an unprotected set. Reconciling tooling (CloudFormation-style drift detection, idempotent provisioners, "is protection enabled?" checks) concludes the association never happened and either re-associates — which on AWS can surface a ConflictException — or reports the account as unprotected. Code that reads ProtectConfigurationId to later disassociate has no id to pass.

Fix: the configuration-set state plus the DescribeConfigurationSets handler/serializer for pinpoint-sms-voice-v2 — AssociateProtectConfiguration must persist the protect-configuration id on the targeted configuration set, DisassociateProtectConfiguration must clear it, and ConfigurationSetInformation must emit ProtectConfigurationId whenever that link is set (and omit it when it is not).

Traces: pinpoint-sms-voice-v2-protect-configuration-association-lifecycle#task#5 (DescribeConfigurationSets, expected 200 / actual 200, missing $.ConfigurationSets[0].ProtectConfigurationId).

Note: the two other divergences in this corpus (CreateConfigurationSet / PutOptedOutNumber validation-error bodies) are already owned by the open issue 20261110T085439Z-pinpoint-sms-voice-v2-error-responses-are-emulator.md and are not part of this issue.

## Evidence
Fixture pinpoint-sms-voice-v2-protect-configuration-association-lifecycle, step task#5, operation DescribeConfigurationSets.
Expected: HTTP 200 with ConfigurationSets[0].ProtectConfigurationId set to the id of the protect configuration associated earlier in the trace (fixture placeholder {{protect_configuration_id}}).
Actual: HTTP 200 with ConfigurationSets[0].ProtectConfigurationId absent (null/omitted).
Replay detail: "$.ConfigurationSets[0].ProtectConfigurationId: missing (expected '{{protect_configuration_id}}', got None)".
Model evidence: ConfigurationSetInformation.ProtectConfigurationId (ProtectConfigurationIdOrArn, length 1-256, pattern ^[A-Za-z0-9_:/-]+$); AssociateProtectConfigurationResult.ProtectConfigurationId required.

## Steps to Reproduce
1. CreateConfigurationSet with a valid ConfigurationSetName.
2. CreateProtectConfiguration and keep the returned ProtectConfigurationId.
3. AssociateProtectConfiguration with that ProtectConfigurationId and the ConfigurationSetName (succeeds).
4. Call DescribeConfigurationSets with ConfigurationSetNames=[that name].
5. Observe the returned ConfigurationSets[0] has no ProtectConfigurationId member; AWS returns it holding the id from step 2. Equivalent: replay fixture pinpoint-sms-voice-v2-protect-configuration-association-lifecycle step task#5.
