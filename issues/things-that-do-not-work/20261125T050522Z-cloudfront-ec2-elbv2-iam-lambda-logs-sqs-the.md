Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation
Issue-Key: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation|_poll_until#40|Timeout|time.sleep(interval_s)
Issue-Commit: 5af22e124f1b4edcbe644b4b43fbf6a6331dae9c

# cloudfront__ec2__elbv2__iam__lambda__logs__sqs: the viewer-header and cache-hit scenario over an ALB Lambda-target origin reaches no verdict — the whole 60 s budget elapses with no edge answer

> _Note: this issue was identified against the code base at commit `5af22e124f1b4edcbe644b4b43fbf6a6331dae9c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One root cause, grounded in one failing behavioral check that the replay reports owned by no open issue and blocked by no unimplemented stub.

Cause 1 of 1: the CloudFront-over-ALB-over-Lambda-target data path never converges — the check is killed by its own per-check timeout with no assertion evaluated, so neither the viewer-header forwarding nor the cache-hit behavior the check exists to verify produces any observation.

AWS: recorded behavior only for the data plane (it is not in the Smithy model); the control plane at issue is cloudfront CreateDistribution over a CustomOriginConfig origin whose DomainName is an ALB DNS name, with elbv2 DescribeLoadBalancers reporting State.Code 'active' and cloudfront GetDistribution reporting Status 'Deployed'. On AWS, after those two convergence points: a GET at the distribution domain for a never-requested path is answered HTTP 200 with `X-Cache: Miss from cloudfront`, the ALB Lambda-target invocation sees the viewer's own Host (the distribution domain) when the origin request policy forwards all viewer headers, an `X-Forwarded-For` the ALB adds, `CloudFront-Viewer-Address` in AWS's `ip:port` form and `CloudFront-Viewer-Country` as a two-letter code; an identical second request is answered 200 with `X-Cache: Hit from cloudfront`, byte-identical body and NO new function invocation on the side channel; and a request made straight at the ALB DNS name without the origin's secret header gets the listener default's fixed 403 with its own body and no invocation. The hosts, ALB DNS name, secret value and body markers are per-run; the statuses, the X-Cache values, the header forms and which request does or does not invoke the function are the requirements.

Emulator: the check produces no verdict at all — the whole replay detail is `Error: Check timed out after 60.0s`. Its bounded convergence polls are DescribeLoadBalancers until State.Code=='active' (600 s), GetDistribution until Status=='Deployed' (1200 s) and then a warm-up GET at the distribution domain until a 200 (300 s at 10 s intervals); all of them exceed the harness cap without producing an answer, so no status line, no forwarded header and no SQS side-channel message is ever observed. Hypothesis (the timeout alone does not name the stalling call): the same edge-never-serves condition this run shows directly in the sibling headings — a Deployed distribution whose published domain either refuses connections or never answers 200 — leaves the warm-up poll spinning.

Why: a client that fronts an ALB Lambda target with CloudFront can verify none of this against the emulator: not that the edge forwards the viewer Host and the CloudFront-Viewer-* headers it was configured to forward, not that a cache hit spares the origin an invocation, and not that the ALB's secret-header gate keeps direct viewers out. Worse, the call never fails with a diagnosis — a caller waiting on an edge response hangs until a budget ends.

Fix: the CloudFront edge data plane (a Deployed distribution must accept viewer requests at its published domain, fetch from the CustomOriginConfig origin, serve Miss/Hit with the X-Cache header and inject the forwarded viewer headers) together with whatever leaves the ALB short of 'active' or the distribution short of 'Deployed' in this combination. Instrumenting which of the three polls does not converge is the first step.

Traces: cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation (the heading's only failing check). Related but non-covering OPEN issue: 20261124T213515Z-cloudfront-elbv2-a-distribution-over-an-alb-custom.md describes the same edge-never-serves condition over an ALB origin but carries only cloudfront__elbv2 check ids, so this check is unowned. The resolved issues 20261117T115022Z-cloudfront-ec2-elbv2-iam-lambda-logs-sqs-cloudfront-forwarding-all-viewer-headers-rewrites-host-to.md and 20261119T232925Z-cloudfront-ec2-elbv2-iam-lambda-logs-sqs-the-cloudfront-viewer-address-header-forwarded-to-the.md are grounded in this same check and record the edge then answering with forwarded headers; their subjects are now unobservable because nothing is answered at all. Nothing here is refused with an HTTP status or error code, so this is filed as its own cause rather than described as a refusal regression.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (937/1144 passed; bootstrap_signal false; every service in this check's heading is bootstrapped). Per-service rate: cloudfront__ec2__elbv2__iam__lambda__logs__sqs 0/1 (0%).

Failing check `cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation`, verdict mismatch, with the entire detail being:
  `Error: Check timed out after 60.0s`
No assertion message, no HTTP status, no error code — the scenario reached no verdict.

Expected (AWS), from the check body: GET <distribution-domain>/sim2real/<marker> -> 200, `X-Cache: Miss from cloudfront`, body whose `headers.host` equals the distribution domain, `x-forwarded-for` present, `cloudfront-viewer-address` matching ^[0-9a-fA-F.:]+:\d+$ and `cloudfront-viewer-country` matching ^[A-Z]{2}$, plus one side-channel SQS message carrying that invocation id; the identical repeat -> 200, `X-Cache: Hit from cloudfront`, byte-identical body and zero further messages for that path; GET http://<alb-dns><path> directly -> 403 with the listener default's body and zero invocations.
Actual (emulator): none of it was observed; the check's convergence polls (ALB 'active' 600 s, distribution 'Deployed' 1200 s, first 200 at the edge 300 s) never completed inside the 60 s harness cap.

## Steps to Reproduce
1. Create a VPC with two public subnets in two AZs and a security group for the listener port; create an internet-facing ipv4 ALB with an HTTP:80 listener whose default action is a fixed-response 403 and a priority rule matching a secret http-header that forwards to a Lambda-target group; the function echoes its event (path, headers, a fresh invocation id) and also posts it to an SQS queue.
2. cloudfront CreateDistribution with a single CustomOriginConfig origin over the ALB DNS name, the secret header as an Origin CustomHeader, an origin request policy that forwards all viewer headers plus CloudFront-Viewer-Address and CloudFront-Viewer-Country, and a caching cache policy.
3. Poll elbv2 DescribeLoadBalancers until State.Code == 'active' and cloudfront GetDistribution until Status == 'Deployed'; record which (if either) converges.
4. GET <distribution domain>/sim2real/warmup until it answers 200 — confirm whether the edge ever answers at all, and with what.
5. Then GET a fresh path: expect 200 + `X-Cache: Miss from cloudfront`, the forwarded viewer Host/X-Forwarded-For/CloudFront-Viewer-Address (ip:port)/CloudFront-Viewer-Country (2 letters) in the function's event, and one SQS message; repeat the identical request: expect 200 + `X-Cache: Hit from cloudfront`, the same body and no new SQS message; then GET the ALB DNS name directly with no secret header: expect the listener's fixed 403 and no invocation.
6. Re-replay cloudfront__ec2__elbv2__iam__lambda__logs__sqs.alb-lambda-target-sees-viewer-headers-and-cache-hit-skips-invocation and confirm it completes with a verdict.
