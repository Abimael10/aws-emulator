Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-signals__cloudformation.deploy-application-signals-instrumentation-config-and-slo-round-trip-and-delete
Issue-Key: application-signals__cloudformation.deploy-application-signals-instrumentation-config-and-slo-round-trip-and-delete|run#18|AssertionError|assert got == want, f"instrumentation configurations {got!r}, expected {want!r}"
Issue-Commit: ad5ad51aec6de9d772e821238ebdccadf4a703da

# Instrumentation configuration read-back invents values for unset CodeCapture/CaptureLimits members

> _Note: this issue was identified against the code base at commit `ad5ad51aec6de9d772e821238ebdccadf4a703da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListInstrumentationConfigurations

## Description
Cause 1 of 1: an instrumentation configuration created with a partially-specified CaptureConfiguration is read back with every omitted optional member materialized to a server-side default value, so the stored/returned configuration is not the one the caller supplied.

Failing check ids: application-signals__cloudformation.deploy-application-signals-instrumentation-config-and-slo-round-trip-and-delete

AWS: Application Signals stores and returns the CaptureConfiguration union exactly as supplied. Smithy (com.amazonaws.applicationsignals): CaptureConfiguration is a union whose only member is CodeCapture -> CodeCaptureConfiguration; CodeCaptureConfiguration has members CaptureArguments (list, @length max 10), CaptureReturn (Boolean, optional), CaptureStackTrace (Boolean, optional), CaptureLocals (list, @length max 20), and CaptureLimits (CaptureLimitsConfig, @required). CaptureLimitsConfig members MaxHits (@range 1..1000), MaxStringLength (1..255), MaxCollectionWidth (1..20), MaxCollectionDepth (1..5), MaxStackFrames (1..20), MaxStackTraceSize (1..1000), MaxObjectDepth (1..5), MaxFieldsPerObject (1..20) are ALL optional and NONE of them carries a modeled `@default` trait — the documented defaults ("Defaults to false" for CaptureReturn, "Defaults to true" for CaptureStackTrace, "Defaults to 128" for MaxStringLength, etc.) describe the agent-side capture behavior when a member is absent, they are not values the control plane writes into the resource. Compare MaxResults on ListInstrumentationConfigurations, which DOES carry a modeled default of 50. Recorded behavior confirms this: after a CloudFormation AWS::ApplicationSignals::InstrumentationConfig whose Properties.CaptureConfiguration is {"CodeCapture": {"CaptureLimits": {"MaxHits": 10}}}, ListInstrumentationConfigurations returns that configuration with CaptureConfiguration == {"CodeCapture": {"CaptureLimits": {"MaxHits": 10}}} — only the members that were set, nothing else, with the 200 status the model declares.

Emulator: the same read returns CaptureConfiguration == {"CodeCapture": {"CaptureReturn": false, "CaptureStackTrace": true, "CaptureLimits": {"MaxHits": 10, "MaxStringLength": 128, "MaxCollectionWidth": 10, "MaxCollectionDepth": 3, "MaxStackFrames": 2, "MaxStackTraceSize": 1000, "MaxObjectDepth": 3, "MaxFieldsPerObject": 10}}}. Every member the caller left unset is present with the value the member's prose documentation names as its default; only MaxHits reflects the request. The ARN, LocationHash, SignalType and Location members all match, so the divergence is confined to CaptureConfiguration: unset optional members are being filled in rather than left absent.

Why: a client that round-trips a configuration cannot tell a value it chose from one the service invented. Drift detection and idempotent re-apply break — CloudFormation and Terraform-style diffing of the templated CaptureConfiguration against the read-back value reports a permanent spurious change on eight members and will try to "correct" it forever. A client that reads a configuration, edits one field and writes it back silently pins all eight defaults as explicit caller-chosen values, so a later change to the service's default behavior no longer applies to that resource. Equality assertions on the configuration a caller just created fail against the emulator while passing against AWS.

Fix: the instrumentation-configuration state/serializer for Application Signals — whatever hydrates a stored instrumentation configuration into InstrumentationConfigurationWithoutServiceEnv (and the identical CaptureConfiguration shape returned by GetInstrumentationConfiguration and written by the CloudFormation AWS::ApplicationSignals::InstrumentationConfig resource provisioner). Optional CodeCaptureConfiguration and CaptureLimitsConfig members with no modeled `@default` must be stored as absent when the request omits them and omitted from the response; do not substitute the documented capture-time defaults at write or at serialization time. Only CaptureLimits itself is @required and must remain present.

Traces: application-signals__cloudformation.deploy-application-signals-instrumentation-config-and-slo-round-trip-and-delete (assertion at check line 245, "instrumentation configurations ... expected ...").

## Evidence
Check: application-signals__cloudformation.deploy-application-signals-instrumentation-config-and-slo-round-trip-and-delete

Template property (AWS::ApplicationSignals::InstrumentationConfig):
  "CaptureConfiguration": {"CodeCapture": {"CaptureLimits": {"MaxHits": 10}}}

Expected (recorded AWS, ListInstrumentationConfigurations -> LatestConfigurations[0].CaptureConfiguration):
  {'CodeCapture': {'CaptureLimits': {'MaxHits': 10}}}

Actual (emulator):
  {'CodeCapture': {'CaptureReturn': False, 'CaptureStackTrace': True, 'CaptureLimits': {'MaxHits': 10, 'MaxStringLength': 128, 'MaxCollectionWidth': 10, 'MaxCollectionDepth': 3, 'MaxStackFrames': 2, 'MaxStackTraceSize': 1000, 'MaxObjectDepth': 3, 'MaxFieldsPerObject': 10}}}

AssertionError: instrumentation configurations [('arn:aws:application-signals:<region>:<account>:instrumentationConfig/<service>/<environment>/SNAPSHOT/<16-hex location hash>', '<location hash>', 'SNAPSHOT', {'CodeLocation': {...}}, {'CodeCapture': {'CaptureReturn': False, 'CaptureStackTrace': True, 'CaptureLimits': {...8 members...}}})], expected [(..., {'CodeCapture': {'CaptureLimits': {'MaxHits': 10}}})]
(the ARN, service/environment/metric names and location hash are per-run values and matched; only CaptureConfiguration diverged)

Model evidence: in com-amazonaws-applicationsignals-applicationsignals.json, CodeCaptureConfiguration.CaptureReturn / CaptureStackTrace and all eight CaptureLimitsConfig members are optional with range/length constraints but NO `default` trait, unlike ListInstrumentationConfigurationsRequest.MaxResults which is modeled `default: 50`.

## Steps to Reproduce
1. Boot the emulator and create a CloudFormation stack containing an AWS::ApplicationSignals::InstrumentationConfig with InstrumentationType=PROBE, SignalType=SNAPSHOT, a Location.CodeLocation, and CaptureConfiguration set to exactly {"CodeCapture": {"CaptureLimits": {"MaxHits": 10}}} (no other CodeCapture or CaptureLimits members). Wait for CREATE_COMPLETE.
2. Call application-signals ListInstrumentationConfigurations with the stack's Service, Environment and InstrumentationType=PROBE (or GetInstrumentationConfiguration with the LocationHash from the resource's LocationHash attribute).
3. Inspect LatestConfigurations[0].CaptureConfiguration: the emulator returns CaptureReturn, CaptureStackTrace and all eight CaptureLimits members; AWS returns only {"CodeCapture": {"CaptureLimits": {"MaxHits": 10}}}.
4. Re-replay application-signals__cloudformation.deploy-application-signals-instrumentation-config-and-slo-round-trip-and-delete to confirm the fix.
