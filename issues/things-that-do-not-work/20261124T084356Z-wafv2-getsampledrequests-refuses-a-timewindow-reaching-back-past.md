Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wafv2.sampled-requests-reflects-recent-window-only wafv2.e2e-logging-capture-then-teardown
Issue-Shape-Id: com.amazonaws.wafv2#AWSWAF_20190729
Issue-Key: wafv2|GetSampledRequests|WAFInvalidParameterException
Issue-Commit: 24307ed54ea96c21794ebdf30494345495199505

# wafv2 GetSampledRequests refuses a TimeWindow reaching back past three hours instead of clamping StartTime to now-3h

> _Note: this issue was identified against the code base at commit `24307ed54ea96c21794ebdf30494345495199505`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSampledRequests

## Description
REOPENS 20261024T065754Z-wafv2-getsampledrequests-stub.md: its fix left the call refused. (The later fixed issue 20261029T070050Z-wafv2-getsampledrequests-never-returns-a-sample-for-a.md covers the same operation's sampling store; this is the refusal that now sits in front of it.)

Cause 1 of 1: GetSampledRequests rejects any request whose TimeWindow.StartTime (or EndTime) is at or before the call instant minus three hours, instead of accepting the call and clamping the window to the sampling horizon.
Failing checks: wafv2.sampled-requests-reflects-recent-window-only, wafv2.e2e-logging-capture-then-teardown

AWS: a start time older than the three-hour sampling horizon is CLAMPED, not refused. Smithy (services/service-com-amazonaws-wafv2-awswaf-20190729/smithy-model/com-amazonaws-wafv2-awswaf-20190729.json), GetSampledRequests$TimeWindow: "You can specify any time range in the previous three hours. If you specify a start time that's earlier than three hours ago, WAF sets it to three hours ago." The operation's own doc adds "GetSampledRequests returns a time range, which is usually the time range that you specified. However, if your resource ... received 5,000 requests before the specified time range elapsed, GetSampledRequests returns an updated time range" — i.e. the response's TimeWindow is the range WAF actually sampled, and the ONLY documented adjustments are the three-hour clamp and the 5,000-request truncation. The model declares WAFInvalidParameterException for GetSampledRequests, but for parameter-level faults (a malformed/unassociable ARN, an unrecognized parameter value) — nothing in the model makes an out-of-horizon StartTime a refusal, and the member doc states the opposite outcome for exactly that input. So the correct response here is HTTP 200 with SampledRequests (possibly empty), PopulationSize, and TimeWindow.StartTime equal to approximately now minus three hours with EndTime as requested (bounded by the call instant).

Emulator: the call is refused with HTTP 400, Error.Code "WAFInvalidParameterException", Error.Message "Start time and end time must be strictly after current time - 3 hours" — the message is wholly fixed text, carrying no value from the request or from state, so no part of it is a per-run value. The refusal is unconditional on the window alone and fires before the web ACL is even resolved: services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/get_sampled_requests.rs defines WINDOW_OUTSIDE_HORIZON_MESSAGE (line 147) and window_outside_horizon() (lines 230-244), which returns the exception whenever either end of the requested window is not strictly after now-3h, and handle() short-circuits on it (lines 299-304). The clamping behaviour AWS documents is already implemented one function below in sampled_window() (lines 203-215) but is unreachable for exactly the inputs that need it. Two shapes of caller hit this: a window deliberately older than the horizon (StartTime = now-10h, EndTime = now) and a legitimate full-horizon window built as EndTime-3h and then re-polled with a fresh EndTime, which drifts a few seconds past the strict bound.

Why: a client that asks for "the whole sampling window" — the documented and natural way to retrieve every available sample, e.g. StartTime = now-3h — gets a 400 instead of its samples, because the bound is strict and any elapsed time between computing StartTime and the request arriving pushes it outside. Retrieval tooling and dashboards that pin StartTime to now-3h once and then poll never read a single sample, and the per-request audit evidence GetSampledRequests exists to provide is unreachable. Worse, the error class says the caller passed a bad parameter when AWS treats that same parameter as valid and silently narrows it, so clients cannot distinguish a real parameter fault from this one and have no documented remedy to code against.

Fix: the GetSampledRequests handler's window validation — drop the out-of-horizon refusal and let the requested window flow into the existing clamp (start raised to now minus the sampling horizon, end capped at the call instant), returning that clamped range in the response's TimeWindow; keep WAFInvalidParameterException for the parameter faults the model actually names (malformed ARN, unknown Scope). File and line citations are given under Emulator above.

Traces: wafv2.sampled-requests-reflects-recent-window-only (error — the clamp leg: StartTime now-10h, EndTime now, expecting the response StartTime within 120s of now-3h); wafv2.e2e-logging-capture-then-teardown (error — the end-to-end logging/sampling leg, whose polling loop asks for StartTime = EndTime-3h and never gets past this refusal to assert the sample's COUNT action; once the refusal is gone, that check continues into the sampling store, which other wafv2 issues cover).

## Evidence
[error] wafv2.sampled-requests-reflects-recent-window-only
  botocore.errorfactory.WAFInvalidParameterException: An error occurred (WAFInvalidParameterException) when calling the GetSampledRequests operation: Start time and end time must be strictly after current time - 3 hours
  Call: GetSampledRequests(WebAclArn=<REGIONAL web ACL arn>, RuleMetricName='sample-rule', Scope='REGIONAL', TimeWindow={StartTime: now-10h, EndTime: now}, MaxItems=100)
  Expected (AWS, per GetSampledRequests$TimeWindow): HTTP 200; response TimeWindow.StartTime clamped to ~now-3h (check tolerance: within 120s of now-3h, and more than 3600s away from the requested now-10h), SampledRequests a list.
  Actual (emulator): HTTP 400 WAFInvalidParameterException, Message 'Start time and end time must be strictly after current time - 3 hours' (fixed text, no request/state values). The positive control in the same check — StartTime now-1h, EndTime now — passes and is echoed unchanged, so only the out-of-horizon branch diverges.

[error] wafv2.e2e-logging-capture-then-teardown
  botocore.errorfactory.WAFInvalidParameterException: An error occurred (WAFInvalidParameterException) when calling the GetSampledRequests operation: Start time and end time must be strictly after current time - 3 hours
  Call: GetSampledRequests(WebAclArn=<web ACL arn>, RuleMetricName='<metric>rule', Scope='REGIONAL', TimeWindow={StartTime: end-3h where end was sampled before traffic was driven, EndTime: now}, MaxItems=100)
  Expected (AWS): HTTP 200 with the window clamped to the horizon and the driven traffic reported (the check then asserts SampledRequests[0]['Action'] == 'COUNT').
  Actual (emulator): the same HTTP 400 WAFInvalidParameterException on every poll, so the check never reaches the DeleteLoggingConfiguration / ListLoggingConfigurations / WAFNonexistentItemException legs.

Source: services/service-com-amazonaws-wafv2-awswaf-20190729/src/ops/get_sampled_requests.rs lines 147-148 (WINDOW_OUTSIDE_HORIZON_MESSAGE), 230-244 (window_outside_horizon, strict '>' on both ends), 299-304 (handle short-circuits before resolving the web ACL), 203-215 (sampled_window, the documented clamp that is never reached for these inputs).

## Steps to Reproduce
1. wafv2 CreateWebACL(Name=<name>, Scope='REGIONAL', DefaultAction={Allow:{}}, Rules=[{Name:'sample-rule', Priority:1, Action:{Count:{}}, Statement:{GeoMatchStatement:{CountryCodes:['US']}}, VisibilityConfig:{SampledRequestsEnabled:true, CloudWatchMetricsEnabled:true, MetricName:'sample-rule'}}], VisibilityConfig={SampledRequestsEnabled:false, CloudWatchMetricsEnabled:false, MetricName:<name>}); note Summary.ARN.
2. wafv2 GetSampledRequests(WebAclArn=<arn>, RuleMetricName='sample-rule', Scope='REGIONAL', TimeWindow={StartTime: now-1h, EndTime: now}, MaxItems=100) — succeeds, StartTime echoed unchanged (positive control).
3. wafv2 GetSampledRequests with the same arguments but TimeWindow={StartTime: now-10h, EndTime: now} — AWS returns 200 with TimeWindow.StartTime ~= now-3h; the emulator returns HTTP 400 WAFInvalidParameterException 'Start time and end time must be strictly after current time - 3 hours'.
4. Also try TimeWindow={StartTime: now-3h, EndTime: now} computed a moment before the call: AWS accepts it (the clamp is a no-op), the emulator refuses it because the horizon bound is strict.
5. Or replay wafv2.sampled-requests-reflects-recent-window-only and wafv2.e2e-logging-capture-then-teardown.
