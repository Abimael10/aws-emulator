Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp__logs.set-log-delivery-configuration-duplicate-or-oversized-leaves-existing-configuration-unchanged
Issue-Key: cognito-identity-provider|SetLogDeliveryConfiguration|InvalidParameterException
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cognito-idp__logs: SetLogDeliveryConfiguration's duplicate-event-source InvalidParameterException is worded by the emulator, not AWS

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetLogDeliveryConfiguration

## Description
Cause 1 of 1: the duplicate-event-source refusal message diverges from AWS's text.
AWS: Smithy: SetLogDeliveryConfiguration.LogConfigurations (LogConfigurationListType, @length 0..2); error InvalidParameterException (400). When the same EventSource appears in more than one LogConfiguration the call is refused InvalidParameterException with the message 'Request validation Failed. Following event sources appear more then once in a request: [<eventSource>].' — the sentence (including AWS's 'more then once' spelling) is fixed text and the bracketed list is taken from the request; the stored configuration is left unchanged.
Emulator: answers InvalidParameterException with 'Request validation Failed. Following event sources in request are configured more than once: [userNotification].' — same code, re-authored sentence.
Why: a client that matches on the message to distinguish the duplicate-source failure from the other validation failures of this operation (oversized list, invalid destination) cannot tell them apart.
Fix: the refusal message in SetLogDeliveryConfiguration's validation path in the cognito-idp crate.
Traces: cognito-idp__logs.set-log-delivery-configuration-duplicate-or-oversized-leaves-existing-configuration-unchanged.

## Evidence
AssertionError: expected ('InvalidParameterException', 'Request validation Failed. Following event sources appear more then once in a request: [userNotification].'), got ('InvalidParameterException', 'Request validation Failed. Following event sources in request are configured more than once: [userNotification].')

## Steps to Reproduce
Replay cognito-idp__logs.set-log-delivery-configuration-duplicate-or-oversized-leaves-existing-configuration-unchanged: configure one log delivery configuration on a pool, then call SetLogDeliveryConfiguration with the same EventSource (userNotification) twice in LogConfigurations and compare the Error.Message; then confirm the stored configuration is unchanged.
