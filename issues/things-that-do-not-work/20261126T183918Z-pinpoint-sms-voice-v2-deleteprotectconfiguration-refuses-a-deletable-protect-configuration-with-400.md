Issue-Source: trace-sweep-divergence
Issue-Trace: pinpoint-sms-voice-v2-protect-configuration-deletion-protection-and-single-association
Issue-Step: task#13
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: pinpoint-sms-voice-v2-protect-configuration-deletion-protection-and-single-association
Issue-Shape-Id: com.amazonaws.pinpointsmsvoicev2#PinpointSMSVoiceV2
Issue-Key: pinpoint-sms-voice-v2|DeleteProtectConfiguration|2xx->4xx|$.AccountDefault
Issue-Key: pinpoint-sms-voice-v2|DeleteProtectConfiguration|2xx->4xx|$.DeletionProtectionEnabled
Issue-Key: pinpoint-sms-voice-v2|DeleteProtectConfiguration|2xx->4xx|$.ProtectConfigurationArn
Issue-Key: pinpoint-sms-voice-v2|DeleteProtectConfiguration|2xx->4xx|$.ProtectConfigurationId
Issue-Key: pinpoint-sms-voice-v2|DeleteProtectConfiguration|2xx->4xx|status
Issue-Commit: 9b8e7e8b562a73bd2762b3b8c825382a8a072dff

# DeleteProtectConfiguration refuses a deletable protect configuration with 400 instead of removing it and returning 200

> _Note: this issue was identified against the code base at commit `9b8e7e8b562a73bd2762b3b8c825382a8a072dff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteProtectConfiguration

## Description
This issue carries 1 cause, in the pinpoint-sms-voice-v2 (AWS End User Messaging SMS and Voice, sms-voice, awsJson1_0) crate. A fixer must close 1 cause before submitting.

Cause 1 of 1 — a protect configuration whose delete preconditions are all satisfied is still refused with HTTP 400, so it can never be deleted (DeleteProtectConfiguration).

AWS: Smithy (services/service-com-amazonaws-pinpointsmsvoicev2-pinpointsmsvoicev2/smithy-model/com-amazonaws-pinpointsmsvoicev2-pinpointsmsvoicev2.json) documents DeleteProtectConfiguration as "Permanently delete the protect configuration. The protect configuration must have deletion protection disabled and must not be associated as the account default protect configuration or associated with a configuration set." Its input is ProtectConfigurationId (ProtectConfigurationIdOrArn, @required, len 1..256, pattern ^[A-Za-z0-9_:/-]+$); its output members are all @required: ProtectConfigurationArn, ProtectConfigurationId, CreatedTimestamp, AccountDefault (PrimitiveBoolean, default false) and DeletionProtectionEnabled (PrimitiveBoolean, default false). ConflictException (errorKind client, HTTP 400) is modeled ONLY for the three documented blockers above. UpdateProtectConfiguration is modeled with DeletionProtectionEnabled (Boolean) and returns the configuration's post-update DeletionProtectionEnabled, i.e. deletion protection is a mutable property: once it is turned off the delete is allowed. The recording for this step shows HTTP 200 with AccountDefault false, DeletionProtectionEnabled false, and the ProtectConfigurationId/ProtectConfigurationArn of the record that was removed (the recorded run's literal id/arn values are per-run and not part of the requirement), and the fixture's later read of the same id expects ResourceNotFoundException because the record is gone.

Emulator: HTTP 400 for that same delete. None of the modeled output members is present — $.AccountDefault, $.DeletionProtectionEnabled, $.ProtectConfigurationArn and $.ProtectConfigurationId are all absent (None) — and the record is NOT removed from the store, which is why the fixture's subsequent DescribeProtectConfigurations on the same id still answers 200 with the configuration alive.

Hypothesis: the delete is refused by its own precondition arm (services/service-com-amazonaws-pinpointsmsvoicev2-pinpointsmsvoicev2/src/ops/delete_protect_configuration.rs:203-234 computes a `blocker` of DELETION_PROTECTION_ENABLED / PROTECT_CONFIGURATION_IS_ACCOUNT_DEFAULT / PROTECT_CONFIGURATION_ASSOCIATED_WITH_CONFIGURATION_SET and returns ConflictException, HTTP 400) on a blocker the emulator's state still holds but AWS no longer did. The only blocker reachable here is deletion protection: it is written once at creation (src/ops/create_protect_configuration.rs:189 and :258 persist the request's DeletionProtectionEnabled) and NOTHING in the crate can clear it afterwards, because UpdateProtectConfiguration — the one modeled operation that sets DeletionProtectionEnabled — is an unimplemented scaffold stub returning 501 (src/ops/update_protect_configuration.rs:174). SetAccountDefaultProtectConfiguration is likewise a stub (src/ops/set_account_default_protect_configuration.rs:169) so account_default is always false, and the association blocker is correctly cleared by the implemented AssociateProtectConfiguration / DisassociateProtectConfiguration / DeleteConfigurationSet handlers, so neither of those can be the stale blocker. Net effect: a protect configuration created with deletion protection enabled is undeletable for the lifetime of the emulator.

Why: a client that follows AWS's documented teardown — disable deletion protection, then delete — gets a permanent 400 ConflictException and can never free the resource. Test suites and CloudFormation-style teardown loops that create a protect configuration with DeletionProtectionEnabled=true (a very common hardening default) leak it forever, eventually tripping the per-account protect-configuration quota the create path enforces, and the caller also never receives the deleted record's final AccountDefault/DeletionProtectionEnabled/Arn/Id, which the modeled output declares @required.

Fix: DeleteProtectConfiguration's precondition evaluation and the protect-configuration state behind it — the blocker set must reflect the configuration's CURRENT state, which requires UpdateProtectConfiguration to persist DeletionProtectionEnabled onto the stored record so the delete stops refusing a configuration whose protection has been turned off; when no blocker holds, the record must be removed and the removed record returned with HTTP 200 and all five modeled output members.

Related: the open issue 20261120T235619Z-pinpoint-sms-voice-v2-deleted-and-disabled-resources.md owns the DescribeProtectConfigurations-after-delete cluster (it carries that replay key), but it attributes the stale record to DeleteProtectConfiguration being a scaffold stub returning 501. At this commit the delete IS implemented and answers 400, so this refusal — not a stub — is what now keeps the record alive; that issue's cause 1 cannot be closed without fixing this one.

Traces: pinpoint-sms-voice-v2-protect-configuration-deletion-protection-and-single-association#task#13.

## Evidence
Cause 1 — fixture pinpoint-sms-voice-v2-protect-configuration-deletion-protection-and-single-association, step task#13, operation DeleteProtectConfiguration.
Expected (recorded AWS): HTTP 200 with $.AccountDefault false, $.DeletionProtectionEnabled false, $.ProtectConfigurationId the id of the removed configuration and $.ProtectConfigurationArn its arn (recorded run values: protect-configuration-00000000000000000000000000000001 and arn:aws:sms-voice:us-west-2:000000000000:protect-configuration/protect-configuration-00000000000000000000000000000001 — per-run values, not requirements), plus the @required CreatedTimestamp.
Actual (emulator): HTTP 400; $.AccountDefault, $.DeletionProtectionEnabled, $.ProtectConfigurationArn and $.ProtectConfigurationId all missing (None). Replay divergence_key service=pinpoint-sms-voice-v2|DeleteProtectConfiguration|status=200->400|keys=$.AccountDefault,$.DeletionProtectionEnabled,$.ProtectConfigurationArn,$.ProtectConfigurationId,status.
Downstream in the same fixture: task#16 DescribeProtectConfigurations on that id answers 200 with the configuration still listed instead of the modeled 400 ResourceNotFoundException, because the delete removed nothing (that cluster is owned by 20261120T235619Z-pinpoint-sms-voice-v2-deleted-and-disabled-resources.md).
Code: src/ops/delete_protect_configuration.rs:203-234 (the ConflictException blocker arm), src/ops/create_protect_configuration.rs:189 and :258 (DeletionProtectionEnabled persisted at create), src/ops/update_protect_configuration.rs:174 (501 scaffold stub — the only modeled writer of DeletionProtectionEnabled), src/ops/set_account_default_protect_configuration.rs:169 (501 stub, so account_default is never true), src/ops/associate_protect_configuration.rs:226 and src/ops/disassociate_protect_configuration.rs:247 (the association blocker is set and cleared correctly). All paths relative to services/service-com-amazonaws-pinpointsmsvoicev2-pinpointsmsvoicev2/.

## Steps to Reproduce
Boot the emulator and point an sms-voice (pinpoint-sms-voice-v2, awsJson1_0) client at it.

1. create_protect_configuration(DeletionProtectionEnabled=true) -> note ProtectConfigurationId.
2. (optional, mirrors the fixture) create_configuration_set(ConfigurationSetName='setA'); associate_protect_configuration(ProtectConfigurationId=<id>, ConfigurationSetName='setA'); disassociate_protect_configuration(ProtectConfigurationId=<id>, ConfigurationSetName='setA').
3. update_protect_configuration(ProtectConfigurationId=<id>, DeletionProtectionEnabled=false) — AWS returns 200 with DeletionProtectionEnabled false; the emulator answers 501 (unimplemented scaffold stub), so the stored flag stays true.
4. delete_protect_configuration(ProtectConfigurationId=<id>). Observe HTTP 400 with no AccountDefault / DeletionProtectionEnabled / ProtectConfigurationArn / ProtectConfigurationId members, instead of HTTP 200 returning the removed record with DeletionProtectionEnabled false and AccountDefault false.
5. describe_protect_configurations(ProtectConfigurationIds=[<id>]) still answers 200 with the configuration alive, confirming nothing was removed.

Re-replay: pinpoint-sms-voice-v2-protect-configuration-deletion-protection-and-single-association#task#13.
