Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: guardduty.update-detector-features-partial-change-rejects-unknown-additional-configuration
Issue-Shape-Id: com.amazonaws.guardduty#GuardDutyAPIService
Issue-Key: guardduty.update-detector-features-partial-change-rejects-unknown-additional-configuration|run#14|AssertionError|assert rm_addl == {
Issue-Commit: 43fae52e1bb177d3b093606628eac3dfc23ef1a6

# Partial UpdateDetector replaces a feature's whole AdditionalConfiguration list, dropping agent-management settings the call never named

> _Note: this issue was identified against the code base at commit `43fae52e1bb177d3b093606628eac3dfc23ef1a6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateDetector

## Description
Cause 1 of 1: UpdateDetector treats a feature's AdditionalConfiguration list as a full replacement of the feature's additional-configuration state instead of a partial update of only the named sub-configurations, so every sub-configuration the request omits is dropped from the detector.

AWS: UpdateDetector (POST /detector/{DetectorId}, 200, errors BadRequestException/InternalServerErrorException) takes Features: list<DetectorFeatureConfiguration>, whose AdditionalConfiguration is a list<DetectorAdditionalConfiguration>{Name: FeatureAdditionalConfiguration, Status: FeatureStatus}. The Smithy model states only the shapes ("Additional configuration for a resource."), not the update semantics, so this is recorded behavior only: on real AWS, naming a single additional configuration under RUNTIME_MONITORING (e.g. EKS_ADDON_MANAGEMENT=ENABLED) changes exactly that entry and leaves the detector's other agent-management entries (ECS_FARGATE_AGENT_MANAGEMENT, EC2_AGENT_MANAGEMENT) at the values a previous CreateDetector/UpdateDetector established. A subsequent GetDetector therefore still reports all three entries, each with its own status, and the feature's top-level Status is unchanged.

Emulator: after CreateDetector stored RUNTIME_MONITORING with AdditionalConfiguration = {ECS_FARGATE_AGENT_MANAGEMENT: ENABLED, EC2_AGENT_MANAGEMENT: ENABLED, EKS_ADDON_MANAGEMENT: DISABLED} (GetDetector reads that back correctly — the create-side read-back assertion passed), a partial UpdateDetector naming only AdditionalConfiguration=[{EKS_ADDON_MANAGEMENT, ENABLED}] makes GetDetector report RUNTIME_MONITORING with AdditionalConfiguration containing a single entry, {EKS_ADDON_MANAGEMENT: ENABLED}. The two unnamed entries are gone entirely rather than retained; the call returns 200 with no error, so nothing signals the loss.

Why: a client that enables or disables one agent-management sub-setting silently turns off the detector's other agent-management sub-settings. Read-modify-write clients and infrastructure tools that send only the delta (the shape AWS supports) end up with a detector whose configuration they never requested, and a later read-back of the detector reports a configuration set that does not include settings the account still believes are enabled. Drift detection comparing GetDetector against desired state reports spurious removals.

Fix: the UpdateDetector handler's feature-state update in the guardduty service crate — the per-feature AdditionalConfiguration list must be merged entry-by-entry by Name into the stored detector's existing additional-configuration map (upserting named entries, leaving unnamed ones untouched) instead of overwriting the stored list with the request's list.

Traces: guardduty.update-detector-features-partial-change-rejects-unknown-additional-configuration (assertion after the partial update: expected {ECS_FARGATE_AGENT_MANAGEMENT: ENABLED, EC2_AGENT_MANAGEMENT: ENABLED, EKS_ADDON_MANAGEMENT: ENABLED}, got {'EKS_ADDON_MANAGEMENT': 'ENABLED'}).

## Evidence
Check guardduty.update-detector-features-partial-change-rejects-unknown-additional-configuration, AssertionError at the post-update read-back:

  expected rm_addl == {'ECS_FARGATE_AGENT_MANAGEMENT': 'ENABLED', 'EC2_AGENT_MANAGEMENT': 'ENABLED', 'EKS_ADDON_MANAGEMENT': 'ENABLED'}
  got      {'EKS_ADDON_MANAGEMENT': 'ENABLED'}

Sequence: CreateDetector(Enable=true, Features=[RUNTIME_MONITORING ENABLED with AdditionalConfiguration ECS_FARGATE_AGENT_MANAGEMENT=ENABLED, EC2_AGENT_MANAGEMENT=ENABLED, EKS_ADDON_MANAGEMENT=DISABLED; S3_DATA_EVENTS ENABLED]) -> GetDetector returns all three additional configurations (passes) -> UpdateDetector(Features=[RUNTIME_MONITORING ENABLED, AdditionalConfiguration=[EKS_ADDON_MANAGEMENT=ENABLED]; S3_DATA_EVENTS DISABLED]) returns 200 -> GetDetector returns RUNTIME_MONITORING with only one additional configuration entry. The two entries not named in the update request were removed instead of preserved.

## Steps to Reproduce
1. gd.create_detector(Enable=True, Features=[{'Name':'RUNTIME_MONITORING','Status':'ENABLED','AdditionalConfiguration':[{'Name':'ECS_FARGATE_AGENT_MANAGEMENT','Status':'ENABLED'},{'Name':'EC2_AGENT_MANAGEMENT','Status':'ENABLED'},{'Name':'EKS_ADDON_MANAGEMENT','Status':'DISABLED'}]},{'Name':'S3_DATA_EVENTS','Status':'ENABLED'}])
2. gd.get_detector(DetectorId=...) — RUNTIME_MONITORING reports all three AdditionalConfiguration entries.
3. gd.update_detector(DetectorId=..., Features=[{'Name':'RUNTIME_MONITORING','Status':'ENABLED','AdditionalConfiguration':[{'Name':'EKS_ADDON_MANAGEMENT','Status':'ENABLED'}]}])
4. gd.get_detector(DetectorId=...) and inspect the RUNTIME_MONITORING feature's AdditionalConfiguration: the emulator reports only {EKS_ADDON_MANAGEMENT: ENABLED}; AWS reports ECS_FARGATE_AGENT_MANAGEMENT=ENABLED, EC2_AGENT_MANAGEMENT=ENABLED and EKS_ADDON_MANAGEMENT=ENABLED.
