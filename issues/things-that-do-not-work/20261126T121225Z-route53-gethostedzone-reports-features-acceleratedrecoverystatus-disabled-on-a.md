Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53.update-hosted-zone-features-limits-exceeded-rejected
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53.update-hosted-zone-features-limits-exceeded-rejected|run#5|AssertionError|assert baseline is None, f"expected a fresh zone to report no status yet, got {baseline!r}"
Issue-Commit: 43586243266d19b209b15f2de40e433673053310

# route53: GetHostedZone reports Features.AcceleratedRecoveryStatus=DISABLED on a zone whose features were never configured, where AWS omits the member

> _Note: this issue was identified against the code base at commit `43586243266d19b209b15f2de40e433673053310`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetHostedZone

## Description
ONE net-new cause, found by the behavioral suite's error-semantics check for UpdateHostedZoneFeatures. The check never got past its first probe, so the rest of the UpdateHostedZoneFeatures refusal semantics it exercises (the InvalidInput already-disabled refusal and the NoSuchHostedZone refusal) are still ungraded and may hide further divergence behind this one.

Cause 1 of 1: a hosted-zone read DEFAULTS the accelerated-recovery feature status instead of leaving it unreported for a zone nothing ever configured the feature on. Failing check: route53.update-hosted-zone-features-limits-exceeded-rejected.

AWS: GET /2013-04-01/hostedzone/{Id} answers 200 with HostedZone. For a hosted zone that was just created and against which no UpdateHostedZoneFeatures has been accepted, the response carries NO AcceleratedRecoveryStatus inside HostedZone.Features — the feature has never been configured, so there is no status to report, and a client reads the member as absent (botocore: hosted_zone['Features'].get('AcceleratedRecoveryStatus') is None). The zone is nonetheless implicitly off, which is why UpdateHostedZoneFeatures with EnableAcceleratedRecovery=false against that same fresh zone is refused 400 InvalidInput with Message 'Accelerated recovery is already disabled for this hosted zone' (fixed text; no request- or state-derived parts). Smithy (services/service-com-amazonaws-route53-awsdnsv20130401/smithy-model/com-amazonaws-route53-awsdnsv20130401.json): HostedZone.Features targets HostedZoneFeatures{AcceleratedRecoveryStatus: AcceleratedRecoveryStatus enum [ENABLING, ENABLE_FAILED, ENABLING_HOSTED_ZONE_LOCKED, ENABLED, DISABLING, DISABLE_FAILED, DISABLED, DISABLING_HOSTED_ZONE_LOCKED], FailureReasons, PublicDNSFallbackStatus}, all OPTIONAL, documented as 'The current status of accelerated recovery for the hosted zone'; UpdateHostedZoneFeatures models InvalidInput (400) and NoSuchHostedZone (404). The model does not say the member is always serialized, so 'absent until the feature is configured' is recorded behavior only — it is what the replayed check observes of AWS.

Emulator: GetHostedZone on a freshly created public zone answers 200 with Features.AcceleratedRecoveryStatus = 'DISABLED'. The single-zone projection deliberately substitutes the OFF status for 'never configured': the zone's stored accelerated-recovery record is None for a new zone, and the projection turns that None into the DISABLED constant, so the member is present from the moment the zone exists.

Why: a client cannot distinguish a zone whose accelerated recovery was explicitly turned off (a state that was configured, and that a client may need to re-enable or wait out of a DISABLING transition) from a zone the feature was never touched on. Feature-posture auditing over zones reports every zone as explicitly DISABLED, and a client that keys 'has this zone ever been configured for accelerated recovery' off the presence of the member takes the wrong branch on every zone in the account. It also makes the emulator's read inconsistent with its own write path, which treats the absent record as 'not configured' when deciding the already-disabled refusal.

Fix: the shared single-zone HostedZone Features projection in the route53 crate's state module (the accelerated-recovery status helper that the GetHostedZone handler's zone projection reaches, and anything else projecting the same Features container) — report the status the zone's stored accelerated-recovery record actually holds, and report NO AcceleratedRecoveryStatus member while that record is absent, rather than defaulting it to the DISABLED constant. Note that the open wire issue 20261121T214447Z-route53-route-53-hostedzone-read-and-list-projections-omit.md asks, from a wire recording, for Features to be emitted on every read/list path (its Cause 2 records AcceleratedRecoveryStatus DISABLED for a public zone); that issue does not cover this behavioral divergence and neither covers the other — the container must still be present, it is the unconfigured feature's STATUS member that must be absent, so a fix must satisfy both and not simply re-add the default.

Traces: behavioral check route53.update-hosted-zone-features-limits-exceeded-rejected (group error-semantics).

## Evidence
Behavioral suite replay against the booted emulator (139/169 passed). Failing check route53.update-hosted-zone-features-limits-exceeded-rejected, verdict [mismatch]:

  File "check_route53_update_hosted_zone_features_limits_exceeded_rejected.py", line 32, in run
    assert baseline is None, f"expected a fresh zone to report no status yet, got {baseline!r}"
  AssertionError: expected a fresh zone to report no status yet, got 'DISABLED'

The check's probe is:
  hosted_zone = route53.get_hosted_zone(Id=zone_id)['HostedZone']
  (hosted_zone.get('Features') or {}).get('AcceleratedRecoveryStatus')
against a zone created moments earlier by CreateHostedZone(Name=<generated>.zone-features-test.com, CallerReference=<uuid>) with no HostedZoneConfig (a PUBLIC zone) and no UpdateHostedZoneFeatures call.

Expected (AWS, recorded): None — HostedZone.Features carries no AcceleratedRecoveryStatus member.
Actual (emulator): 'DISABLED'.

Status matched (200 in both); only the body diverged. The generated zone name and the CallerReference uuid are per-run values and are not part of the requirement.

Code evidence in the checkout: services/service-com-amazonaws-route53-awsdnsv20130401/src/state.rs — StoredZone::accelerated_recovery_status() (around lines 1187-1195) returns Some(self.configured_accelerated_recovery_status().unwrap_or(ACCELERATED_RECOVERY_DISABLED)) for any non-private zone, while configured_accelerated_recovery_status() (around lines 1199-1203) correctly returns None for a zone with no stored AcceleratedRecovery record; StoredZone::to_hosted_zone_features() (around lines 1169-1178) feeds the defaulted value into the Features container read by services/service-com-amazonaws-route53-awsdnsv20130401/src/ops/get_hosted_zone.rs (zone.to_hosted_zone(), line 160). The account-wide listing projection StoredZone::listed_features() (around lines 1144-1153) already uses the undefaulted helper, so the two projections disagree with each other.

Model evidence: smithy_query(query="shape", shape="HostedZone,HostedZoneFeatures,AcceleratedRecoveryStatus") and smithy_query(query="operation", operation="GetHostedZone,UpdateHostedZoneFeatures") on services/service-com-amazonaws-route53-awsdnsv20130401/smithy-model/com-amazonaws-route53-awsdnsv20130401.json — all three HostedZoneFeatures members are optional; GetHostedZone output HostedZone is @required; UpdateHostedZoneFeatures errors: InvalidInput 400, LimitsExceeded 400, NoSuchHostedZone 404, PriorRequestNotComplete 400.

## Steps to Reproduce
1. Boot the emulator from this checkout and run the behavioral suite (simparity-behavioral-aws-v1).
2. Replay check route53.update-hosted-zone-features-limits-exceeded-rejected.
3. Equivalently, by hand against the booted emulator:
   a. route53.create_hosted_zone(Name='<unique>.zone-features-test.com', CallerReference=str(uuid4())) with no HostedZoneConfig, so a PUBLIC zone is created; keep HostedZone.Id.
   b. route53.get_hosted_zone(Id=<that id>) and read HostedZone['Features'].get('AcceleratedRecoveryStatus').
   AWS: the member is absent (None) — nothing has configured the feature on this zone.
   Emulator: the member is present with value 'DISABLED'.
4. The rest of the check then exercises UpdateHostedZoneFeatures semantics that are currently ungraded because step 3 aborts it: UpdateHostedZoneFeatures(HostedZoneId=<that id>, EnableAcceleratedRecovery=False) must be refused 400 InvalidInput with Message 'Accelerated recovery is already disabled for this hosted zone'; UpdateHostedZoneFeatures(HostedZoneId='Z00000000000000000000', EnableAcceleratedRecovery=True) must be refused 404 NoSuchHostedZone with Message 'No hosted zone found with ID: Z00000000000000000000' (fixed text 'No hosted zone found with ID: ' plus the request's HostedZoneId); and neither refusal may change what GetHostedZone reports for the real zone's Features. Re-run the check after the fix so these assertions are reached and graded too.
