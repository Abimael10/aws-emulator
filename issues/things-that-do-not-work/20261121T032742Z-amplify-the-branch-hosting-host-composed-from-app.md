Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: amplify.manual-deployment-job-runs-two-hosting-steps-and-pads-the-job-arn
Issue-Shape-Id: com.amazonaws.amplify#Amplify
Issue-Key: amplify.manual-deployment-job-runs-two-hosting-steps-and-pads-the-job-arn|_http#16|PublishedUrlRejected|target_url, route_headers = ctx.endpoint_route(url)
Issue-Commit: d5ceaf476328a7e82fef92afef1bcc0e0278d2e9

# amplify: the branch hosting host composed from App.defaultDomain (<displayName>.<appId>.localhost) is not announced as one of the emulator's own hosts, so a manual deployment's published page is unreachable

> _Note: this issue was identified against the code base at commit `d5ceaf476328a7e82fef92afef1bcc0e0278d2e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetApp

## Description
One net-new cause, grounded in the behavioral check amplify.manual-deployment-job-runs-two-hosting-steps-and-pads-the-job-arn. The job-record half of that check now passes end to end (jobId "1" in CreateDeployment/ListJobs, jobArn padded to .../branches/main/jobs/0000000001, activeJobId "0000000001", totalNumberOfJobs "0", steps DEPLOY+VERIFY both SUCCEED); the check stops at its last step, the GET of the branch's hosting URL built from the API's own output.

Related already-open issues, named for the fixer, none of which carries this check id: 20261111T130904Z-amplify-2-net-new-causes-app-defaultdomain-is--r2.md (same family of cause, but its Cause 1 describes the now-drifted value 'localhost:<port>' and its Cause 2 lists four other amplify checks), 20261109T143214Z-amplify-s3-every-s3-sourced-deployment-check-stops.md (same family under the amplify__s3 heading only), and 20261110T142528Z-amplify-3-net-new-causes-getjob-reports-a.md (the WIRE corpus, describing the pre-fix <appId>.amplifyapp.com value). An open issue that looks similar does not cover this check, so it is filed here with its own replay key.

Cause 1 of 1: the host a client composes from the two members Amplify documents for that purpose — `<Branch$displayName>.<App$defaultDomain>` — is not a host the emulator serves or announces as its own, so the request never reaches the emulator and the whole branch hosting data plane is unobservable even for a deployment the control plane reports as SUCCEED.
  AWS: Smithy (services/service-com-amazonaws-amplify-amplify/smithy-model/com-amazonaws-amplify-amplify.json) models GetApp -> GET /apps/{appId}, 200 with `app` @required -> App, and GetBranch -> GET /apps/{appId}/branches/{branchName}, 200 with `branch` @required -> Branch. `App$defaultDomain` is documented "The default domain for the Amplify app." and `Branch$displayName` (@required) "The display name for the branch. This is used as the default domain prefix." The model fixes no pattern for either, so the reachability contract is recorded behavior only: on AWS the composed address `https://<displayName>.<defaultDomain>/` is answered by Amplify's hosting edge — after a SUCCEED manual deployment a GET of `/` returns the published bytes with HTTP 200 (and 404 for a path the deployment does not contain). The requirement is that the address the API hands a client be reachable by that client; the concrete host labels, zone and port are per-run environment values and are never the requirement.
  Emulator: GetApp reports `defaultDomain` as `<appId>.localhost` (the loopback zone, composed by src/state/hosting_address.rs `announced_default_domain` from `announced_zone_of_authority`), and GetBranch reports `displayName` `main`, so a client composes `https://main.<appId>.localhost/`. That address is refused before any request is sent: `simparity.published.PublishedUrlRejected: endpoint_url was given https://main.d0j9hgbq7z0rey.localhost/ (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener`. (The app-id label `d0j9hgbq7z0rey` and the port are per-run values; the defect is the host not being one the emulator announces.) The crate only compares an INBOUND Host against `hosted_branch_host`; nothing registers `<displayName>.<defaultDomain>` as a host the emulator serves on, and the only address the edge is actually reachable at is the private path form `/_aws/amplify/<appId>/<displayName>` — an address no AWS response ever names.
  Why: a client that completed CreateDeployment -> PUT zip -> StartDeployment -> GetJob SUCCEED has no way to confirm what the job published, because the one address the API gives it for the site leaves the emulator entirely. Every hosting-plane behavior a client exercises through that address (published bytes, 404 for an unpublished path, redirect rules, custom headers, branch basic auth) is unobservable, and a client that follows AWS's documented composition gets a DNS/connection failure where AWS returns 200.
  Fix: the hosting edge addressing in this crate — src/state/hosting_address.rs (`announced_default_domain`, `hosted_branch_host`, `hosted_site_host`) together with the edge that claims hosting requests in src/service.rs — must announce every branch host it serves as one of the emulator's own hosts: register `<displayName>.<defaultDomain>` on a per-resource listener so `https://<displayName>.<defaultDomain>/<path>` routes back to the emulator that published it, and announce in `defaultDomain` the host of that listener.
  Traces: amplify.manual-deployment-job-runs-two-hosting-steps-and-pads-the-job-arn.

## Evidence
[mismatch] amplify.manual-deployment-job-runs-two-hosting-steps-and-pads-the-job-arn
  The check builds the URL exactly as a client would: `domain = get_app(appId)['app']['defaultDomain']; display = get_branch(appId, branch)['branch']['displayName']; url = f"https://{display}.{domain}/"`, then GETs it.
  Observed (replay output):
    File "context.py", line 616, in endpoint_route
      return self._published_target(url, "endpoint_url"), {}
    simparity.published.PublishedUrlRejected: endpoint_url was given https://main.d0j9hgbq7z0rey.localhost/ (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:34111/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
  The app-id label (d0j9hgbq7z0rey) and the port (34111) are per-run values, not part of the requirement.
  Expected: the address GetApp/GetBranch announce is served by the emulator — after the SUCCEED manual deployment, GET https://<displayName>.<defaultDomain>/ returns HTTP 200 with the deployed index.html bytes byte-for-byte, and a path the deployment does not contain returns 404.
  Everything the check asserts before that step matched AWS: jobId '1', jobArn ending '/branches/main/jobs/0000000001', steps [('DEPLOY','SUCCEED'),('VERIFY','SUCCEED')], branch activeJobId '0000000001', totalNumberOfJobs '0', ListJobs [('1','SUCCEED')].
  Source: services/service-com-amazonaws-amplify-amplify/src/state/hosting_address.rs:65-75 (`announced_default_domain` -> `format!("{app_id}.{zone}")`) and :103-164 (`hosted_branch_host`/`hosted_site_host` compare an inbound Host only; no listener claims that host).

## Steps to Reproduce
1. app_id = amplify.create_app(name=<unique>)['app']['appId']
2. amplify.create_branch(appId=app_id, branchName='main')
3. dep = amplify.create_deployment(appId=app_id, branchName='main'); PUT a zip containing index.html to dep['zipUploadUrl'] (expect HTTP 200); amplify.start_deployment(appId=app_id, branchName='main', jobId=dep['jobId']); poll amplify.get_job until summary.status == 'SUCCEED'.
4. domain = amplify.get_app(appId=app_id)['app']['defaultDomain']  -> '<appId>.localhost'
   display = amplify.get_branch(appId=app_id, branchName='main')['branch']['displayName'] -> 'main'
5. GET https://{display}.{domain}/
   Observed: the published host '<display>.<appId>.localhost' is not one of the emulator's own hosts, so the GET never reaches the emulator.
   Expected: HTTP 200 with the deployed index.html bytes; a path the deployment does not contain answers 404.

Replay: run_behavioral_suite and re-run amplify.manual-deployment-job-runs-two-hosting-steps-and-pads-the-job-arn.
