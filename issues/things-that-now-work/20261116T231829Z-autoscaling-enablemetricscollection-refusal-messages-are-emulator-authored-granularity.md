Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.metrics-collection-enable-disable autoscaling.metrics-collection-invalid-granularity-rejected autoscaling.metrics-collection-selected-metrics-only-enabled
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.metrics-collection-enable-disable|run#26|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: autoscaling.metrics-collection-invalid-granularity-rejected|run#14|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: autoscaling.metrics-collection-selected-metrics-only-enabled|run#24|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: c037406dab1b28830e94eb95f4ccf1fc05361c78

# autoscaling EnableMetricsCollection refusal messages are emulator-authored: granularity sentence re-worded and the valid-metrics sentence uses the wrong fixed text and value order (2 causes)

> _Note: this issue was identified against the code base at commit `c037406dab1b28830e94eb95f4ccf1fc05361c78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EnableMetricsCollection

## Description
Two net-new root causes in the EnableMetricsCollection validation path. In both, the emulator gets the HTTP status (400) and the error Code (ValidationError) right, but Error.Message is emulator-authored prose rather than the fixed sentence AWS emits for that refusal class. Each cause must be closed on its own failing checks.

Adjacent open issue named for reference: 20261116T221312Z-autoscaling-refusal-messages-diverge-from-aws-s-fixed.md describes the same granularity sentence from the WIRE corpus (its Cause 1), but it carries only wire fixture#step replay keys, not the behavioral check ids below; and it says nothing about the valid-metrics (unknown metric name) sentence in Cause 2 here. Neither cause below is marked ALREADY FILED or OWNED in the behavioral replay report.

=== Cause 1 of 2: unsupported Granularity refusal sentence is re-worded ===
AWS: recorded behavior only. Smithy (com.amazonaws.autoscaling#AutoScaling_2011_01_01, awsQuery) declares EnableMetricsCollection with required input members AutoScalingGroupName and Granularity, both XmlStringMaxLen255 (@length 1..255), and models only ResourceContentionFault — the refusal for an unsupported granularity is not in the model. AWS answers HTTP 400, Error.Code ValidationError, Error.Message 'Valid metrics granularity type is: [1Minute].' The whole sentence is FIXED text, including the plural 'metrics' and the singular 'granularity type is'; the only variable part is the bracketed set, which is the enumeration of granularity values the service accepts (today exactly 1Minute) — not a per-run value.
Emulator: HTTP 400, Code ValidationError, Error.Message 'Valid metric granularity values are: [1Minute].' — singular 'metric' instead of 'metrics', and 'granularity values are' instead of 'granularity type is'. The bracketed set happens to agree, so the divergence is purely in the sentence AWS fixes.
Why: a client (or test, or operator-facing log scraper) that matches AWS's granularity refusal sentence does not recognize this refusal at all, and cannot distinguish it from the other ValidationError refusals EnableMetricsCollection can return.
Fix: the Granularity validation in the EnableMetricsCollection handler of the autoscaling crate — the message template it formats for the unsupported-granularity refusal.
Traces: autoscaling.metrics-collection-invalid-granularity-rejected (Granularity='5Minutes'), autoscaling.metrics-collection-enable-disable (Granularity='5Minute').

=== Cause 2 of 2: unknown metric name refusal uses the wrong fixed text AND lists the metric names in the wrong order ===
AWS: recorded behavior only. Smithy declares EnableMetricsCollection.Metrics as a list of XmlStringMaxLen255 with no enum constraint, and models only ResourceContentionFault, so this refusal is not modeled; DescribeMetricCollectionTypes is the modeled catalogue of the same names. AWS answers HTTP 400, Error.Code ValidationError, Error.Message 'Valid metrics collection types are: [<the service's metric-collection-type names, in the service's catalogue order>].' Fixed text: 'Valid metrics collection types are: [', the ', ' separators, and '].' Variable part: the bracketed list of supported metric-collection-type names and their order, which is a property of the service catalogue, not of the request. AWS's order is GroupMinSize, GroupMaxSize, GroupDesiredCapacity, GroupInServiceInstances, GroupInServiceCapacity, GroupPendingInstances, GroupPendingCapacity, GroupTerminatingInstances, GroupTerminatingCapacity, GroupTerminatingRetainedInstances, GroupTerminatingRetainedCapacity, GroupStandbyInstances, GroupStandbyCapacity, GroupTotalInstances, GroupTotalCapacity, WarmPoolMinSize, WarmPoolDesiredCapacity, WarmPoolPendingCapacity, WarmPoolPendingRetainedCapacity, WarmPoolTerminatingCapacity, WarmPoolTerminatingRetainedCapacity, WarmPoolWarmedCapacity, WarmPoolTotalCapacity, GroupAndWarmPoolDesiredCapacity, GroupAndWarmPoolTotalCapacity — i.e. each Group* instance metric paired with its *Capacity twin.
Emulator: HTTP 400, Code ValidationError, Error.Message 'Valid metrics are: [GroupMinSize, GroupMaxSize, GroupDesiredCapacity, GroupInServiceInstances, GroupPendingInstances, GroupStandbyInstances, GroupTerminatingInstances, GroupTerminatingRetainedInstances, GroupTotalInstances, GroupInServiceCapacity, GroupPendingCapacity, GroupStandbyCapacity, GroupTerminatingCapacity, GroupTerminatingRetainedCapacity, GroupTotalCapacity, WarmPoolMinSize, WarmPoolDesiredCapacity, WarmPoolWarmedCapacity, WarmPoolPendingCapacity, WarmPoolPendingRetainedCapacity, WarmPoolTerminatingCapacity, WarmPoolTerminatingRetainedCapacity, WarmPoolTotalCapacity, GroupAndWarmPoolDesiredCapacity, GroupAndWarmPoolTotalCapacity].' Two divergences: the fixed prefix is 'Valid metrics are: ' instead of 'Valid metrics collection types are: ', and the name set is emitted grouped as all Group*Instances first then all Group*Capacity (and WarmPoolWarmedCapacity hoisted ahead of the WarmPool pending/terminating names) instead of AWS's interleaved instance/capacity ordering.
Why: a client matching AWS's refusal sentence sees an unknown message, and anything that parses the bracketed enumeration out of the refusal to drive a retry or to present the allowed values gets a differently ordered catalogue than AWS and than DescribeMetricCollectionTypes implies.
Fix: the Metrics-name validation in the EnableMetricsCollection handler of the autoscaling crate — both the message template and the ordered metric-collection-type list it renders (which should be the same ordered catalogue DescribeMetricCollectionTypes serves).
Traces: autoscaling.metrics-collection-selected-metrics-only-enabled (Metrics=['NotARealMetricName']).

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against the emulator built from this checkout.

Cause 1:
  [mismatch] autoscaling.metrics-collection-invalid-granularity-rejected
    AssertionError: expected message 'Valid metrics granularity type is: [1Minute].', got 'Valid metric granularity values are: [1Minute].'
  [mismatch] autoscaling.metrics-collection-enable-disable
    AssertionError: expected message 'Valid metrics granularity type is: [1Minute].', got 'Valid metric granularity values are: [1Minute].'
  In both, Error.Code is ValidationError as AWS returns and the HTTP status is 400 — only Error.Message diverges, and the bracketed granularity set agrees.

Cause 2:
  [mismatch] autoscaling.metrics-collection-selected-metrics-only-enabled
    AssertionError: expected message 'Valid metrics collection types are: [GroupMinSize, GroupMaxSize, GroupDesiredCapacity, GroupInServiceInstances, GroupInServiceCapacity, GroupPendingInstances, GroupPendingCapacity, GroupTerminatingInstances, GroupTerminatingCapacity, GroupTerminatingRetainedInstances, GroupTerminatingRetainedCapacity, GroupStandbyInstances, GroupStandbyCapacity, GroupTotalInstances, GroupTotalCapacity, WarmPoolMinSize, WarmPoolDesiredCapacity, WarmPoolPendingCapacity, WarmPoolPendingRetainedCapacity, WarmPoolTerminatingCapacity, WarmPoolTerminatingRetainedCapacity, WarmPoolWarmedCapacity, WarmPoolTotalCapacity, GroupAndWarmPoolDesiredCapacity, GroupAndWarmPoolTotalCapacity].', got 'Valid metrics are: [GroupMinSize, GroupMaxSize, GroupDesiredCapacity, GroupInServiceInstances, GroupPendingInstances, GroupStandbyInstances, GroupTerminatingInstances, GroupTerminatingRetainedInstances, GroupTotalInstances, GroupInServiceCapacity, GroupPendingCapacity, GroupStandbyCapacity, GroupTerminatingCapacity, GroupTerminatingRetainedCapacity, GroupTotalCapacity, WarmPoolMinSize, WarmPoolDesiredCapacity, WarmPoolWarmedCapacity, WarmPoolPendingCapacity, WarmPoolPendingRetainedCapacity, WarmPoolTerminatingCapacity, WarmPoolTerminatingRetainedCapacity, WarmPoolTotalCapacity, GroupAndWarmPoolDesiredCapacity, GroupAndWarmPoolTotalCapacity].'
    The check reached this assertion only after the Code==ValidationError assertion passed, so the status and code are right and the effect-absent assertion (nothing enabled by the rejected call) was not the failure.

Model grounding: services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json — service AutoScaling_2011_01_01, protocol awsQuery. EnableMetricsCollection input: AutoScalingGroupName (required, XmlStringMaxLen255 @length 1..255), Granularity (required, XmlStringMaxLen255 @length 1..255), Metrics (list, unconstrained strings); errors: ResourceContentionFault only. So both refusal sentences are recorded behavior only, and the valid-value enumerations come from the service catalogue exposed by DescribeMetricCollectionTypes.

## Steps to Reproduce
1. git clone the project repo; build and boot the emulator from the checkout.
2. Replay suite simparity-behavioral-aws-v1 and re-run the three checks listed in trace_ids.
3. Cause 1: create a launch template and an Auto Scaling group, then call autoscaling EnableMetricsCollection on that group with Granularity='5Minutes' (or '5Minute') and Metrics=['GroupMinSize']. Assert HTTP 400 / Error.Code 'ValidationError' / Error.Message exactly 'Valid metrics granularity type is: [1Minute].' The emulator answers 'Valid metric granularity values are: [1Minute].'
4. Cause 2: on the same group call autoscaling EnableMetricsCollection with Granularity='1Minute' and Metrics=['NotARealMetricName']. Assert HTTP 400 / Error.Code 'ValidationError' / Error.Message exactly 'Valid metrics collection types are: [' + the catalogue in AWS's interleaved instance/capacity order (see Description) + '].' The emulator answers the 'Valid metrics are: [...]' sentence with the names regrouped.
5. Both causes must pass before closing; each has its own failing check(s) above.
