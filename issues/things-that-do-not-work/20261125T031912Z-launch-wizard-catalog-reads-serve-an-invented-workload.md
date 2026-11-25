Issue-Source: trace-sweep-divergence
Issue-Trace: launch-wizard-catalog-list-workload-deployment-patterns
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: launch-wizard-catalog-list-workload-deployment-patterns launch-wizard-catalog-list-workloads
Issue-Shape-Id: com.amazonaws.launchwizard#LaunchWizard
Issue-Key: launch-wizard|ListWorkloadDeploymentPatterns|2xx->4xx|$.workloadDeploymentPatterns
Issue-Key: launch-wizard|ListWorkloadDeploymentPatterns|2xx->4xx|status
Issue-Key: launch-wizard|ListWorkloadDeploymentPatterns|2xx->2xx|$.workloadDeploymentPatterns[*]
Issue-Key: launch-wizard|ListWorkloads|2xx->2xx|$.workloads[*]
Issue-Commit: 643d7fc40de53dee6cfee0ebba73d0a58d0d5505

# launch-wizard catalog reads serve an invented workload catalogue: wrong pattern ids, versions and display text, omitted summary members, and 404 for workloads AWS publishes

> _Note: this issue was identified against the code base at commit `643d7fc40de53dee6cfee0ebba73d0a58d0d5505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListWorkloadDeploymentPatterns

## Description
The Launch Wizard read-only catalogue the emulator serves is a hand-written seed table (`CATALOGUE` in the service crate's state module) rather than the catalogue AWS publishes. Every catalogue read in the corpus therefore diverges: the workload set, the deployment-pattern identifiers, the version line each entry is published at, the display text, and the members each summary carries. Six net-new causes, all in the same seed/projection path.

Cause 1 of 6 — ListWorkloadDeploymentPatterns for a workload AWS publishes but the seed does not hold answers 404 instead of the pattern list.
AWS: ListWorkloadDeploymentPatterns is @readonly and answers HTTP 200 with workloadDeploymentPatterns for every workloadName the account's catalogue publishes; ResourceNotFoundException (httpError 404, "The specified workload or deployment resource can't be found.") is reserved for a name the catalogue does NOT publish (Smithy: ListWorkloadDeploymentPatterns.workloadName WorkloadName len 1..100 pattern ^[A-Za-z][a-zA-Z0-9-_]*$; errors ResourceNotFoundException/ValidationException/InternalServerException). Recorded behavior: three steps of the fixture ask for workloads the recording answers 200 for — one of them is workloadName "SQL", whose patterns are SQLHAAlwaysOn, SQLHAFCIONTAP, SQLHALinux, SQLHALinuxRhel (and more) at workloadVersionName/deploymentPatternVersionName 1.0.30, status ACTIVE, accountConstraints [].
Emulator: HTTP 404 with the catalogue not-found ResourceNotFoundException, and no workloadDeploymentPatterns at all, for all three of those workload names. The seed publishes Microsoft SQL Server under workloadName "SQLServer" with patterns SqlServerHA/SqlServerSingle, so the name AWS publishes resolves to nothing.
Why: a client that discovers a workload name from AWS's documentation (or from a recorded ListWorkloads page) cannot reach its deployment patterns at all, and CreateDeployment for that workload is unreachable — the 404 makes a published workload look deleted.
Fix: the seeded catalogue in the service crate's state module (the `CATALOGUE` table and `seeded_workloads`), which must publish AWS's workload names and patterns; the handler's resolve-then-404 rule itself is correct.
Traces: launch-wizard-catalog-list-workload-deployment-patterns#task#5 (cluster also covers #task#6 and #task#7).

Cause 2 of 6 — the SO0023 (Dynamic Image Transformation for Amazon CloudFront) patterns are published at the wrong version with emulator-authored text.
AWS: recorded behavior only. The recording returns, for workloadName SO0023, deploymentPatternName ecs-deployment with workloadVersionName 8.1.1, deploymentPatternVersionName 8.1.1, displayName "ECS architecture deployment", description "ECS Architecture deployment for Dynamic Image Transformation for Amazon CloudFront.", and lambda-deployment with 8.1.1/8.1.1, displayName "Lambda architecture deployment", description "Lambda Architecture deployment for Dynamic Image Transformation for Amazon CloudFront."; status ACTIVE, accountConstraints [].
Emulator: the same two pattern names at workloadVersionName/deploymentPatternVersionName 7.0.0 with its own displayName ("Image transformation on Amazon ECS" / "Image transformation on AWS Lambda") and its own description sentences ("Deploys the image transformation solution's services as tasks on an Amazon ECS cluster.").
Why: the version members are the inputs CreateDeployment/GetWorkloadDeploymentPattern are keyed by, so a client that reads the pattern list and then deploys the version it was told about asks for a version the service does not publish; the display text a console renders is also not the published text.
Fix: the per-entry version line and pattern text in the seeded catalogue table (state module), not the handler.
Traces: launch-wizard-catalog-list-workload-deployment-patterns#task#1.

Cause 3 of 6 — the MicrosoftActiveDirectory patterns are published under identifiers AWS does not use.
AWS: recorded behavior only. The recording returns deploymentPatternName adAwsManagedExistingVpc (displayName "AWS Managed Microsoft AD - existing VPC", description "Deploys AD DS with AWS Directory Service into existing AWS infrastructure."), adAwsManagedNewVpc, adOnPremiseExistingVpc, ... all at workloadVersionName/deploymentPatternVersionName 1.0.0, status ACTIVE.
Emulator: ADExtensionExistingVPC, ADExtensionNewVPC, AWSManagedADExistingVPC, AWSManagedADNewVPC, SelfManagedAD* at version 1.0.5, each with emulator-authored display text ("On-premises Active Directory extension in an existing VPC").
Why: deploymentPatternName is a required input of GetWorkloadDeploymentPattern, ListDeploymentPatternVersions and CreateDeployment; a pattern id that exists only in the emulator means no real-world pattern id resolves, and every id a client learns here is rejected by the real service.
Fix: the seeded catalogue table's MicrosoftActiveDirectory row (state module).
Traces: launch-wizard-catalog-list-workload-deployment-patterns#task#2.

Cause 4 of 6 — the SAP pattern summaries carry emulator-authored displayName/description.
AWS: recorded behavior only. For workloadName SAP at 1.0.27 the recording returns SapHanaHA displayName "SAP HANA database cross AZ high availability deployment" / description "Deploy SAP HANA with high availability configured across two Availability Zones.", SapHanaMulti "SAP HANA database multiple instances deployment" / "Deploy SAP HANA in a multi-node, scale-out architecture.", SapHanaSingle "SAP HANA database single instance deployment" / "Deploy SAP HANA in a single-node, scale-up architecture.", and further patterns through index 7.
Emulator: the same pattern names and version, but "SAP HANA, high availability" / "Deploys an SAP HANA database as a high-availability deployment across two Availability Zones." and so on — text written in the crate, and a pattern set that does not line up index-for-index with the recorded one.
Why: the catalogue is what a client renders and what it matches patterns on by description/display name; publishing invented prose makes the catalogue read differ from AWS for every SAP pattern.
Fix: the seeded catalogue table's SAP row (state module).
Traces: launch-wizard-catalog-list-workload-deployment-patterns#task#3.

Cause 5 of 6 — the SO0062 (Distributed Load Testing) patterns are entirely different entries.
AWS: recorded behavior only. The recording returns, at workloadVersionName/deploymentPatternVersionName 4.3.0, deploymentPatternName ALB-ECS (displayName "Distributed Load Testing - ALB Deployed Web Console (ECS)"), Default ("Distributed Load Testing - CloudFront Deployed Web Console") and Headless, status ACTIVE.
Emulator: existing-vpc-deployment, fargate-deployment and regional-deployment at 3.3.0 with its own display text.
Why: as in cause 3 — none of the pattern ids a client takes from this listing exist on AWS, and the version is wrong, so the whole deploy path for this workload is keyed on fictional identifiers.
Fix: the seeded catalogue table's SO0062 row (state module).
Traces: launch-wizard-catalog-list-workload-deployment-patterns#task#4.

Cause 6 of 6 — ListWorkloads serves the wrong workload set and omits the members each summary carries.
AWS: ListWorkloads is @readonly, HTTP 200, items WorkloadDataSummaryList (Smithy: ListWorkloads, paginated items=workloads). Recorded behavior: the listing holds seven entries, among them MicrosoftActiveDirectory, SO0023, SAP and the distributed-load-testing solution, and each entry carries description, documentationUrl and workloadOwnerDisplayName (e.g. workloadOwnerDisplayName "AWS Launch Wizard" for MicrosoftActiveDirectory and "AWS Specialist Engineering" for SO0023) alongside workloadName, displayName, status and accountConstraints. The on-disk model's WorkloadDataSummary declares only workloadName/displayName/status/accountConstraints, so the model is behind the service and the recording is the requirement.
Emulator: five seeded rows (SAP, SQLServer, MicrosoftActiveDirectory, SO0023, SO0062) sorted by workloadName, each projected by `Workload::to_summary` with exactly four members — no description, no documentationUrl, no workloadOwnerDisplayName — so every index of the recorded listing diverges ($.workloads[0]..$.workloads[6]).
Why: ListWorkloads is the documented discovery entry point ("use ListWorkloadDeploymentPatterns to discover ... for a given workload"); a client that enumerates workloads gets a short, differently ordered list with no owner or documentation metadata, and cannot find the workloads that are missing at all.
Fix: the seeded catalogue table plus the ListWorkloads summary projection in the state module (`Workload::to_summary`), and the WorkloadDataSummary shape it serializes through.
Traces: launch-wizard-catalog-list-workloads#task#1.

The earlier catalogue issues (20261029T234833Z-launch-wizard-catalog-returns-placeholder-workload-data-listworkloads and 20261115T082538Z-launch-wizard-workload-catalog-is-seeded-with-invented-entries-list) are RESOLVED and do not cover these: the corpus still replays a seeded, invented catalogue.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 72.7%, 9 mismatched steps of 33):

1) launch-wizard-catalog-list-workload-deployment-patterns#task#5 (cluster: task#5, task#6, task#7), ListWorkloadDeploymentPatterns — expected status 200, actual 404; detail: "status: expected 200, got 404" and "$.workloadDeploymentPatterns: missing (expected [{'deploymentPatternName': 'SQLHAAlwaysOn', 'deploymentPatternVersionName': '1.0.30', 'displayName': 'SQL Server - Always On (AG) - Windows', 'status': 'ACTIVE', 'workloadName': 'SQL', 'workloadVersionName': '1.0.30'}, {'deploymentPatternName': 'SQLHAFCIONTAP', ...}, {'deploymentPatternName': 'SQLHALinux', ...}, {'deploymentPatternName': 'SQLHALinuxRhel', ...}], got None)".

2) ...#task#1, status 200 vs 200 — "$.workloadDeploymentPatterns[0]: expected {'deploymentPatternName': 'ecs-deployment', 'deploymentPatternVersionName': '8.1.1', 'workloadVersionName': '8.1.1', 'displayName': 'ECS architecture deployment', 'description': 'ECS Architecture deployment for Dynamic Image Transformation for Amazon CloudFront.'}, got {'deploymentPatternName': 'ecs-deployment', 'workloadVersionName': '7.0.0', 'deploymentPatternVersionName': '7.0.0', 'displayName': 'Image transformation on Amazon ECS', 'description': \"Deploys the image transformation solution's services as tasks on an Amazon ECS cluster.\"}" (same for lambda-deployment).

3) ...#task#2 — expected adAwsManagedExistingVpc/adAwsManagedNewVpc/adOnPremiseExistingVpc at 1.0.0 for workloadName MicrosoftActiveDirectory; got ADExtensionExistingVPC/ADExtensionNewVPC at 1.0.5 (six indices diverge, $.workloadDeploymentPatterns[0]..[5]).

4) ...#task#3 — workloadName SAP at 1.0.27: expected displayName 'SAP HANA database cross AZ high availability deployment' / description 'Deploy SAP HANA with high availability configured across two Availability Zones.'; got 'SAP HANA, high availability' / \"Deploys an SAP HANA database as a high-availability deployment across two Availability Zones.\" (eight indices diverge).

5) ...#task#4 — workloadName SO0062: expected ALB-ECS/Default/Headless at 4.3.0; got existing-vpc-deployment/fargate-deployment at 3.3.0.

6) launch-wizard-catalog-list-workloads#task#1, ListWorkloads, status 200 vs 200 — "$.workloads[0]: expected {'workloadName': 'MicrosoftActiveDirectory', 'displayName': 'Microsoft Active Directory', 'description': 'AWS Launch Wizard for Active Directory guides you through sizing, configuration, and deployment of Microsoft Active Directory (AD) workloads on EC2.', 'documentationUrl': 'https://docs.aws.amazon.com/launchwizard/latest/userguide/launch-wizard-active-directory-landing.html', 'workloadOwnerDisplayName': 'AWS Launch Wizard', 'status': 'ACTIVE', 'accountConstraints': []}, got {'workloadName': 'MicrosoftActiveDirectory', 'displayName': 'Microsoft Active Directory', 'status': 'ACTIVE', 'accountConstraints': []}"; $.workloads[1] expected SO0023 but got SAP, and all seven indices $.workloads[0]..[6] diverge.

Code: services/service-com-amazonaws-launchwizard-launchwizard/src/state.rs — `CATALOGUE` static (line 2492), `seeded_workloads()` (line 2786), `Workload::to_summary()` (line 630, four members only); services/service-com-amazonaws-launchwizard-launchwizard/src/ops/list_workloads.rs line 155.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay fixture launch-wizard-catalog-list-workload-deployment-patterns: step task#5 (POST /listWorkloadDeploymentPatterns with workloadName "SQL") answers 404 ResourceNotFoundException instead of 200 with the SQL pattern list; task#6 and task#7 do the same for the other workloads the recording answers 200 for.
3. Replay steps task#1..task#4 of the same fixture (workloadName SO0023, MicrosoftActiveDirectory, SAP, SO0062): every returned workloadDeploymentPatterns entry diverges in deploymentPatternName and/or workloadVersionName, deploymentPatternVersionName, displayName and description.
4. Replay fixture launch-wizard-catalog-list-workloads step task#1 (POST /listWorkloads): the response holds the emulator's five seeded rows in workloadName order with only workloadName/displayName/status/accountConstraints, against seven recorded entries each carrying description, documentationUrl and workloadOwnerDisplayName.
