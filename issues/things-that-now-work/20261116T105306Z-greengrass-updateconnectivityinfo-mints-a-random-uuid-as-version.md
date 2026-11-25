Issue-Source: trace-sweep-divergence
Issue-Trace: greengrassv2-connectivity-info-roundtrip
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: greengrassv2-connectivity-info-roundtrip
Issue-Shape-Id: com.amazonaws.greengrassv2#GreengrassV2
Issue-Key: greengrass|UpdateConnectivityInfo|2xx->2xx|$.Version
Issue-Commit: cf9fc6ab8267f9d5f0acca19e6a4a41633dc20ed

# greengrass UpdateConnectivityInfo mints a random UUID as Version instead of AWS's timestamp version token

> _Note: this issue was identified against the code base at commit `cf9fc6ab8267f9d5f0acca19e6a4a41633dc20ed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateConnectivityInfo

## Description
One net-new cause on the legacy Greengrass connectivity-information route (PUT /greengrass/things/{ThingName}/connectivityInfo), which this service co-hosts. Not covered by any open issue: the nearest one, 20261115T063730Z-greengrassv2-legacy-greengrass-routes-diverge-service-role-404.md, covers the UpdateConnectivityInfo refusal envelope/wording and GetConnectivityInfo's absent-record path, not the shape of the version token a SUCCESSFUL write answers; the replay marked this cluster as owned by nothing.

Cause 1 of 1: the Version member of a successful UpdateConnectivityInfo response is not a version token of AWS's form.
  AWS: Smithy (com.amazonaws.greengrass#UpdateConnectivityInfo) binds PUT /greengrass/things/{ThingName}/connectivityInfo, success 200, output members Message (jsonName "message") and Version (__string, "The new version of the connectivity info"); the model constrains the string no further, so the token's FORM is recorded behavior only. The recording shows the service answering Version as an ISO-8601 UTC instant for the write — a date-time with millisecond precision and a trailing 'Z' (of the shape YYYY-MM-DDTHH:MM:SS.sssZ) — i.e. the version is derived from the instant the list was stored, and it advances monotonically across successive writes for the same thing name. The digits in any one recording (the particular date, time and milliseconds) are per-run values filled into that template; the template itself — a timestamp-formatted string, not an opaque random identifier — is the requirement, and both writes in the fixture answer one.
  Emulator: answers HTTP 200 with the right body members, but Version is a random UUID v4 (e.g. a value of the form 65a01f1c-f476-42b9-8d9b-5012c26b41fb). It is minted by new_identifier() — the deployment/job opaque-identifier helper — so the token carries no instant, has no ordering relation to the previous write's token, and does not parse as a date-time.
  Why: Version is the token a client keeps to tell which published endpoint list it is looking at and to detect a concurrent overwrite. Code that parses the version as a timestamp (to show when connectivity information was last published, to age it out, or to decide which of two observed versions is newer) fails to parse it or orders two UUIDs arbitrarily; a client that round-trips Update then Get and compares the version against a recorded/stored timestamp string never matches. The divergence is silent — the call succeeds — so it surfaces as wrong staleness/ordering decisions rather than an error.
  Fix: the version minting for connectivity information in this service's state, i.e. publish_connectivity_info's token source (it must derive the token from the store instant in AWS's timestamp form rather than calling the opaque UUID identifier helper), with the UpdateConnectivityInfo handler and GetConnectivityInfo continuing to report whatever the record holds.
  Traces: greengrassv2-connectivity-info-roundtrip#task#1 and greengrassv2-connectivity-info-roundtrip#task#3 (the second write of the same round-trip), both diverging on $.Version only. Code: services/service-com-amazonaws-greengrassv2-greengrassv2/src/state/connectivity_info.rs:131 (`let version = new_identifier();` in publish_connectivity_info, record field documented at lines 32-35) and services/service-com-amazonaws-greengrassv2-greengrassv2/src/state.rs:1107-1110 (new_identifier -> Uuid::new_v4()); the handler that returns it is services/service-com-amazonaws-greengrassv2-greengrassv2/src/ops/update_connectivity_info.rs:169-212.

## Evidence
Replay of fixture greengrassv2-connectivity-info-roundtrip, step task#1 (and task#3, same cluster): expected HTTP 200 with $.Version a timestamp-form version token (recorded value '2026-09-06T05:01:43.511Z', i.e. YYYY-MM-DDTHH:MM:SS.sssZ — the digits are per-run, the ISO-8601 millisecond-precision UTC form is the requirement); actual HTTP 200 with $.Version = '65a01f1c-f476-42b9-8d9b-5012c26b41fb', a UUID v4. Verdict: mismatch, keys=$.Version, status 200->200; every other member of the response matched, and the Get leg reads back the same token the Update answered. Source of the actual value: publish_connectivity_info calls new_identifier(), which is Uuid::new_v4().to_string().

## Steps to Reproduce
1. Boot the emulator and point a client at the legacy Greengrass connectivity-information route.
2. UpdateConnectivityInfo(ThingName=T, ConnectivityInfo=[{Id:"1", HostAddress:"192.0.2.10", PortNumber:8883}]) -> HTTP 200. Inspect the response's Version member: it is a UUID, not an ISO-8601 UTC timestamp of the form YYYY-MM-DDTHH:MM:SS.sssZ.
3. Call UpdateConnectivityInfo again for the same ThingName with a different endpoint list. The second Version is another unrelated UUID, so it carries no instant and is not ordered after the first.
4. GetConnectivityInfo(ThingName=T) reads back the stored list; the version the write answered cannot be parsed or compared as a timestamp.
5. Replay fixture greengrassv2-connectivity-info-roundtrip, steps task#1 and task#3.
