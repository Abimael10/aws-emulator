Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift__sns.event-subscription-severity-category-rejected redshift__sns.modify-event-subscription-severity-category-rejected
Issue-Key: redshift__sns.event-subscription-severity-category-rejected|run#17|AssertionError|assert observed == expected, (
Issue-Key: redshift__sns.modify-event-subscription-severity-category-rejected|run#10|AssertionError|assert (code, message) == (
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# redshift__sns: event-subscription Severity/EventCategories refusals carry an emulator-authored "Invalid value for ... Valid Values: ..." message instead of AWS's "... was not recognized." wording, on both Create and ModifyEventSubscription

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSubscription

## Description
Cause 1 of 1: the shared Severity/EventCategories validator for Redshift event subscriptions emits its own enumerate-the-valid-values message instead of AWS's per-value not-recognized sentence. The same wrong wording appears on CreateEventSubscription and on ModifyEventSubscription, for both the Severity and the EventCategories member, so it is one root cause reached from two operations.
  AWS: Smithy (service-com-amazonaws-redshift-redshiftserviceversion20121201): CreateEventSubscription and ModifyEventSubscription both model SubscriptionSeverityNotFoundFault (awsQueryError SubscriptionSeverityNotFound, httpError 404) and SubscriptionCategoryNotFoundFault (awsQueryError SubscriptionCategoryNotFound, httpError 404); Severity is a plain String and EventCategories targets EventCategoriesList, so neither member is a modelled enum and the accepted values are not in the model. Recorded behavior: an unrecognised Severity is refused with SubscriptionSeverityNotFound and the message "Severity '<severity>' was not recognized.", and an unrecognised EventCategories entry with SubscriptionCategoryNotFound and "Category '<category>' was not recognized." Fixed text: "Severity '" / "Category '" and "' was not recognized."; variable part: the single offending value copied from the request. AWS does not list the valid values in either message.
  Emulator: the error codes are correct (SubscriptionSeverityNotFound / SubscriptionCategoryNotFound) but both messages are the emulator's own form that names the input member and enumerates the accepted set — "Invalid value for Severity: '<severity>'. Valid Values: ERROR | INFO" and "Invalid value for EventCategories: '<category>'. Valid Values: configuration | management | monitoring | security | pending". Identical wording is returned by ModifyEventSubscription.
  Why: a client that matches AWS's "was not recognized." sentence to tell an unknown-severity/unknown-category mistake apart from the other 404 faults these operations share (SNSTopicArnNotFound, SourceNotFound, SubscriptionEventIdNotFound, SubscriptionNotFound) fails to classify the refusal, and surfaces an unrecognised error to the user. The surrounding assertions pass, so the only divergence is the message text: the positive control, the no-subscription-left-behind reads, the unchanged stored Severity/EventCategoriesList after a refused modify, and the later valid modify all behave correctly.
  Fix: the shared event-subscription Severity/EventCategories validation used by the CreateEventSubscription and ModifyEventSubscription handlers — emit AWS's per-value "<Severity|Category> '<value>' was not recognized." message for each fault and stop enumerating the valid values.
  Traces: redshift__sns.event-subscription-severity-category-rejected, redshift__sns.modify-event-subscription-severity-category-rejected.

## Evidence
redshift__sns.event-subscription-severity-category-rejected (CreateEventSubscription with SourceType='cluster', SnsTopicArn=<an existing topic>):
  Expected {'bad-severity': ('SubscriptionSeverityNotFound', "Severity 'CRITICAL' was not recognized."), 'bad-category': ('SubscriptionCategoryNotFound', "Category 'not-a-category' was not recognized.")}
  Observed {'bad-severity': ('SubscriptionSeverityNotFound', "Invalid value for Severity: 'CRITICAL'. Valid Values: ERROR | INFO"), 'bad-category': ('SubscriptionCategoryNotFound', "Invalid value for EventCategories: 'not-a-category'. Valid Values: configuration | management | monitoring | security | pending")}

redshift__sns.modify-event-subscription-severity-category-rejected (ModifyEventSubscription on an existing subscription):
  Expected ('SubscriptionSeverityNotFound', "Severity 'CRITICAL' was not recognized.")
  Observed ('SubscriptionSeverityNotFound', "Invalid value for Severity: 'CRITICAL'. Valid Values: ERROR | INFO")

In both checks the error code and the HTTP class are right; only the message diverges. ('CRITICAL' and 'not-a-category' are the values the request supplied — they fill the message template and are not part of it.)

## Steps to Reproduce
1. sns.CreateTopic to get a topic ARN.
2. CreateEventSubscription(SubscriptionName=<name>, SnsTopicArn=<topic>, SourceType='cluster', Severity='INFO', EventCategories=['configuration']) — succeeds (positive control).
3. CreateEventSubscription with the same topic and SourceType but Severity='CRITICAL'. Observe SubscriptionSeverityNotFound with "Invalid value for Severity: 'CRITICAL'. Valid Values: ERROR | INFO" instead of "Severity 'CRITICAL' was not recognized."
4. CreateEventSubscription with Severity='INFO' and EventCategories=['not-a-category']. Observe SubscriptionCategoryNotFound with "Invalid value for EventCategories: 'not-a-category'. Valid Values: ..." instead of "Category 'not-a-category' was not recognized."
5. On the subscription created in step 2, call ModifyEventSubscription(Severity='CRITICAL') and then ModifyEventSubscription(EventCategories=['not-a-category']). Observe the same two emulator-authored messages.
