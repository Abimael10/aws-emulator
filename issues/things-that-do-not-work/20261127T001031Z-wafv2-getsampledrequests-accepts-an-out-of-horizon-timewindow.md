Issue-Source: trace-sweep-divergence
Issue-Trace: wafv2-rate-based-rule-analytics-and-sampling
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wafv2-rate-based-rule-analytics-and-sampling
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|GetSampledRequests|4xx->2xx|$.__type
Issue-Key: wafv2|GetSampledRequests|4xx->2xx|$.message
Issue-Key: wafv2|GetSampledRequests|4xx->2xx|status
Issue-Commit: 730c79e9a78b8174020f0edbe823eb16fd629eef

# wafv2 GetSampledRequests accepts an out-of-horizon TimeWindow instead of refusing it with WAFInvalidParameterException

> _Note: this issue was identified against the code base at commit `730c79e9a78b8174020f0edbe823eb16fd629eef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSampledRequests

## Description
Cause 1 of 1: GetSampledRequests serves HTTP 200 with a silently clamped sampling window for a TimeWindow that reaches at or before the call instant minus three hours, where the recorded AWS wire response for the same call is an HTTP 400 WAFInvalidParameterException refusal.

AWS: recorded behavior only for the refusal itself — the on-disk Smithy model (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json) declares WAFInvalidParameterException (client, HTTP 400) for GetSampledRequests, and GetSampledRequestsRequest$TimeWindow documents only the narrowing outcome ("You can specify any time range in the previous three hours. If you specify a start time that's earlier than three hours ago, WAF sets it to three hours ago"), so the model does NOT state this refusal. The recording does: for a TimeWindow whose ends are not strictly after the call instant minus three hours, AWS answers HTTP 400 with __type "WAFInvalidParameterException" and message "Start time and end time must be strictly after current time - 3 hours". That message is wholly FIXED text in AWS's wording — it interpolates no name, id, value, limit or member name from the request or from service state, and carries no per-run value (no request id, no clock reading), so it is reproducible verbatim. Hypothesis: the member doc describes an older/front-end narrowing behavior that the service no longer applies to a window whose bounds fall outside the retention horizon, and the parameter check now runs ahead of it; the recording is the authoritative statement of the wire contract either way.

Emulator: the horizon check is gone entirely, so the window is always accepted. services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/get_sampled_requests.rs defines SAMPLING_HORIZON (line 137) and sampled_window() (lines 197-209), which clamps StartTime up to now-SAMPLING_HORIZON and EndTime into [start, now] unconditionally; handle() (lines 242-309) validates only Scope and the WebAclArn (WAFInvalidParameterException for a malformed ARN, WAFNonexistentItemException for one naming no web ACL in the scope) and then calls sampled_window() at line 308. There is no branch left that can produce a window-related WAFInvalidParameterException. Replay: expected status 400, got 200; $.__type missing (expected 'WAFInvalidParameterException', got None); $.message missing (expected 'Start time and end time must be strictly after current time - 3 hours', got None).

Note the conflicting OPEN issue 20261124T084356Z-wafv2-getsampledrequests-refuses-a-timewindow-reaching-back-past.md, which is grounded in the TimeWindow member doc and asks for exactly the clamping the emulator now does; it does not carry this fixture step's replay key, so it does not cover this divergence. The recorded AWS response contradicts that issue's premise: AWS refuses this input rather than clamping it. Whoever fixes this must reconcile the two — the wire recording is the contract, so the horizon refusal belongs back in front of the clamp, with the clamp kept for a window that lies inside the horizon ("usually the time range that you specified").

Why: a client relying on AWS's behavior treats the 400 as the signal that its requested window has aged out of the sampling horizon, and re-derives a fresh window (or surfaces the condition) before reading any samples. Against the emulator it instead receives 200 with a TimeWindow it did not ask for and a SampledRequests list drawn from a different, silently substituted range, so an out-of-horizon query is indistinguishable from a successful in-horizon one. Audit and dashboard tooling therefore attributes samples to the wrong interval and never exercises its error path, and negative tests that assert the WAFInvalidParameterException refusal pass against AWS and fail against the emulator.

Fix: the GetSampledRequests handler's window validation in services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/get_sampled_requests.rs — reinstate the pre-clamp check that refuses a TimeWindow whose StartTime or EndTime is not strictly after the call instant minus the sampling horizon, returning WAFInvalidParameterException (HTTP 400) with AWS's fixed message text, and leave sampled_window() to report the actually-sampled range for a window that passes. File and line citations are given under Emulator above.

Traces: wafv2-rate-based-rule-analytics-and-sampling#task#8.

## Evidence
Fixture wafv2-rate-based-rule-analytics-and-sampling, step task#8, operation GetSampledRequests (verdict: mismatch, cluster size 1).

Expected (recorded AWS wire response):
  HTTP 400
  $.__type   = "WAFInvalidParameterException"
  $.message  = "Start time and end time must be strictly after current time - 3 hours"
  (message is entirely fixed text — no request-derived or state-derived substitutions, no per-run values)

Actual (emulator):
  HTTP 200
  $.__type   absent
  $.message  absent
  body carries SampledRequests / PopulationSize / a TimeWindow clamped to [now-3h, now]

Replay detail, verbatim:
  "status: expected 400, got 200\n$.__type: missing (expected 'WAFInvalidParameterException', got None)\n$.message: missing (expected 'Start time and end time must be strictly after current time - 3 hours', got None)"
  divergence_key: service=wafv2|GetSampledRequests|status=400->200|keys=$.__type,$.message,status

Source: services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/get_sampled_requests.rs line 137 (SAMPLING_HORIZON), lines 197-209 (sampled_window clamps both ends unconditionally), lines 242-309 (handle validates Scope and WebAclArn only, then clamps at line 308 — no window refusal remains).

## Steps to Reproduce
1. Boot the emulator and point a wafv2 client at it.
2. wafv2 CreateWebACL(Scope='REGIONAL', DefaultAction={Allow:{}}, with a rate-based rule whose VisibilityConfig sets SampledRequestsEnabled=true and MetricName=<metric>); note Summary.ARN.
3. wafv2 GetSampledRequests(WebAclArn=<arn>, RuleMetricName=<metric>, Scope='REGIONAL', MaxItems=100, TimeWindow={StartTime: an instant at or before now-3h, EndTime: now}).
4. AWS answers HTTP 400 with __type 'WAFInvalidParameterException' and message 'Start time and end time must be strictly after current time - 3 hours'; the emulator answers HTTP 200 with a TimeWindow clamped to [now-3h, now] and a sample list.
5. Or replay fixture wafv2-rate-based-rule-analytics-and-sampling, step task#8.
