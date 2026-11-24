Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.validate-configuration-settings-unknown-namespace-rejected elasticbeanstalk.validate-configuration-settings-unknown-option-rejected elasticbeanstalk.validate-configuration-settings-flags-bad-option elasticbeanstalk.validate-configuration-settings-asg-min-above-max-rejected
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.validate-configuration-settings-unknown-namespace-rejected|run#17|AssertionError|assert messages, (
Issue-Key: elasticbeanstalk.validate-configuration-settings-unknown-option-rejected|run#23|AssertionError|assert len(scoped) == 1, (
Issue-Key: elasticbeanstalk.validate-configuration-settings-flags-bad-option|run#17|AssertionError|assert scoped[0]["Message"] == expected_validation_message, (
Issue-Key: elasticbeanstalk.validate-configuration-settings-asg-min-above-max-rejected|run#26|AssertionError|assert scoped_messages == {"MinSize": min_size_message, "MaxSize": max_size_message}, (
Issue-Commit: f7b60fee7b066cbc6f1eedeac1b0cd0f17c7dcd0

# Elastic Beanstalk configuration-option validation: unknown namespaces/options are silently accepted, and two ValidationMessage texts are emulator-worded (3 causes)

> _Note: this issue was identified against the code base at commit `f7b60fee7b066cbc6f1eedeac1b0cd0f17c7dcd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ValidateConfigurationSettings

## Description
Three net-new root causes in the Elastic Beanstalk configuration-option validation engine that backs ValidateConfigurationSettings (and the pre-write validation of UpdateConfigurationTemplate). No open issue covers them: the nearest ones (20261029T180136Z-elasticbeanstalk-elastic-beanstalk-refusal-messages-use-non-aws-templates.md, 20261104T102022Z-elasticbeanstalk-elastic-beanstalk-state-guard-refusals-use-the-wrong.md, 20261110T135343Z-elasticbeanstalk-elastic-beanstalk-configuration-template-responses-announce-an-off.md) concern refusal envelopes on other operations, template URLs and resolved settings reads, not the ValidationMessage list this operation returns.

Cause 1 of 3: an option specification the platform does not define produces NO error-severity ValidationMessage (unknown namespace, and unknown option name inside a known namespace).
AWS: ValidateConfigurationSettings returns Messages: list<ValidationMessage>{Message, Severity(enum error|warning), Namespace, OptionName} (Smithy: ValidateConfigurationSettings output ConfigurationSettingsValidationMessages; ValidationMessage.Severity values 'error','warning'). Recorded behavior: for an OptionSettings entry whose Namespace is not a real namespace, and for one whose OptionName is not published by DescribeConfigurationOptions for that namespace, AWS returns exactly one ValidationMessage with Severity='error', Namespace and OptionName echoing the values the CALLER supplied, and Message of the fixed form "Invalid option specification (Namespace: '<namespace>', OptionName: '<optionName>'): Unknown configuration setting." — the fixed text is "Invalid option specification (Namespace: '", "', OptionName: '", "'): Unknown configuration setting."; the variable parts are the namespace and option name taken verbatim from the request. The matching write, UpdateConfigurationTemplate carrying the same setting, is refused HTTP 400 with error code ConfigurationValidationException (not a modeled error of the operation — Smithy lists only InsufficientPrivilegesException/TooManyBucketsException — so pinned from the recording) and message "Configuration validation exception: " + that same ValidationMessage text, storing nothing.
Emulator: ValidateConfigurationSettings answers 200 with an empty Messages list for the unknown namespace, and with no message scoped to the unknown option name for the unknown option; the replay fails before it can even reach the UpdateConfigurationTemplate refusal, so the write side is unverified and likely also accepts the setting.
Why: a client that validates a configuration before applying it gets a clean bill of health for a typo'd namespace or option name. On AWS the setting is rejected; against the emulator it validates and (apparently) is stored, so the caller believes an option is in effect that the platform will never honour, and tooling that surfaces validation errors to users shows none.
Fix: the option-validation engine behind the ValidateConfigurationSettings handler must look each submitted (Namespace, OptionName) up in the platform's configuration-option catalog and emit the error-severity 'Invalid option specification ... Unknown configuration setting.' message for every specification it cannot resolve, before any per-option value check; the same engine must gate UpdateConfigurationTemplate.
Traces: elasticbeanstalk.validate-configuration-settings-unknown-namespace-rejected (AssertionError: "ValidateConfigurationSettings must report the unknown namespace 'aws:elasticbeanstalk:namespace-that-does-not-exist'; an empty Messages list means the validation engine accepted it"), elasticbeanstalk.validate-configuration-settings-unknown-option-rejected (AssertionError: "exactly one error-severity ValidationMessage must be attributed to ('aws:autoscaling:asg', 'NotAnOptionInThisNamespace'); got []").

Cause 2 of 3: the message for a non-numeric value of a numeric option is reworded.
AWS: recorded behavior only (the model does not state message text). For an integer-valued option given a value that is not a number, the ValidationMessage is Severity='error', Namespace/OptionName echoing the request, and Message of the form "Invalid option value: '<value>' (Namespace: '<namespace>', OptionName: '<optionName>'): Value is not a number". Fixed text: "Invalid option value: '", "' (Namespace: '", "', OptionName: '", "'): Value is not a number"; variable parts: the submitted value, namespace and option name. The corresponding UpdateConfigurationTemplate is refused 400 ConfigurationValidationException with "Configuration validation exception: " + that text.
Emulator: emits the same envelope but ends the sentence "...: Value is not a valid integer" instead of "...: Value is not a number".
Why: a client or operator matching on the diagnostic sentence (log scraping, user-facing error surfacing, test fixtures) does not recognise the emulator's text, and the same divergence propagates into the ConfigurationValidationException message of the refused write.
Fix: the numeric-value check in the option-validation engine must emit AWS's trailing clause "Value is not a number".
Traces: elasticbeanstalk.validate-configuration-settings-flags-bad-option (expected "Invalid option value: 'not-a-number' (Namespace: 'aws:autoscaling:asg', OptionName: 'MinSize'): Value is not a number", got "... Value is not a valid integer").

Cause 3 of 3: the MinSize>MaxSize co-requirement is reported as a range violation instead of a co-requirement violation.
AWS: recorded behavior only. aws:autoscaling:asg MinSize and MaxSize are each individually inside the range DescribeConfigurationOptions publishes (MinValue..MaxValue), but MinSize must not exceed MaxSize. When the pair is inverted, AWS returns TWO error-severity ValidationMessages in the aws:autoscaling:asg namespace: one on OptionName='MinSize' with Message "Invalid option value: '<minSize>' (Namespace: 'aws:autoscaling:asg', OptionName: 'MinSize'): MinSize is greater than MaxSize" and one on OptionName='MaxSize' with Message "Invalid option value: '<maxSize>' (Namespace: 'aws:autoscaling:asg', OptionName: 'MaxSize'): MaxSize is less than MinSize". Fixed text is the "Invalid option value: '...' (Namespace: '...', OptionName: '...'): " envelope plus the clause "MinSize is greater than MaxSize" / "MaxSize is less than MinSize"; the variable parts are the two submitted values and the namespace/option names from the request. UpdateConfigurationTemplate carrying the inverted pair is refused 400 ConfigurationValidationException with "Configuration validation exception: " + one of those two texts (AWS picks either half run to run), and the stored legal pair is left intact.
Emulator: validates each half against the OTHER half as if it were that option's published range bound, producing "Invalid option value: '4' (Namespace: 'aws:autoscaling:asg', OptionName: 'MinSize'): Value exceeds maximum allowed value: 2" and "Invalid option value: '2' (Namespace: 'aws:autoscaling:asg', OptionName: 'MaxSize'): Value is below minimum allowed value: 4" — i.e. it misreports a cross-option co-requirement as a per-option range violation, naming a limit that is not the option's published MinValue/MaxValue.
Why: the emulator tells the caller the value is outside the option's allowed range when it is in range; a client reading the diagnostic (or a user) is pointed at the wrong fix (widen the range) instead of the real one (order the pair), and the invented limit contradicts DescribeConfigurationOptions' published MinValue/MaxValue for the same option.
Fix: add a cross-option co-requirement check for aws:autoscaling:asg MinSize/MaxSize in the option-validation engine, emitting AWS's two clauses, and keep the per-option range check bounded by the catalog's published MinValue/MaxValue only.
Traces: elasticbeanstalk.validate-configuration-settings-asg-min-above-max-rejected.

## Evidence
Suite: elasticbeanstalk 120/173 (69%). Four surviving (not already-filed, not owned) failures, all on the configuration-option validation engine:

1) [mismatch] elasticbeanstalk.validate-configuration-settings-unknown-namespace-rejected
   AssertionError: ValidateConfigurationSettings must report the unknown namespace 'aws:elasticbeanstalk:namespace-that-does-not-exist'; an empty Messages list means the validation engine accepted it
   Expected: exactly one ValidationMessage {Severity:'error', Namespace:'aws:elasticbeanstalk:namespace-that-does-not-exist', OptionName:'MinSize', Message:"Invalid option specification (Namespace: 'aws:elasticbeanstalk:namespace-that-does-not-exist', OptionName: 'MinSize'): Unknown configuration setting."}, then UpdateConfigurationTemplate -> 400 ConfigurationValidationException "Configuration validation exception: <that text>".
   Actual: HTTP 200, Messages = [].

2) [mismatch] elasticbeanstalk.validate-configuration-settings-unknown-option-rejected
   AssertionError: exactly one error-severity ValidationMessage must be attributed to ('aws:autoscaling:asg', 'NotAnOptionInThisNamespace'); got []
   Expected: Message "Invalid option specification (Namespace: 'aws:autoscaling:asg', OptionName: 'NotAnOptionInThisNamespace'): Unknown configuration setting." (the option is absent from DescribeConfigurationOptions for that namespace, which the check verifies first).
   Actual: no error-severity message scoped to that option.

3) [mismatch] elasticbeanstalk.validate-configuration-settings-flags-bad-option
   Expected Message: "Invalid option value: 'not-a-number' (Namespace: 'aws:autoscaling:asg', OptionName: 'MinSize'): Value is not a number"
   Actual   Message: "Invalid option value: 'not-a-number' (Namespace: 'aws:autoscaling:asg', OptionName: 'MinSize'): Value is not a valid integer"

4) [mismatch] elasticbeanstalk.validate-configuration-settings-asg-min-above-max-rejected
   AssertionError: expected the exact ValidationMessage text on both halves of the inverted pair, got
     {'MinSize': "Invalid option value: '4' (Namespace: 'aws:autoscaling:asg', OptionName: 'MinSize'): Value exceeds maximum allowed value: 2",
      'MaxSize': "Invalid option value: '2' (Namespace: 'aws:autoscaling:asg', OptionName: 'MaxSize'): Value is below minimum allowed value: 4"}
   Expected:
     MinSize -> "Invalid option value: '4' (Namespace: 'aws:autoscaling:asg', OptionName: 'MinSize'): MinSize is greater than MaxSize"
     MaxSize -> "Invalid option value: '2' (Namespace: 'aws:autoscaling:asg', OptionName: 'MaxSize'): MaxSize is less than MinSize"
   (the check first asserts via DescribeConfigurationOptions that MinSize=4 and MaxSize=2 are each individually inside the published [MinValue, MaxValue] range, so the emulator's 'exceeds maximum allowed value' / 'below minimum allowed value' limits are not the option's published bounds.)

Smithy (elasticbeanstalk, AWSElasticBeanstalkService): ValidateConfigurationSettings(ApplicationName required, TemplateName, EnvironmentName, OptionSettings required) -> ConfigurationSettingsValidationMessages{Messages: list<ValidationMessage>}; ValidationMessage{Message, Severity: enum[error,warning], Namespace, OptionName}; modeled errors only InsufficientPrivilegesException(403)/TooManyBucketsException(400). UpdateConfigurationTemplate models the same two errors only, so ConfigurationValidationException (400) is pinned from the recordings.

## Steps to Reproduce
For all causes: CreateApplication, then CreateConfigurationTemplate on a SolutionStackName from ListAvailableSolutionStacks.

Cause 1 (unknown namespace): ValidateConfigurationSettings(ApplicationName, TemplateName, OptionSettings=[{Namespace:'aws:elasticbeanstalk:namespace-that-does-not-exist', OptionName:'MinSize', Value:'2'}]). Observe Messages == [] instead of one error-severity message "Invalid option specification (Namespace: '<that namespace>', OptionName: 'MinSize'): Unknown configuration setting.". Then UpdateConfigurationTemplate with the same OptionSettings and observe it is not refused with 400 ConfigurationValidationException.

Cause 1 (unknown option): same, with OptionSettings=[{Namespace:'aws:autoscaling:asg', OptionName:'NotAnOptionInThisNamespace', Value:'2'}] (confirm via DescribeConfigurationOptions(SolutionStackName) that the namespace publishes MinSize but not that option). Observe no error-severity message scoped to it.

Cause 2: ValidateConfigurationSettings with OptionSettings=[{Namespace:'aws:autoscaling:asg', OptionName:'MinSize', Value:'not-a-number'}]. Observe the message ends "Value is not a valid integer" instead of "Value is not a number".

Cause 3: create the template with OptionSettings=[{asg/MinSize:'2'},{asg/MaxSize:'4'}]; confirm MinSize=4 and MaxSize=2 are each inside their published MinValue/MaxValue via DescribeConfigurationOptions; ValidateConfigurationSettings with OptionSettings=[{asg/MinSize:'4'},{asg/MaxSize:'2'}]. Observe the two messages blame per-option range limits instead of "MinSize is greater than MaxSize" / "MaxSize is less than MinSize".

Replay: run_behavioral_suite and re-run the four checks listed in trace_ids.
