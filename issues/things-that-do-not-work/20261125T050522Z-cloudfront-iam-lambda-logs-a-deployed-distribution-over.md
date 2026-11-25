Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502
Issue-Key: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502|_fetch_ok#42|AssertionError|raise AssertionError(f"{DATAPLANE_MARKER}: {exc}") from exc
Issue-Commit: 5af22e124f1b4edcbe644b4b43fbf6a6331dae9c

# cloudfront__iam__lambda__logs: a Deployed distribution over a Lambda function-URL origin never answers at its edge, so the ErrorCachingMinTTL behavior is unobservable

> _Note: this issue was identified against the code base at commit `5af22e124f1b4edcbe644b4b43fbf6a6331dae9c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDistribution

## Description
One root cause, grounded in one failing behavioral check that the replay reports owned by no open issue and blocked by no unimplemented stub.

Cause 1 of 1: a distribution the control plane accepted and reports as Status=Deployed serves nothing at the address its published DomainName resolves to — the reachability control (a GET of a path the origin answers 200) never gets an answer at all within 240 s — so every error-caching behavior behind it is unobservable.

AWS: the CloudFront data plane is not in the Smithy model, so this is recorded behavior only; the control plane at issue is cloudfront CreateDistribution/GetDistribution with DistributionConfig$CustomErrorResponses -> CustomErrorResponse{ErrorCode, ErrorCachingMinTTL}. On AWS, once Status=Deployed the distribution's domain serves viewer requests to the configured origin (here a Lambda function URL as a custom origin): a path the origin answers 200 is served 200 with the origin's own body; a 502 from the origin with no Cache-Control header is passed through to the viewer with the origin's status and body and then held in the edge cache for the configured ErrorCachingMinTTL, so an immediate repeat returns the byte-identical stored body and a request after the TTL elapses returns a freshly generated one; a 502 whose origin sends `Cache-Control: no-store` is never cached, so every repeat reaches the origin and returns a different body. The distribution domain, host, port and body markers are per-run; the statuses, which body is served and the caching durations are the requirements.

Emulator: the edge answers nothing. The check's reachability poll for the first 200 fails with the check's own data-plane marker: `DATAPLANE_UNIMPLEMENTED: timed out waiting for 200 from https://<host>/_aws/cloudfront/<distribution-id>/ok after 240.0s (last: None)` — `last: None` means every one of the ~48 probes raised a transport error rather than returning any HTTP response, so there is no status, no header and no body. Everything before that point succeeded: CreateFunctionUrlConfig returned an absolute https origin URL, the function reached State=Active, CreateDistribution echoed CustomErrorResponses as {502: 10} unchanged, and GetDistribution reached Status=Deployed.

Why: a client cannot observe any CloudFront error-caching behavior in the emulator — not the pass-through of an origin 502, not that ErrorCachingMinTTL holds a header-less error response, not that `Cache-Control: no-store` defeats that hold, and not the expiry. A distribution that reports itself Deployed while serving nothing also misleads any caller that treats Deployed as readiness: it gets a transport failure, not an HTTP condition it can classify or retry on.

Fix: the CloudFront edge data plane for this origin type — a Deployed distribution's published domain must accept viewer requests and fetch from the Lambda function-URL custom origin, returning the origin's status and body; the CustomErrorResponses/ErrorCachingMinTTL cache logic sits behind that and cannot be exercised until it answers.

Traces: cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502. The resolved issues 20261119T010042Z-cloudfront-iam-lambda-logs-a-502-whose-origin-sends-cache-control-no.md and 20261117T115022Z-cloudfront-iam-lambda-logs-2-causes-a-negative.md are grounded in this same check and record the edge then serving 502 responses; their subjects are now unreachable because the edge answers nothing at all. Nothing is refused with an HTTP status or error code here, so this is filed as its own transport-level cause rather than described as a refusal regression.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (937/1144 passed; bootstrap_signal false; every service in this check's heading is bootstrapped). Per-service rate: cloudfront__iam__lambda__logs 1/2 (50%).

Failing check `cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502`, verdict mismatch, detail verbatim:
  File "check_cloudfront__iam__lambda__logs_error_caching_min_ttl_holds_a_plain_502_but_not_a_no_store_502.py", line 351, in run
    ok = _fetch_ok(session, f"{edge}/ok")
  File "...", line 186, in _fetch_ok
    raise AssertionError(f"{DATAPLANE_MARKER}: {exc}") from exc
  AssertionError: DATAPLANE_UNIMPLEMENTED: timed out waiting for 200 from https://127.0.0.1/_aws/cloudfront/EU75EL7EWHHV91/ok after 240.0s (last: None)
(the host, port and distribution id are per-run values; `last: None` is the diagnosis — no probe returned any response.)

Expected (AWS): GET <edge>/ok -> 200 with the origin's own body; GET <edge>/err-502-plain -> 502 with the origin's 502 body, and an immediate repeat -> 502 with the byte-identical body (held by ErrorCachingMinTTL=10), with a later request after 10 s returning a fresh body; GET <edge>/err-502-no-store -> 502 with the origin's body, and an immediate repeat -> 502 with a DIFFERENT body (never cached).
Actual (emulator): no HTTP response at all at the edge for 240 s.

Passing controls in the same check (not at issue): CreateFunctionUrlConfig returns an absolute https URL, the function reaches State=Active, CustomErrorResponses round-trips as {502: 10}, and GetDistribution reaches Status=Deployed.

## Steps to Reproduce
1. Create a Lambda function whose handler answers 200 for /ok, 502 with no Cache-Control for /err-502-plain and 502 with `Cache-Control: no-store` for /err-502-no-store, each with a body that changes per invocation; wait for State=Active and create a function URL (AuthType AWS_IAM).
2. cloudfront CreateDistribution with that function URL's host as a CustomOriginConfig origin, CustomErrorResponses = [{ErrorCode: 502, ErrorCachingMinTTL: 10}], a no-cache-policy default behavior forwarding all viewer headers except Host, and ViewerProtocolPolicy allow-all; confirm CustomErrorResponses round-trips as {502: 10}.
3. Poll GetDistribution until Status == 'Deployed' (this already completes).
4. GET <distribution domain>/ok: AWS answers 200 with the origin's body; the emulator returns no HTTP response at all within 240 s.
5. Then exercise /err-502-plain twice (expect 502 with identical bodies within the 10 s TTL, a fresh body after it) and /err-502-no-store twice (expect 502 with different bodies).
6. Re-replay cloudfront__iam__lambda__logs.error-caching-min-ttl-holds-a-plain-502-but-not-a-no-store-502 and confirm it reaches its caching assertions.
