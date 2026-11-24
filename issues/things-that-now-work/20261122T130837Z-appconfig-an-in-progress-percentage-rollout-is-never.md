Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig.exponential-growth-type-advances-in-powers-of-the-growth-factor
Issue-Shape-Id: com.amazonaws.appconfig#AmazonAppConfig
Issue-Key: appconfig.exponential-growth-type-advances-in-powers-of-the-growth-factor|run#42|AssertionError|assert v1_bytes in mid_payloads and v2_bytes in mid_payloads, (
Issue-Commit: 8e4854a18a70a7338c2bf03aa195a8119bab6faa

# appconfig: an in-progress percentage rollout is never partial — the appconfigdata data plane serves the old version to every session until the deployment completes, then flips all at once

> _Note: this issue was identified against the code base at commit `8e4854a18a70a7338c2bf03aa195a8119bab6faa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetLatestConfiguration

## Description
Cause 1 of 1: a deployment that is part-way through its growth schedule is not actually available to a percentage of targets. The AppConfig control plane advances the rollout correctly (the emulator reports GrowthType=EXPONENTIAL, GrowthFactor=10, the G*(2^N) percentage grid 10/20/40/80/100, five PERCENTAGE_UPDATED EventLog entries, and State COMPLETE — all of those assertions in the check pass), but the appconfigdata data plane ignores PercentageComplete entirely: while 0 < PercentageComplete < 100 every configuration session is served the PREVIOUSLY deployed version, and the new version only becomes visible once the deployment reaches its terminal state. The rollout is therefore all-or-nothing rather than gradual.

AWS: Smithy (appconfig) — Deployment$PercentageComplete is documented as "The percentage of targets for which the deployment is available." CreateDeploymentStrategy$GrowthType documents that AppConfig "processes the deployment by dividing the total number of targets" and rolls the configuration out to that percentage of hosts at each step (LINEAR), and exponentially via G*(2^N) for EXPONENTIAL — "2% of the targets, 4% of the targets, 8% of the targets, and continues until the configuration has been deployed to all targets." So at a reported PercentageComplete of P (0 < P < 100), approximately P percent of independent configuration sessions must be served the NEW configuration version's bytes and the remainder the OLD version's bytes; both versions are in service simultaneously. Bucketing is per session, so the only requirement is that both versions appear across a sample, never an exact proportion. Each served document is one hosted version's exact bytes — never blended or torn (the emulator does satisfy this part). The gradual-rollout semantics are the documented purpose of GrowthFactor/GrowthType, and the data plane (appconfigdata StartConfigurationSession + GetLatestConfiguration) is the path through which a target observes its bucket.

Emulator: across 8 independent mid-rollout sessions sampled while the emulator itself reported 0 < PercentageComplete < 100, the only payload ever served was the old version's bytes (the check's own words: "across 8 mid-rollout sessions only [b'{\"tier\":\"one\"}'] was ever served, so the new version reached either none or all of the fleet instead of a percentage of it"). After the deployment reached COMPLETE, every session was served the new version. No status or error code diverges here — every call returns 200 and well-formed bytes; the divergence is purely in WHICH version the data plane selects for an in-flight deployment.

Why: a client that uses a percentage rollout to validate a configuration change on a slice of its fleet gets no slice. Any consumer that polls the data plane expecting to see both the old and the new configuration in service during the rollout (canary validation, a monitor that compares behavior across the two versions, a rollback decision taken on partial exposure) sees a single instantaneous cutover instead, so the whole point of GrowthFactor/GrowthType/DeploymentDurationInMinutes is unobservable from the data plane. A bake-time or CloudWatch-alarm rollback exercised against a partially exposed fleet can never be reproduced against the emulator.

Fix: the appconfigdata data-plane resolution of which configuration version a session is served — the handler behind StartConfigurationSession/GetLatestConfiguration must consult the environment's in-flight deployment and bucket the session by the deployment's current rollout percentage (a stable per-session assignment), rather than reading only the last COMPLETEd deployment's version. The deployment state that the control plane advances through the growth grid needs to expose that current percentage to the data plane; the growth-schedule computation itself is already correct and should not change.

Traces: appconfig.exponential-growth-type-advances-in-powers-of-the-growth-factor (assertion at line 337 of the check: `assert v1_bytes in mid_payloads and v2_bytes in mid_payloads`). Note this is NOT covered by the open issues on Deployment$State casing (20261115T214126Z / 20261116T014021Z / 20261116T220658Z) nor by 20261119T030428Z (S3-backed profiles never served): here the control-plane states and percentages are accepted by the check and the hosted configuration IS served — only the mid-rollout version selection is wrong.

## Evidence
Check: appconfig.exponential-growth-type-advances-in-powers-of-the-growth-factor [mismatch]

Setup (all succeeded against the emulator): application + environment + hosted configuration profile; hosted version 1 = {"tier":"one"}, hosted version 2 = {"tier":"two"}; an instant strategy (duration 0, GrowthFactor 100) deployed version 1 and all 5 baseline sessions were served version 1's bytes; an EXPONENTIAL strategy with GrowthFactor=10, DeploymentDurationInMinutes=4, FinalBakeTimeInMinutes=0.

Act: StartDeployment of version 2 with the EXPONENTIAL strategy. GetDeployment reported GrowthType="EXPONENTIAL" and GrowthFactor=10.0 (passed), the observed PercentageComplete sequence lay on the exponential grid [10.0, 20.0, 40.0, 80.0, 100.0] and advanced monotonically to 100.0 (passed), and the EventLog carried exactly 5 PERCENTAGE_UPDATED entries, not LINEAR's 10 (passed). State reached COMPLETE (passed).

Expected: while the emulator reported 0 < PercentageComplete < 100, independent StartConfigurationSession/GetLatestConfiguration sessions are served a mix of the two hosted versions — both b'{"tier":"one"}' and b'{"tier":"two"}' appear across the mid-rollout sample (no particular proportion required).

Actual: AssertionError: the rollout was not partial — across 8 mid-rollout sessions only [b'{"tier":"one"}'] was ever served, so the new version reached either none or all of the fleet instead of a percentage of it

No payload was torn or blended, and post-completion convergence on version 2 was not reached because the check failed first; the single divergence is that 100% of mid-rollout sessions got the pre-deployment version while the deployment reported itself 10–80% available.

## Steps to Reproduce
1. appconfig: CreateApplication, CreateEnvironment, CreateConfigurationProfile(LocationUri="hosted").
2. CreateHostedConfigurationVersion twice: version 1 content {"tier":"one"}, version 2 content {"tier":"two"} (ContentType application/json).
3. CreateDeploymentStrategy(DeploymentDurationInMinutes=0, GrowthFactor=100, FinalBakeTimeInMinutes=0, ReplicateTo=NONE); StartDeployment of version 1 with it and wait for State COMPLETE. Confirm every appconfigdata session is served version 1's bytes.
4. CreateDeploymentStrategy(DeploymentDurationInMinutes=4, GrowthFactor=10, GrowthType=EXPONENTIAL, FinalBakeTimeInMinutes=0, ReplicateTo=NONE).
5. StartDeployment of version 2 with the exponential strategy.
6. Poll GetDeployment. Whenever 0 < PercentageComplete < 100, open several INDEPENDENT appconfigdata sessions (StartConfigurationSession then GetLatestConfiguration) and record the bytes each is served.
7. Observe: every mid-rollout session is served version 1's bytes; version 2's bytes never appear until the deployment reaches COMPLETE, at which point every session is served version 2. Expected: both versions served concurrently while the rollout is between 0% and 100%.
