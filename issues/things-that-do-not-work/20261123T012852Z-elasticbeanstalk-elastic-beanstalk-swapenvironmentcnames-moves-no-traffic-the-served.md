Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers|_http_until#24|Timeout|time.sleep(interval_s)
Issue-Commit: fda9c10fdfacd5c8a2bec05c9c210d2748ab313c

# Elastic Beanstalk SwapEnvironmentCNAMEs moves no traffic: the served content does not follow the exchanged CNAME (1 cause)

> _Note: this issue was identified against the code base at commit `fda9c10fdfacd5c8a2bec05c9c210d2748ab313c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SwapEnvironmentCNAMEs

## Description
One net-new root cause in the Elastic Beanstalk environment data plane. This replay did NOT report the failing check as owned by any open issue. The open issue 20261121T153828Z-elasticbeanstalk-elastic-beanstalk-environment-data-plane-ebextensions-files-container.md describes a neighbouring SwapEnvironmentCNAMEs data-plane defect and 20261107T192913Z-elasticbeanstalk-elastic-beanstalk-environments-have-no-data-plane-endpointurl.md / 20261121T141417Z-elasticbeanstalk-elastic-beanstalk-environments-have-no-running-application-process.md describe the surrounding missing data plane; they are named here for context only, not as cover, because the replay key of this failure is not carried by any of them.

Cause 1 of 1: SwapEnvironmentCNAMEs exchanges the CNAME values the control plane reports (or nothing at all) but does not exchange which environment's application answers a published CNAME, so the check's post-swap data-plane poll never satisfies and the check runs to the harness cap.

AWS: Smithy: SwapEnvironmentCNAMEs, input SwapEnvironmentCNAMEsMessage {SourceEnvironmentId: EnvironmentId, SourceEnvironmentName: EnvironmentName @length 4-40, DestinationEnvironmentId: EnvironmentId, DestinationEnvironmentName: EnvironmentName @length 4-40}, output Unit (empty), no modeled errors. The data-plane consequence is recorded behavior only: after the call, DescribeEnvironments reports each environment's CNAME exactly exchanged (env-A now holds what was env-B's CNAME and vice versa) AND an HTTP GET to the first CNAME reaches the SECOND environment's application while a GET to the second CNAME reaches the FIRST environment's application. At the same time each environment's own EndpointURL keeps reaching its own application, neither EndpointURL moves, Health stays Green throughout, and Status stays within {Ready, Updating} — that combination is what distinguishes a CNAME-layer swap from a redeploy. The variable parts (environment names, CNAME hostnames, the response bodies' per-run nonces) come from the request and the emulator's state; the requirement is the exchange rule, not any one run's values.
Emulator: the bytes never trade places. The check publishes two PROCESSED application versions whose bundles serve two distinct minted nonces, launches two Ready environments (env-A -> nonce-A, env-B -> nonce-B), establishes the baseline that each CNAME serves its own nonce, calls SwapEnvironmentCNAMEs(SourceEnvironmentName=env-A, DestinationEnvironmentName=env-B), and then polls each published CNAME for the OTHER environment's nonce. That condition is never observed, so the check never reaches its post-swap assertions and is cut off with 'Check timed out after 60.0s' at the harness cap — the CNAME-to-environment request route was not exchanged together with the CNAME value, and in this run the check does not even get as far as the frame recorded in the pre-existing issue, so the stall is reached by a different path.
Why: blue/green cut-over — the only reason SwapEnvironmentCNAMEs exists — cannot be exercised. A client that promotes a staging environment to production by swapping CNAMEs sees the control-plane call succeed while every request keeps landing on the old application, and a rollback-by-swapping-back is equally unobservable. Deployment tooling built on this pattern (eb deploy blue/green, CodePipeline Beanstalk swap stages) silently never cuts traffic over, and a client cannot tell that from its own misconfiguration.
Fix: the SwapEnvironmentCNAMEs handler in the elasticbeanstalk service together with the environment data-plane request router — the CNAME -> environment-application route must be exchanged atomically with the CNAME values the control plane reports, and both environments must keep serving over their own unswapped EndpointURL while Health stays Green and Status stays in {Ready, Updating}.
Traces: elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers.

## Evidence
Cause 1: [mismatch] elasticbeanstalk.eb-swap-environment-cnames-swaps-which-application-answers — Error: Check timed out after 60.0s.

Expected (AWS): after eb.swap_environment_cnames(SourceEnvironmentName=<env-a>, DestinationEnvironmentName=<env-b>):
  - DescribeEnvironments reports the CNAMEs exactly exchanged: (<cname-a>, <cname-b>) becomes (<cname-b>, <cname-a>).
  - GET http://<cname-a>/ -> 200 with body 'sim2real-nonce=<nonce-b>'.
  - GET http://<cname-b>/ -> 200 with body 'sim2real-nonce=<nonce-a>'.
  - GET http://<endpoint-url-a>/ -> 200 'sim2real-nonce=<nonce-a>' and GET http://<endpoint-url-b>/ -> 200 'sim2real-nonce=<nonce-b>' (the per-environment endpoints do NOT swap).
  - Over every observation during the swap: Health == 'Green' for both, Status in {'Ready','Updating'} for both, and EndpointURL unchanged for both.
  The hostnames and the 'sim2real-nonce=<nonce>' tokens are per-run values filling that template; the fixed part is the body shape 'sim2real-nonce=' plus the nonce the request's own bundle minted.

Actual (emulator): the post-swap poll `_http_until(ctx, 'http://<cname-a>/', lambda r: r[0] == 200 and <nonce-b> in r[1], timeout_s=900)` never observes the other environment's nonce; the equivalent poll on <cname-b> likewise never satisfies. The check makes no further progress and the harness kills it at its 60.0s cap, so no post-swap assertion is ever evaluated. The swap moved no traffic.

## Steps to Reproduce
1. CreateApplication(ApplicationName=<app>).
2. CreateStorageLocation, then PutObject two source bundles, each containing a Procfile ('web: python3 -u server.py') and a stdlib HTTP server that answers GET / with 200 'sim2real-nonce=<nonce>' for its own distinct minted nonce (nonce-A and nonce-B).
3. CreateApplicationVersion twice with Process=true; poll DescribeApplicationVersions until each Status is PROCESSED.
4. CreateEnvironment twice: env-A with VersionLabel=<version-a> and CNAMEPrefix=<env-a>, env-B with VersionLabel=<version-b> and CNAMEPrefix=<env-b>, EnvironmentType=SingleInstance. Poll DescribeEnvironments until both Status == 'Ready'; record each CNAME and EndpointURL.
5. Baseline: GET http://<cname-a>/ returns 200 'sim2real-nonce=<nonce-a>' and GET http://<cname-b>/ returns 200 'sim2real-nonce=<nonce-b>'.
6. Call SwapEnvironmentCNAMEs(SourceEnvironmentName=<env-a>, DestinationEnvironmentName=<env-b>).
7. GET http://<cname-a>/ and GET http://<cname-b>/ repeatedly — neither ever serves the other environment's nonce; the served content never trades places, and the check stalls here until the harness cap.
