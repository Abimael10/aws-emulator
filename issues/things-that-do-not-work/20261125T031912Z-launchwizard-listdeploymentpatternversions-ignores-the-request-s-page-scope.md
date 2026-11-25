Issue-Source: trace-sweep-divergence
Issue-Trace: launch-wizard-workload-catalog-discovery
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: launch-wizard-workload-catalog-discovery
Issue-Shape-Id: com.amazonaws.launchwizard#LaunchWizard
Issue-Key: launchwizard|ListDeploymentPatternVersions|2xx->2xx|$.deploymentPatternVersions[*]
Issue-Key: launchwizard|ListDeploymentPatternVersions|2xx->2xx|$.nextToken
Issue-Commit: 643d7fc40de53dee6cfee0ebba73d0a58d0d5505

# launchwizard ListDeploymentPatternVersions ignores the request's page scope and publishes an invented version line

> _Note: this issue was identified against the code base at commit `643d7fc40de53dee6cfee0ebba73d0a58d0d5505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDeploymentPatternVersions

## Description
Cause 1 of 1 — the deployment-pattern-version page the emulator answers is neither scoped as the request asks nor drawn from a published version line.
AWS: ListDeploymentPatternVersions is @readonly, HTTP 200, paginated with inputToken/outputToken nextToken (len 1..2048), pageSize maxResults (range 1..100) and items deploymentPatternVersions; a page that selects nothing on this pass still carries nextToken when further pages remain, and nextToken is absent only when there are no more items (Smithy: ListDeploymentPatternVersions, DeploymentPatternVersionDataSummary members deploymentPatternVersionName/description/documentationUrl/workloadName/deploymentPatternName; FilterList of DeploymentPatternVersionFilter). Recorded behavior: for this step the recording's body carries NO deploymentPatternVersions entry and a nextToken (recorded as the placeholder 'opaque-pagination-token' — the token's literal value is per-run and is not the requirement; its PRESENCE is).
Emulator: HTTP 200 with three entries for workloadName SAP / deploymentPatternName SapHanaHA — deploymentPatternVersionName 1.0.27, 1.0.1 and 1.0.0, each with a description derived in-crate from the pattern's own description ("Version 1.0.27 of this deployment pattern. Deploys an SAP HANA database as a high-availability deployment across two Availability Zones.") and a featureFlags block (cloudFormationOutput/modifyDeployments/postDeploymentSteps all DISABLED, updateFromVersion []) — and NO nextToken. Hypothesis: the handler materialises every seeded version of the pattern and only then applies its own paginate/filter helper, so the request's page scope (maxResults/filters/nextToken) does not shape the result the way the recording shows, and because the whole seeded line fits in one page it reports no nextToken.
Why: a client paginating the version list is told the listing is complete when AWS says more pages remain, so it stops early and never sees the versions on the following pages; and the versions it does see (plus their derived descriptions and the featureFlags capability block) are the emulator's own invention, so a client that picks a version from this page and passes it to CreateDeployment/UpdateDeployment names a version the service does not publish.
Fix: the ListDeploymentPatternVersions handler's page/filter composition (services/.../src/ops/list_deployment_pattern_versions.rs) together with the seeded per-pattern version line and its projection in the state module (`seeded_pattern_versions`, `version_summaries`, `state::pattern_version_capabilities`).
Traces: launch-wizard-workload-catalog-discovery#task#5.

The earlier version-read issues (20261110T073938Z-launchwizard-deployment-pattern-version-responses-ignore-pagination-and-omit... and 20261121T052824Z-launchwizard-deployment-pattern-version-reads-still-diverge-pagination) are RESOLVED and therefore do not cover this step: the corpus still replays a page that ignores the request's scope and reports no nextToken.

## Evidence
Replay of trace set simparity-wire-aws-v1, divergence cluster service=launchwizard|ListDeploymentPatternVersions|status=200->200, fixture launch-wizard-workload-catalog-discovery, step task#5 (affected: launch-wizard-workload-catalog-discovery#task#5), expected status 200, actual status 200. Detail:
  "$.deploymentPatternVersions[0]: extra (expected None, got {'deploymentPatternVersionName': '1.0.27', 'description': 'Version 1.0.27 of this deployment pattern. Deploys an SAP HANA database as a high-availability deployment across two Availability Zones.', 'workloadName': 'SAP', 'deploymentPatternName': 'SapHanaHA', 'featureFlags': {'cloudFormationOutput': {'configuration': {'fields': []}, 'status': 'DISABLED'}, 'modifyDeployments': {'status': 'DISABLED'}, 'postDeploymentSteps': {'status': 'DISABLED'}, 'updateFromVersion': []}})"
  "$.deploymentPatternVersions[1]: extra (expected None, got {'deploymentPatternVersionName': '1.0.1', ...})"
  "$.deploymentPatternVersions[2]: extra (expected None, got {'deploymentPatternVersionName': '1.0.0', ...})"
  "$.nextToken: missing (expected 'opaque-pagination-token', got None)"

Code: services/service-com-amazonaws-launchwizard-launchwizard/src/ops/list_deployment_pattern_versions.rs lines 212-272 (all version_summaries materialised, filtered, then paginated); services/service-com-amazonaws-launchwizard-launchwizard/src/state.rs line 489 (DeploymentPatternVersionDataSummary projection incl. featureFlags) and line 2851 (`seeded_pattern_versions`, descriptions derived from the pattern description).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay fixture launch-wizard-workload-catalog-discovery and stop at step task#5 (POST /listDeploymentPatternVersions for workloadName SAP, deploymentPatternName SapHanaHA as the fixture records it).
3. Compare the body with the recorded response: the emulator returns three deploymentPatternVersions entries (1.0.27, 1.0.1, 1.0.0, each with a derived description and a featureFlags block) and omits nextToken, where the recording carries no entry at indices 0..2 and does carry a nextToken.
