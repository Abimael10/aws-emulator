Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.eb-ebextensions-config-files-really-run-on-the-instance elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.eb-ebextensions-config-files-really-run-on-the-instance|_http_until#24|Timeout|last = _http_probe(ctx, url, timeout=20.0)
Issue-Key: elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers|_http_get#44|Timeout|with urllib.request.urlopen(req, timeout=timeout) as r:
Issue-Commit: 2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92

# Elastic Beanstalk environment data plane: .ebextensions files:/container_commands: never execute on the instance, and SwapEnvironmentCNAMEs does not move which application answers (2 causes)

> _Note: this issue was identified against the code base at commit `2c5dcf2b9a89c9dd56a9400ba366d007bab5eb92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SwapEnvironmentCNAMEs

## Description
Two net-new root causes in the Elastic Beanstalk environment data plane. Both failing checks run to the harness cap ("Check timed out after 60.0s") because a data-plane poll they issue never satisfies; neither was reported as owned by an existing issue by this replay. Related open issues describe neighbouring defects but are named here only for context, not as cover: 20261107T192913Z-elasticbeanstalk-elastic-beanstalk-environments-have-no-data-plane-endpointurl.md (no data plane / EndpointURL never published), 20261112T072923Z-elasticbeanstalk-elastic-beanstalk-publishes-cnames-that-are-not-dns.md (un-served CNAMEs), and 20261121T141417Z-elasticbeanstalk-elastic-beanstalk-environments-have-no-running-application-process.md (no persistent application process). Two distinct causes must be closed.

Cause 1 of 2: the source bundle's `.ebextensions/*.config` `files:` and `container_commands:` sections never execute on the environment's instances.
AWS: recorded behavior only — the Smithy model (CreateApplicationVersion: ApplicationName/VersionLabel required @length 1-100, SourceBundle S3Location, Process boolean; errors S3LocationNotInServiceRegionException 400, InsufficientPrivilegesException 403) models only the control-plane call, and says nothing about bundle provisioning. AWS's documented deployment behavior, and what the recording shows, is that a `.ebextensions/*.config` in the uploaded source bundle is interpreted during deployment: each `files:` entry is written to the declared on-instance path with exactly the declared content and declared mode/owner/group, and each `container_commands:` shell command really runs on the instance, so its side-effect file exists on the instance filesystem afterwards. A path no section of the config mentions does not exist. The variable parts are the paths and contents named by the config and the tokens minted by the request; the requirement is the rule, not any one run's token values.
Emulator: the environment's instance filesystem never shows either artefact. The check deploys a version whose bundle carries a config with one `files:` entry and one `container_commands:` entry, reaches a Ready environment, confirms the negative control (an unmentioned path answers 404 through the on-instance file reader), and then polls the file reader for the `files:` path and for the container_command's output path for a 200. Neither ever becomes readable, so the check never advances past that poll and is cut off at the harness cap.
Why: bundle-carried provisioning is a silent no-op. A client whose deployment depends on writing config files, installing hooks, or running build/migration commands sees the deployment report success while the instance is not actually configured, so it cannot distinguish "my .ebextensions is wrong" from "the platform never ran it".
Fix: the application-version deployment path in the elasticbeanstalk service — unpacking a source bundle must parse `.ebextensions/*.config` and apply its `files:` and `container_commands:` sections against the simulated instance filesystem before the web process is started.
Traces: elasticbeanstalk.eb-ebextensions-config-files-really-run-on-the-instance.

Cause 2 of 2: SwapEnvironmentCNAMEs exchanges nothing in the data plane — the served content does not follow the CNAME.
AWS: Smithy: SwapEnvironmentCNAMEs, input SwapEnvironmentCNAMEsMessage {SourceEnvironmentId, SourceEnvironmentName @length 4-40, DestinationEnvironmentId, DestinationEnvironmentName @length 4-40}, output empty (Unit), no modeled errors; recorded behavior only for the data-plane consequence. After the swap each environment's published CNAME is exactly exchanged AND requests to the first CNAME reach the SECOND environment's application and vice versa, while each environment's own EndpointURL still reaches its own application, the EndpointURLs do not move, neither environment leaves Ready/Updating, and neither goes non-Green.
Emulator: the bytes do not follow the swap. The check launches two environments serving two distinct minted nonces, establishes the baseline that each CNAME serves its own nonce, calls SwapEnvironmentCNAMEs, and then polls each CNAME for the OTHER environment's nonce. That condition never holds and the check is cut off at the harness cap, so the request routing that maps a published CNAME to an environment's application was not exchanged with the CNAME record.
Why: blue/green cut-over — the reason SwapEnvironmentCNAMEs exists — cannot be exercised at all. Traffic never moves, so a client's cut-over test either hangs or passes for reasons unrelated to its own code, and a rollback-by-swap-back is equally unobservable.
Fix: the SwapEnvironmentCNAMEs handler together with the environment data-plane request router — the CNAME-to-environment route must be exchanged atomically with the CNAME values the control plane reports.
Traces: elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers.

## Evidence
Cause 1: [mismatch] elasticbeanstalk.eb-ebextensions-config-files-really-run-on-the-instance — Error: Check timed out after 60.0s. Expected: GET http://<cname>/sim2real/file?<files-path> returns 200 with exactly the content declared by the config's `files:` section, and GET http://<cname>/sim2real/file?<cmd-path> returns 200 with the token the `container_commands:` entry wrote, while the never-written control path returns 404 both before and after. Actual: the control path correctly 404s, but neither declared path ever returns 200 within the check's 300s-per-path poll, so the check never reaches its assertions and is killed at the 60s harness cap.

Cause 2: [mismatch] elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers — Error: Check timed out after 60.0s. Expected after eb.swap_environment_cnames(SourceEnvironmentName=<env-a>, DestinationEnvironmentName=<env-b>): GET http://<cname-a>/ returns 200 'sim2real-nonce=<nonce-b>' and GET http://<cname-b>/ returns 200 'sim2real-nonce=<nonce-a>' (nonces are per-run values filling the template), each environment's own EndpointURL still returns its own nonce, Health stays Green and Status stays in {Ready, Updating}. Actual: the post-swap poll on both CNAMEs never observes the other environment's nonce within its 900s budget; the check is killed at the 60s harness cap. The swap moved no traffic.

## Steps to Reproduce
Cause 1:
1. CreateApplication, then CreateStorageLocation and PutObject a source bundle containing a Procfile (`web: python3 -u server.py`), a stdlib HTTP server that exposes a route reading an arbitrary on-instance path (200 with its contents, 404 on OSError), and `.ebextensions/sim2real.config` declaring one `files:` entry (a /tmp path, mode 000644, a minted content token) and one `container_commands:` entry (`printf %s <token> > /tmp/<other-path>`).
2. CreateApplicationVersion with Process=true and wait for Status PROCESSED.
3. CreateEnvironment with that VersionLabel and a CNAMEPrefix; wait for Status Ready.
4. GET http://<cname>/sim2real/file?<a-path-the-config-never-mentions> — correctly 404.
5. GET http://<cname>/sim2real/file?<files-path> and .../?<container-command-output-path> — neither ever returns 200.

Cause 2:
1. CreateApplication; publish two PROCESSED application versions whose bundles serve two distinct minted nonces.
2. CreateEnvironment twice (env-a with version-a and CNAMEPrefix env-a, env-b with version-b and CNAMEPrefix env-b); wait for both Ready.
3. Confirm the baseline: GET http://<cname-a>/ serves nonce-A and GET http://<cname-b>/ serves nonce-B.
4. Call SwapEnvironmentCNAMEs(SourceEnvironmentName=env-a, DestinationEnvironmentName=env-b).
5. GET http://<cname-a>/ and http://<cname-b>/ — each still serves its original nonce (or nothing); the served content never trades places.
