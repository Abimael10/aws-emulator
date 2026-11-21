Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.eb-update-environment-refuses-the-settings-validate-configuration-flags
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.eb-update-environment-refuses-the-settings-validate-configuration-flags|run#34|AssertionError|assert flagged["Message"] == expected_validator_message, (
Issue-Commit: 8cbf0dcd0020846f13fa76c0319ab04fca2e29a2

# Elastic Beanstalk option validation: the BatchSize over-maximum ValidationMessage omits AWS's " for <BatchSizeType> Batch Size Type." clause

> _Note: this issue was identified against the code base at commit `8cbf0dcd0020846f13fa76c0319ab04fca2e29a2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateEnvironment

## Description
Cause 1 of 1: the validator's message for an aws:elasticbeanstalk:command BatchSize value above the allowed maximum is emulator-worded.
AWS: Smithy: UpdateEnvironment / ValidateConfigurationSettings return ValidationMessage {Message, Severity, Namespace, OptionName}; Severity 'error' means "this is not a valid setting for an option". For a BatchSize above the maximum allowed for the environment's BatchSizeType, AWS's Message is "Invalid option value: '<value>' (Namespace: '<namespace>', OptionName: '<option>'): Value exceeds maximum allowed value: <maximum> for <BatchSizeType> Batch Size Type." The fixed text is "Invalid option value: ", " (Namespace: ", ", OptionName: ", "): Value exceeds maximum allowed value: ", " for ", " Batch Size Type." — the variable parts are the rejected value, the namespace, the option name, the enforced maximum, and the BatchSizeType the maximum belongs to, all taken from the request and the environment's configuration. Recorded behavior for the exact text.
Emulator: the message stops after the number — "Invalid option value: '200' (Namespace: 'aws:elasticbeanstalk:command', OptionName: 'BatchSize'): Value exceeds maximum allowed value: 100" — dropping the " for Percentage Batch Size Type." clause (and its trailing period). Status/shape are otherwise as expected; only the text diverges.
Why: the clause is the part that tells a client WHICH limit was applied — the maximum for a Percentage batch size differs from the maximum for a Fixed one. Without it an operator cannot tell whether to lower the value or change BatchSizeType, and tooling that matches on AWS's message text (or surfaces it to users) diverges.
Fix: the configuration-option validator's message construction for the BatchSize range rule in the elasticbeanstalk service. Related open issue 20261115T085940Z-elasticbeanstalk-elastic-beanstalk-configuration-option-validation-unknown-namespaces-options.md covers two OTHER emulator-worded ValidationMessage texts and does not own this check or this rule.
Traces: elasticbeanstalk.eb-update-environment-refuses-the-settings-validate-configuration-flags.

## Evidence
AssertionError: the validator must name the published maximum it is enforcing; expected "Invalid option value: '200' (Namespace: 'aws:elasticbeanstalk:command', OptionName: 'BatchSize'): Value exceeds maximum allowed value: 100 for Percentage Batch Size Type.", its message was "Invalid option value: '200' (Namespace: 'aws:elasticbeanstalk:command', OptionName: 'BatchSize'): Value exceeds maximum allowed value: 100"

## Steps to Reproduce
1. Create an application, an application version and an environment; wait for Ready.
2. Call UpdateEnvironment (or ValidateConfigurationSettings) with OptionSettings including {Namespace: 'aws:elasticbeanstalk:command', OptionName: 'BatchSizeType', Value: 'Percentage'} and {Namespace: 'aws:elasticbeanstalk:command', OptionName: 'BatchSize', Value: '200'}.
3. Read the returned ValidationMessage for Namespace 'aws:elasticbeanstalk:command' / OptionName 'BatchSize': its Message ends at the maximum instead of continuing "... for Percentage Batch Size Type."
