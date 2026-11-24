Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.eb-restart-app-server-restarts-the-process-without-replacing-the-instance elasticbeanstalk.eb-enhanced-health-degrades-when-the-application-returns-http-500 elasticbeanstalk.eb-abort-environment-update-withholds-the-instance-replacement elasticbeanstalk.eb-ebextensions-option-setting-loses-to-the-direct-environment-setting elasticbeanstalk.eb-ebextensions-config-files-really-run-on-the-instance elasticbeanstalk.eb-saved-configuration-template-reproduces-its-option-in-a-new-environment elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.eb-restart-app-server-restarts-the-process-without-replacing-the-instance|run#18|AssertionError|assert counts == sorted(counts) and len(set(counts)) == 3, (
Issue-Key: elasticbeanstalk.eb-enhanced-health-degrades-when-the-application-returns-http-500|run#32|AssertionError|assert broken[0] == 500, (
Issue-Key: elasticbeanstalk.eb-abort-environment-update-withholds-the-instance-replacement|run#33|AssertionError|assert _boot()["boot"] == boot_before, (
Issue-Key: elasticbeanstalk.eb-ebextensions-option-setting-loses-to-the-direct-environment-setting|run#20|AssertionError|assert seen.get("SIM2REAL_EB_EXTONLY") == extonly_value, (
Issue-Key: elasticbeanstalk.eb-ebextensions-config-files-really-run-on-the-instance|_http_get#24|Timeout|with urllib.request.urlopen(req, timeout=timeout) as r:
Issue-Key: elasticbeanstalk.eb-saved-configuration-template-reproduces-its-option-in-a-new-environment|_http_get#32|Timeout|with urllib.request.urlopen(req, timeout=timeout) as r:
Issue-Key: elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers|_http_get#45|Timeout|with urllib.request.urlopen(req, timeout=timeout) as r:
Issue-Commit: 8cbf0dcd0020846f13fa76c0319ab04fca2e29a2

# Elastic Beanstalk environments have no running application process: per-request state is lost, .ebextensions never executes, template options never reach the process, and a CNAME swap does not move the served content (6 causes)

> _Note: this issue was identified against the code base at commit `8cbf0dcd0020846f13fa76c0319ab04fca2e29a2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RestartAppServer

## Description
Six net-new root causes in the Elastic Beanstalk environment data plane. Related open issues describe the *absence* of a data plane and un-served CNAMEs (20261107T192913Z-elasticbeanstalk-elastic-beanstalk-environments-have-no-data-plane-endpointurl.md, 20261112T072923Z-elasticbeanstalk-elastic-beanstalk-publishes-cnames-that-are-not-dns.md); none of them covers these: here something DOES answer at the environment address and serves the bundle's nonce, but it is not a persistent process running the deployed source bundle. Six distinct causes must be closed.

Cause 1 of 6: the environment serves no persistent application process — state written by one request is invisible to the next.
AWS: an Elastic Beanstalk environment runs the source bundle's web process (here a Procfile `web: python3 -u server.py`) as one long-lived OS process on the instance. Consecutive HTTP requests to the environment CNAME therefore reach the SAME process: an in-process counter increments monotonically across requests, and a route that flips in-process state (`/sim2real/fail-on`) changes what subsequent `GET /` requests return (500 instead of 200). Smithy: RestartAppServer (input EnvironmentName/EnvironmentId, empty output) is the only API that is documented to restart that process; nothing else does. Recorded behavior only for the data-plane semantics.
Emulator: every request is served independently of the last. Three consecutive reads of the bundle's request counter returned [1, 1, 1] instead of strictly increasing values, and after `GET /sim2real/fail-on` succeeded a following `GET /` still answered (200, 'sim2real-nonce=<nonce>') instead of (500, 'degraded <nonce>').
Why: a client cannot use the emulator to exercise anything that depends on application state surviving between requests — session state, warm caches, in-process counters — and cannot drive an application into a failing state to observe health degradation, deployment rollback or alarm behavior. RestartAppServer also becomes unobservable: there is no process state to reset, so a restart cannot be distinguished from no-op.
Fix: the environment data-plane runner in the elasticbeanstalk service — it must start the deployed bundle's web command once per instance and keep that process alive across requests, rather than synthesizing a response per request.
Traces: elasticbeanstalk.eb-restart-app-server-restarts-the-process-without-replacing-the-instance, elasticbeanstalk.eb-enhanced-health-degrades-when-the-application-returns-http-500.

Cause 2 of 6: an aborted environment update restarts the application process.
AWS: Smithy: AbortEnvironmentUpdate (input EnvironmentName/EnvironmentId, empty output, error InsufficientPrivilegesException 403). Aborting an in-flight update withholds the pending change: the instances that had not yet been replaced keep running the process they were running, so the per-process boot identity the application mints at start-up is unchanged after the abort.
Emulator: after AbortEnvironmentUpdate the application's boot identity had changed (from one minted value to a different one), i.e. the process was replaced/restarted even though the update was aborted.
Why: a client validating a rollback/abort path sees the instance replacement it explicitly aborted, so in-process state and warm instances are lost where real AWS keeps them; abort looks like "apply anyway".
Fix: the AbortEnvironmentUpdate handler and the environment update state machine — abort must leave the already-running instances and their processes untouched.
Traces: elasticbeanstalk.eb-abort-environment-update-withholds-the-instance-replacement.

Cause 3 of 6: option settings declared only in the bundle's .ebextensions never reach the running process environment.
AWS: an `option_settings:` block in a `.ebextensions/*.config` file in the source bundle is merged into the environment's configuration at deploy time, so a variable in namespace `aws:elasticbeanstalk:application:environment` set only in .ebextensions is present in the application process's environment (a value set directly by API for the same option wins, but the .ebextensions-only option still applies).
Emulator: the running application reported only the API-set variable ({'SIM2REAL_EB_PRECEDENCE': 'from-api-<v>'}); the variable set only in .ebextensions (SIM2REAL_EB_EXTONLY) was absent entirely.
Why: every configuration that an application carries in its own bundle is silently dropped, so a client cannot test bundle-carried configuration at all, and cannot observe the documented precedence between API-set and bundle-set options.
Fix: the deployment path that unpacks an application version — it must parse `.ebextensions/*.config` `option_settings:` and merge them into the environment configuration and the process environment.
Traces: elasticbeanstalk.eb-ebextensions-option-setting-loses-to-the-direct-environment-setting.

Cause 4 of 6: the .ebextensions `files:` and `container_commands:` sections never execute on the instance.
AWS: `.ebextensions/*.config` `files:` entries are written to the declared on-instance paths with exactly the declared content, and `container_commands:` shell commands really run during deployment, so their side-effect files exist on the instance filesystem afterwards; a path no config ever wrote does not exist.
Emulator: neither artefact ever appears. The check polls the on-instance file reader for the `files:` path and for the container_command's output path and never gets a 200, so it runs to the harness cap and the check is reported as "Check timed out after 60.0s".
Why: bundle-carried provisioning — writing config files, installing hooks, running build/migration commands — is a no-op in the emulator, so deployments that depend on it appear to succeed while the instance is not actually configured.
Fix: the deployment path for an application version — execute the `.ebextensions` `files:` and `container_commands:` sections against the simulated instance filesystem before the web process is started.
Traces: elasticbeanstalk.eb-ebextensions-config-files-really-run-on-the-instance.

Cause 5 of 6: an environment created from a saved configuration template alone never applies the template's environment option to the running process.
AWS: CreateEnvironment with only TemplateName (no SolutionStackName, no OptionSettings) takes its platform AND its option settings from the saved configuration, so an `aws:elasticbeanstalk:application:environment` option stored in the template is present in the new environment's application process environment; the template is a snapshot, so a later UpdateConfigurationTemplate does not change the running environment.
Emulator: the check never gets past reading the new environment's process environment — the poll for the application reporting its own environment never satisfies, and the check hits the harness cap ("Check timed out after 60.0s"). Hypothesis: with no persistent process (Cause 1) and no template-sourced option propagation, the environment never serves the option, so the snapshot semantics below are never reached either.
Why: a client that stores configuration in saved templates and launches environments from them gets an environment whose configuration is not actually in effect, and cannot verify the snapshot (non-live-binding) behavior AWS documents.
Fix: CreateEnvironment's template resolution plus the data-plane process environment — template option settings must be materialized into the launched environment's configuration and its application process.
Traces: elasticbeanstalk.eb-saved-configuration-template-reproduces-its-option-in-a-new-environment.

Cause 6 of 6: SwapEnvironmentCNAMEs does not move which application answers at each CNAME.
AWS: Smithy: SwapEnvironmentCNAMEs (input SourceEnvironmentName/Id + DestinationEnvironmentName/Id, empty output). After the swap each environment's CNAME is exchanged AND requests to the first CNAME reach the second environment's application (and vice versa), while each environment's own EndpointURL still reaches its own application and neither environment leaves Ready/Updating or goes non-Green.
Emulator: the check never completes — after the swap it polls the two CNAMEs for the OTHER environment's nonce and the condition never holds, so the check hits the harness cap ("Check timed out after 60.0s"). The served bytes do not follow the swap.
Why: blue/green deployment — the main reason SwapEnvironmentCNAMEs exists — cannot be exercised: traffic never moves, so a client's cut-over test passes or hangs for reasons unrelated to its own code.
Fix: the SwapEnvironmentCNAMEs handler together with the data-plane request router — the route that maps a published CNAME to an environment's application must be exchanged with the CNAME record.
Traces: elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers.

## Evidence
Cause 1: AssertionError: the in-memory request counter must strictly increase across three reads, otherwise its reset below proves nothing; the process reported [1, 1, 1]. AssertionError: after the toggle the application must really return 500, got (200, 'sim2real-nonce=nonce-aa2edc558ebd').
Cause 2: AssertionError: an aborted update must not have restarted the application process; its boot identity was '17c6c901f03c4b17b8c697fa6bd2c519' and is now 'ad4af83b0a614d31ad5181de49311c20'.
Cause 3: AssertionError: the positive control: an option set ONLY in .ebextensions must reach the running process ... The application reported {'SIM2REAL_EB_PRECEDENCE': 'from-api-7ed4af2b8b96'}, expected SIM2REAL_EB_EXTONLY == 'ext-only-5c613fd7da58'.
Cause 4: Error: Check timed out after 60.0s — the check polls http://<cname>/sim2real/file?<files_path> and ?<cmd_path> for a 200 (300s poll each); neither path ever becomes readable, while the never-written control path correctly 404s.
Cause 5: Error: Check timed out after 60.0s — poll on http://<cname>/sim2real/env returning 200 (expected exactly {'SIM2REAL_EB_FROM_TEMPLATE': '<value>'}).
Cause 6: Error: Check timed out after 60.0s — after eb.swap_environment_cnames(...) the poll for http://<cname_a>/ serving nonce-B and http://<cname_b>/ serving nonce-A never satisfies.

## Steps to Reproduce
1. Create an Elastic Beanstalk application and an application version whose source bundle is a Procfile + stdlib HTTP server that (a) mints a boot id once per process, (b) counts requests in memory, (c) exposes /sim2real/fail-on to make subsequent GET / return 500, (d) echoes SIM2REAL_* environment variables, (e) reads an on-instance path.
2. CreateEnvironment with that version and wait for Ready, then GET the environment CNAME three times at /sim2real/boot: the hit counter stays 1 instead of increasing (Cause 1).
3. GET /sim2real/fail-on, then GET /: still 200 with the healthy body instead of 500 (Cause 1).
4. Start an UpdateEnvironment that replaces instances, call AbortEnvironmentUpdate, then re-read /sim2real/boot: the boot id changed (Cause 2).
5. Deploy a version whose bundle contains .ebextensions with an option_settings entry for aws:elasticbeanstalk:application:environment and with files:/container_commands: sections; read /sim2real/env and the declared on-instance paths: the variable is missing and the paths never become readable (Causes 3 and 4).
6. CreateConfigurationTemplate carrying an aws:elasticbeanstalk:application:environment option, then CreateEnvironment with only TemplateName; read /sim2real/env (Cause 5).
7. Launch two environments serving distinct nonces, call SwapEnvironmentCNAMEs, then GET each CNAME (Cause 6).
