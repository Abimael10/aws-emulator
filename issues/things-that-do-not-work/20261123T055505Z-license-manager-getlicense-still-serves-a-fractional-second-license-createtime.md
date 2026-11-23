Issue-Source: trace-sweep-divergence
Issue-Trace: license-manager-checkout-checkin-tracks-entitlement-usage
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: license-manager-checkout-checkin-tracks-entitlement-usage
Issue-Shape-Id: com.amazonaws.licensemanager#AWSLicenseManager
Issue-Key: license-manager|GetLicense|2xx->2xx|$.License.CreateTime
Issue-Commit: 609217dca61089ea930f65d0212d84467c6e9593

# GetLicense still serves a fractional-second License.CreateTime the recorded shape rejects

> _Note: this issue was identified against the code base at commit `609217dca61089ea930f65d0212d84467c6e9593`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetLicense

## Description
GetLicense answers 200 with the right License body in every member except the creation instant: the emulator renders License.CreateTime with a fractional-second field, and the fixture's shaped comparison of that member rejects the rendering. ONE cause.

Cause 1 of 1: License.CreateTime is rendered with a sub-second (millisecond) fractional field, and in a form that varies call to call, where the recorded response carries a whole-second ISO8601-UTC instant.
AWS: Smithy: License.CreateTime targets ISO8601DateTime (@length 0..50, pattern '^(-?(?:[1-9][0-9]*)?[0-9]{4})-(1[0-2]|0[1-9])-(3[0-1]|0[1-9]|[1-2][0-9])T(2[0-3]|[0-1][0-9]):([0-5][0-9]):([0-5][0-9])(\.[0-9]+)?(Z|[+-](?:2[ 0-3]|[0-1][0-9]):[0-5][0-9])+$'), doc 'License creation time'; the model's optional '(\.[0-9]+)?' group means the pattern alone does not pin the precision, so the precision is recorded behavior, not a model constraint. Recorded behavior only: this member's instant is per-run, so the fixture compares it by SHAPE rather than by value ('<shaped-not-asserted>'), and the recorded shape does NOT admit a fractional-second field — the recorded CreateTime is a whole-second, Z-suffixed UTC instant (YYYY-MM-DDThh:mm:ssZ). Hypothesis: real License Manager truncates the creation instant to the second before reporting it, so ANY fractional field fails the comparison regardless of how many digits it carries.
Emulator: crate::state::iso8601 (reached through crate::state::now_iso8601, which CreateLicense stores into StoredLicense.create_time and License::wire() projects unchanged) renders the whole second through aws_smithy_types Format::DateTime and then appends '.' plus the clock's millisecond field zero-padded to RENDERED_SUBSECOND_DIGITS = 3, producing 'YYYY-MM-DDThh:mm:ss.sssZ'. Replay: expected status 200, actual status 200; $.License.CreateTime: value_mismatch (expected '<shaped-not-asserted>', got a three-fractional-digit instant — the date, time and digits of that run are values filling the template, not part of it). The helper additionally emits TWO different formats: it drops the fractional field entirely when the millisecond field happens to be zero and emits it otherwise, so the response format is not stable across calls.
Why: a client that parses CreateTime with a strict reader for the form real License Manager emits, that string-matches or golden-compares a license body, or that round-trips the value back into another API, sees a value AWS never produces — and because the fractional field appears or vanishes depending on the clock, the failure is intermittent rather than reproducible. CreateTime is the only member of the License read body with this defect; the issuer fingerprint half of the earlier divergence now matches.
Fix: the timestamp rendering helper in this service's state — crate::state::iso8601 / crate::state::now_iso8601 — must truncate the instant to the whole second and emit exactly one stable form, rather than appending a millisecond field; every stored instant this service reports goes through it, so the GetLicense handler (which only projects the stored record) and License::wire() need no change.
Traces: license-manager-checkout-checkin-tracks-entitlement-usage#task#3.

Relationship to already-filed work: the RESOLVED issue '20261109T151407Z-license-manager-getlicense-reports-a-derived-license-issuer-keyfingerprint-and.md' covered this same fixture and step and closed its KeyFingerprint cause, but its CreateTime cause was closed on the hypothesis that millisecond precision is what AWS emits; the shaped comparison still rejects the member after that change, so this is a live, net-new divergence and a resolved issue does not cover it. No OPEN license-manager issue mentions it: the five open license-manager-* issues are all against license-manager-linux-subscriptions operations (RegisterSubscriptionProvider, GetRegisteredSubscriptionProvider, ListLinuxSubscriptions) and say nothing about the License read body.

## Evidence
Replay of fixture license-manager-checkout-checkin-tracks-entitlement-usage (trace set simparity-wire-aws-v1), step task#3, operation GetLicense, service license-manager: expected status 200, actual status 200 — the response BODY diverges. Cluster key service=license-manager|GetLicense|status=200->200|keys=$.License.CreateTime, 1 affected step.

Divergence detail as replayed:
  $.License.CreateTime: value_mismatch (expected '<shaped-not-asserted>', got '2026-11-23T05:52:18.063Z')

The expected side is a shaped (non-literal) comparison because the instant is per-run; the date and time in the actual value are therefore NOT the requirement. What diverges is the presence of the fractional-second field: the actual carries '.063' between the seconds and the 'Z', and the recorded shape carries no fractional field at all. Nothing else in the License body diverges — the ARN, name, product, issuer (including Issuer.KeyFingerprint), home region, status, validity, beneficiary, entitlements, consumption configuration and version all match.

Model evidence (services/service-com-amazonaws-licensemanager-awslicensemanager/smithy-model/com-amazonaws-licensemanager-awslicensemanager.json): GetLicense input {LicenseArn: Arn required, Version: String}, output {License}; License.CreateTime -> ISO8601DateTime, @length 0..50, pattern with an OPTIONAL '(\.[0-9]+)?' fractional group, so both the whole-second and the fractional forms satisfy the model and the model does not decide this.

Code evidence: services/service-com-amazonaws-licensemanager-awslicensemanager/src/ops/get_license.rs lines 189-191 return license.wire() verbatim; src/state.rs lines 324-341 (StoredLicense::wire) copy create_time unchanged; src/state.rs lines 1983-2030 define RENDERED_SUBSECOND_DIGITS = 3 and iso8601(), which appends '.{millis:03}' to the Format::DateTime rendering when millis != 0 and returns the bare whole-second rendering when millis == 0 — the two-format instability. Commit history: the resolved issue 20261109T151407Z-... moved this helper from the clock's full microsecond precision to three digits; the shaped comparison rejected '…372014Z' then and rejects '…063Z' now.

## Steps to Reproduce
Replay fixture license-manager-checkout-checkin-tracks-entitlement-usage from trace set simparity-wire-aws-v1 against the emulator (every step is in its 'task' phase):
1. The fixture's earlier task steps CreateLicense (an issuer, a product SKU, a validity range and entitlements) and CheckoutLicense against the returned LicenseArn.
2. task#3 — GetLicense on that LicenseArn. Status is 200 on both sides; compare the body member $.License.CreateTime: it must be an ISO8601-UTC instant at the precision License Manager emits — whole seconds, 'YYYY-MM-DDThh:mm:ssZ', with no fractional-second field. The emulator returns the same instant with a three-digit millisecond field appended ('YYYY-MM-DDThh:mm:ss.sssZ').
3. To see the format instability directly, CreateLicense repeatedly and GetLicense each result: the responses alternate between the fractional and the non-fractional form depending on whether the host clock's millisecond field is zero, so no single strict parser accepts every response the emulator serves.
