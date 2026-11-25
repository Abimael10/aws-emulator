Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked
Issue-Key: cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked|_poll_until#18|Timeout|last = probe()
Issue-Commit: 643d7fc40de53dee6cfee0ebba73d0a58d0d5505

# cloudfront__lambda: the CloudFront geo-restriction blocklist scenario never completes — the check exhausts its 60 s budget with no verdict

> _Note: this issue was identified against the code base at commit `643d7fc40de53dee6cfee0ebba73d0a58d0d5505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
Cause 1 of 1: the CloudFront distribution + Lambda-origin geo-restriction scenario does not complete against the emulator, so the blocklist enforcement assertion is never reached.

AWS:      recorded behavior only — the recording completes this scenario inside the suite's 60 s per-check budget. The graded behavior is CloudFront's documented geo-restriction contract: with a Restrictions.GeoRestriction of RestrictionType=blacklist naming a country, a viewer resolved to that country is refused AT THE EDGE with HTTP 403 and the origin is never contacted (the origin echoes a per-request nonce and the CloudFront-Viewer-Country header it was given, so a cached or fabricated answer cannot pass); a viewer outside the blocklist is served the origin's answer with the viewer-country header CloudFront added.

Emulator: the check reaches no verdict at all — reported as "Check timed out after 60.0s". The sequence (create the Lambda origin, CreateDistribution with the geo restriction, wait for the configuration to propagate — the check requires 3 consecutive identical edge answers per phase — and then fetch through the edge) does not finish inside the budget the recorded run fits in. The check also carries its own DATAPLANE_UNIMPLEMENTED marker for an edge data-plane gap, and not even that is produced.

Why:      A client relying on AWS's behavior gets an unbounded wait where AWS answers in seconds. Because the scenario never reaches its edge leg, the emulator's geo-restriction enforcement is entirely unverified: an edge that served blocked viewers, or that forwarded them to the origin, would be indistinguishable from this result.

Fix:      The CloudFront distribution lifecycle (CreateDistribution reaching a Deployed/servable state promptly) and the edge data plane that must evaluate Restrictions.GeoRestriction before forwarding to the origin. The timeout carries no stack frame, so the stalling call must be identified by replaying the listed check and timing each step.

Traces:   cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked.

## Evidence
Failing check cloudfront__lambda.deep-cloudfront-2-cf-edge-enforce-geo-restriction-blocklist-blocked, [mismatch]:

  Error: Check timed out after 60.0s

The check (GROUP=security, SERVICES=[cloudfront, lambda]) drives a Lambda origin that echoes the marker it was configured with, the method, the raw query string (carrying a per-request nonce), the CloudFront-added viewer-country header and the header NAMES it saw, and requires 3 consecutive identical edge answers before a phase counts as propagated.

Expected (AWS): the scenario completes within the 60 s budget; a viewer in a blacklisted country is answered HTTP 403 by the edge and the origin is never invoked (no echoed nonce), while an unblocked viewer is served the origin's body with the viewer-country header present.
Actual (emulator): no response/verdict at all inside 60 s — the check is killed by its budget, so neither the 403 nor the "origin not contacted" property is produced.

## Steps to Reproduce
1. Create the Lambda origin function (echoes marker, method, raw query string, viewer-country header and header names) and expose it as the distribution's origin.
2. CreateDistribution with Restrictions.GeoRestriction = {RestrictionType: "blacklist", Items: [<country>]}; wait for the distribution to be servable.
3. GET through the distribution with a viewer resolved to the blocked country, repeating until 3 consecutive identical answers.
   Expected: HTTP 403 from the edge, origin never invoked.
4. GET with a viewer outside the blocklist.
   Expected: HTTP 200 with the origin's echo and the CloudFront viewer-country header.
   Observed: the whole sequence does not complete within the 60 s check budget, so neither step is graded.
