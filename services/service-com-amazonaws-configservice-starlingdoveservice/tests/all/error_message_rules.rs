// SPDX-License-Identifier: CC0-1.0

//! How Config WORDS a refusal, on the wire.
//!
//! An error message whose production rule is known is part of the contract, and these
//! cases pin the rules rather than the one recorded instance of each:
//!
//! 1. a precondition failure carries a FIXED message naming the precondition that
//!    failed, so two operations sharing one error class do not collapse into one
//!    sentence;
//! 2. a not-found refusal whose message is BUILT FROM THE REQUEST quotes the name the
//!    caller sent, and the same class words the same way in every operation that
//!    raises it — while a class whose message is fixed carries that one text wherever
//!    it is raised, including the ARN lookups;
//! 3. a not-found refusal follows the SHAPE of the member that named the resource: a
//!    list-shaped lookup reports the plural form, a single-name member the singular
//!    form quoting that name;
//! 4. a limit refusal states the service's own limit and nothing of the environment,
//!    so the same cause reads identically in every Region;
//! 5. a request-validation refusal advertises a `@pattern` without the model's
//!    whole-value anchors, names a member the input wraps by the name the model
//!    declares it with, and withholds the value of a member Config does not echo;
//! 6. a request-validation refusal of a value outside an enumerated type lists the
//!    values that type admits in the order CONFIG lists them, which is neither the
//!    order the model declares them in nor a sort of it, and lists one type the same
//!    way wherever a member targeting it carried the value.
//!
//! Every case drives the emulator the way a client does, through the same
//! `MultiServiceRouter` the production router builds, reusing the `Emulator` harness
//! `config_setup_and_rule_loop` already provides.

use serde_json::{json, Value};

use crate::config_setup_and_rule_loop::{recorder_request, Emulator, ROLE_ARN};

/// A delivery channel body naming a bucket; its value is never asserted on.
fn channel(name: &str) -> Value {
    json!({ "DeliveryChannel": { "name": name, "s3BucketName": "config-bucket" } })
}

/// Call `operation`, require a 400 with the error code `code`, and return the message
/// the service worded the refusal with.
async fn refusal(emulator: &Emulator, operation: &str, body: &Value, code: &str) -> String {
    let (status, parsed) = emulator.call(operation, body).await;
    assert_eq!(
        status, 400,
        "{operation}: expected HTTP 400, got {status} with body {parsed}"
    );
    let actual = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
        .unwrap_or_else(|| format!("<no __type in {parsed}>"));
    assert_eq!(
        actual, code,
        "{operation}: expected the error code {code}, got {actual:?} in {parsed}"
    );
    parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{operation}: expected a message in {parsed}"))
        .to_string()
}

/// A precondition failure names the precondition that failed, not the class: the two
/// operations that share `NoAvailableConfigurationRecorderException` word it
/// differently, and the delivery-channel precondition of
/// `StartConfigurationRecorder` has its own sentence too.
#[tokio::test]
async fn a_precondition_failure_names_the_precondition_that_failed() {
    // (case, setup, operation, body, error code, message).
    for (case, needs_recorder, operation, body, code, expected) in [
        (
            "a rule with no recorder",
            false,
            "PutConfigRule",
            json!({
                "ConfigRule": {
                    "ConfigRuleName": "rule-one",
                    "Source": { "Owner": "AWS", "SourceIdentifier": "S3_BUCKET_VERSIONING_ENABLED" },
                }
            }),
            "NoAvailableConfigurationRecorderException",
            "You must create a configuration recorder before you can create or update a \
             Config rule.",
        ),
        (
            "a delivery channel with no recorder",
            false,
            "PutDeliveryChannel",
            channel("default"),
            "NoAvailableConfigurationRecorderException",
            "Configuration recorder is not available to put delivery channel.",
        ),
        (
            "starting a recorder with no delivery channel",
            true,
            "StartConfigurationRecorder",
            json!({ "ConfigurationRecorderName": "default" }),
            "NoAvailableDeliveryChannelException",
            "Delivery channel is not available to start configuration recorder.",
        ),
    ] {
        let emulator = Emulator::new();
        if needs_recorder {
            emulator
                .ok(
                    "PutConfigurationRecorder",
                    &recorder_request(json!({ "roleARN": ROLE_ARN })),
                )
                .await;
        }
        let message = refusal(&emulator, operation, &body, code).await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// A `NoSuchDeliveryChannelException` quotes the channel name the request carried,
/// wherever it is raised: the delete path, the describe path and the status path all
/// word it the same way.
#[tokio::test]
async fn an_absent_delivery_channel_is_quoted_by_the_name_the_request_sent() {
    // Names differing in form — the default Config assigns, a hyphenated name, one
    // with digits and underscores, and a single character (the `ChannelName` length
    // minimum, from `smithy_query(query="shape", shape="ChannelName")`: 1..256).
    for name in ["default", "sim-probe-channel", "channel_42", "c"] {
        let emulator = Emulator::new();
        let expected = format!("Cannot find delivery channel with specified name '{name}'.");

        let deleted = refusal(
            &emulator,
            "DeleteDeliveryChannel",
            &json!({ "DeliveryChannelName": name }),
            "NoSuchDeliveryChannelException",
        )
        .await;
        assert_eq!(
            deleted, expected,
            "case {name:?} (delete): expected {expected:?}, got {deleted:?}"
        );

        let described = refusal(
            &emulator,
            "DescribeDeliveryChannels",
            &json!({ "DeliveryChannelNames": [name] }),
            "NoSuchDeliveryChannelException",
        )
        .await;
        assert_eq!(
            described, expected,
            "case {name:?} (describe): expected {expected:?}, got {described:?}"
        );

        let status = refusal(
            &emulator,
            "DescribeDeliveryChannelStatus",
            &json!({ "DeliveryChannelNames": [name] }),
            "NoSuchDeliveryChannelException",
        )
        .await;
        assert_eq!(
            status, expected,
            "case {name:?} (describe status): expected {expected:?}, got {status:?}"
        );
    }
}

/// Deleting the account's only delivery channel while its recorder is running quotes
/// the channel name and says why the delete failed.
#[tokio::test]
async fn a_blocked_delivery_channel_delete_quotes_the_channel_it_refused() {
    for name in ["default", "sim-probe-channel", "c"] {
        let emulator = Emulator::new();
        emulator
            .ok(
                "PutConfigurationRecorder",
                &recorder_request(json!({ "roleARN": ROLE_ARN })),
            )
            .await;
        emulator.ok("PutDeliveryChannel", &channel(name)).await;
        emulator
            .ok(
                "StartConfigurationRecorder",
                &json!({ "ConfigurationRecorderName": "default" }),
            )
            .await;

        let message = refusal(
            &emulator,
            "DeleteDeliveryChannel",
            &json!({ "DeliveryChannelName": name }),
            "LastDeliveryChannelDeleteFailedException",
        )
        .await;
        let expected = format!(
            "Failed to delete last specified delivery channel with name '{name}', because \
             there is a running configuration recorder."
        );
        assert_eq!(
            message, expected,
            "case {name:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// `NoSuchConfigurationRecorderException` names the MEMBER the request selected the
/// recorder by: a lookup by name is refused with the name-variant sentence, a lookup by
/// ARN with the arn-variant one, so the caller is told to verify the input it actually
/// sent — which for a service-linked recorder, addressable only by ARN or by service
/// principal, is the only guidance there is.
#[tokio::test]
async fn an_absent_configuration_recorder_names_the_selector_the_request_used() {
    const BY_NAME: &str = "The name of the configuration recorder you entered cannot be \
                           found. Verify the configuration recorder name, and try again.";
    const BY_ARN: &str = "The arn of the configuration recorder you entered cannot be found. \
                          Verify the configuration recorder arn, and try again. ";
    const ABSENT_ARN: &str = "arn:aws:config:us-east-1:000000000000:configuration-recorder/x/y";

    // (case, operation, body, the message expected) — every operation that looks a
    // recorder up through a member that names ONE recorder among several selectors.
    for (case, operation, body, expected) in [
        (
            "start by name",
            "StartConfigurationRecorder",
            json!({ "ConfigurationRecorderName": "absent-recorder" }),
            BY_NAME,
        ),
        (
            "stop by name",
            "StopConfigurationRecorder",
            json!({ "ConfigurationRecorderName": "absent-recorder" }),
            BY_NAME,
        ),
        (
            "delete by name",
            "DeleteConfigurationRecorder",
            json!({ "ConfigurationRecorderName": "absent-recorder" }),
            BY_NAME,
        ),
        (
            "describe by name",
            "DescribeConfigurationRecorders",
            json!({ "ConfigurationRecorderNames": ["absent-recorder"] }),
            BY_NAME,
        ),
        (
            "describe status by name",
            "DescribeConfigurationRecorderStatus",
            json!({ "ConfigurationRecorderNames": ["absent-recorder"] }),
            BY_NAME,
        ),
        (
            "describe by ARN",
            "DescribeConfigurationRecorders",
            json!({ "Arn": ABSENT_ARN }),
            BY_ARN,
        ),
        (
            "describe status by ARN",
            "DescribeConfigurationRecorderStatus",
            json!({ "Arn": ABSENT_ARN }),
            BY_ARN,
        ),
    ] {
        let emulator = Emulator::new();
        let message = refusal(
            &emulator,
            operation,
            &body,
            "NoSuchConfigurationRecorderException",
        )
        .await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The service-linked delete resolves ONE recorder of ONE account from the selector
    // it was given, and states what it looked for: the account and that selector's own
    // value, naming the member it came from.
    let account = emulator_server::emulator_account_id();
    for (case, body, expected) in [
        (
            "service-linked delete by ARN",
            json!({ "Arn": ABSENT_ARN }),
            format!(
                "Configuration recorder (AccountId: {account}, RecorderArn: {ABSENT_ARN}) \
                 cannot be found"
            ),
        ),
        (
            "service-linked delete by service principal",
            json!({ "ServicePrincipal": "observabilityadmin.amazonaws.com" }),
            format!(
                "Configuration recorder (AccountId: {account}, Service Principal: \
                 observabilityadmin.amazonaws.com) cannot be found"
            ),
        ),
    ] {
        let emulator = Emulator::new();
        let message = refusal(
            &emulator,
            "DeleteServiceLinkedConfigurationRecorder",
            &body,
            "NoSuchConfigurationRecorderException",
        )
        .await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// `NoSuchConfigRuleException` follows the operation's relation to the rules named: a
/// read that answers about a LIST of rules at once reports the plural form and quotes no
/// name, while an operation that resolves one rule to act on names the name it carried.
#[tokio::test]
async fn an_absent_config_rule_is_worded_for_the_member_that_named_it() {
    const PLURAL: &str = "One or more ConfigRules provided in the request are invalid. \
                          Please check the configRule names.";

    for name in ["absent-rule", "probe_rule_two", "r"] {
        let emulator = Emulator::new();

        // `DescribeConfigRules$ConfigRuleNames` is a list (0..25 of `ConfigRuleName`).
        let listed = refusal(
            &emulator,
            "DescribeConfigRules",
            &json!({ "ConfigRuleNames": [name] }),
            "NoSuchConfigRuleException",
        )
        .await;
        assert_eq!(
            listed, PLURAL,
            "case {name:?} (list member): expected {PLURAL:?}, got {listed:?}"
        );

        // `DescribeComplianceByConfigRule$ConfigRuleNames` is the same list-shaped
        // member, so the same class words itself the same way.
        let compliance = refusal(
            &emulator,
            "DescribeComplianceByConfigRule",
            &json!({ "ConfigRuleNames": [name] }),
            "NoSuchConfigRuleException",
        )
        .await;
        assert_eq!(
            compliance, PLURAL,
            "case {name:?} (second list member): expected {PLURAL:?}, got {compliance:?}"
        );

        // `DeleteConfigRule$ConfigRuleName` names exactly one rule.
        let single = refusal(
            &emulator,
            "DeleteConfigRule",
            &json!({ "ConfigRuleName": name }),
            "NoSuchConfigRuleException",
        )
        .await;
        let expected = format!(
            "The ConfigRule '{name}' provided in the request is invalid. Please check the \
             configRule name."
        );
        assert_eq!(
            single, expected,
            "case {name:?} (single member): expected {expected:?}, got {single:?}"
        );
    }
}

/// The recorder limit refusal states the service's own limit and nothing of the
/// environment: the same cause reads identically in every Region.
#[tokio::test]
async fn the_recorder_limit_refusal_states_the_limit_and_not_the_region() {
    const EXPECTED: &str = "Failed to put configuration recorder  because you have reached \
                            the limit for the maximum number of customer managed \
                            configuration records: (1)";

    for region in ["us-east-1", "us-west-2", "eu-west-1", "ap-southeast-2"] {
        let emulator = Emulator::in_region(region);
        emulator
            .ok(
                "PutConfigurationRecorder",
                &recorder_request(json!({ "name": "default", "roleARN": ROLE_ARN })),
            )
            .await;

        let message = refusal(
            &emulator,
            "PutConfigurationRecorder",
            &recorder_request(json!({ "name": "second", "roleARN": ROLE_ARN })),
            "MaxNumberOfConfigurationRecordersExceededException",
        )
        .await;
        assert_eq!(
            message, EXPECTED,
            "case {region:?}: expected {EXPECTED:?}, got {message:?}"
        );
        assert!(
            !message.contains(region),
            "case {region:?}: the message must not name the Region, got {message:?}"
        );
    }
}

/// A request-validation refusal advertises the `@pattern` a value must satisfy without
/// the whole-value anchors the model spells it with, and echoes the value the caller
/// submitted.
#[tokio::test]
async fn a_validation_refusal_advertises_the_pattern_without_its_anchors() {
    // (case, operation, body, the clause the service states). The patterns come from
    // `smithy_query(query="shape", shape="AccountId")` (`^\d{12}$`) and
    // `smithy_query(query="shape", shape="QueryName")` (`^[a-zA-Z0-9-_]+$`).
    for (case, operation, body, expected) in [
        (
            "an account id that is not twelve digits",
            "DeletePendingAggregationRequest",
            json!({ "RequesterAccountId": "not-an-acct1", "RequesterAwsRegion": "us-east-1" }),
            "1 validation error detected: Value 'not-an-acct1' at 'requesterAccountId' \
             failed to satisfy constraint: Member must satisfy regular expression pattern: \
             \\d{12}",
        ),
        (
            "an account id that is too short",
            "DeletePendingAggregationRequest",
            json!({ "RequesterAccountId": "12345", "RequesterAwsRegion": "eu-west-1" }),
            // Twelve digits are the only form the member admits, so a shorter value
            // broke the length its digits fix as well as the pattern: a clause each.
            "2 validation errors detected: Value '12345' at 'requesterAccountId' failed \
             to satisfy constraint: Member must have length greater than or equal to 12; \
             Value '12345' at 'requesterAccountId' failed to satisfy constraint: Member \
             must satisfy regular expression pattern: \\d{12}",
        ),
        (
            "an aggregation request missing its Region",
            "DeletePendingAggregationRequest",
            json!({ "RequesterAccountId": "000000000000" }),
            "1 validation error detected: Value null at 'requesterAwsRegion' failed to \
             satisfy constraint: Member must not be null",
        ),
    ] {
        let emulator = Emulator::new();
        let message = refusal(&emulator, operation, &body, "ValidationException").await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// The value submitted at a member Config does not echo back is withheld: the clause
/// states the position and the constraint alone, wherever that member travels.
#[tokio::test]
async fn a_withheld_members_value_is_not_echoed_into_the_clause() {
    const EXPECTED: &str = "1 validation error detected: Value at 'servicePrincipal' failed \
                            to satisfy constraint: Member must satisfy regular expression \
                            pattern: [\\w+=,.@-]+";

    // Every operation whose input carries `ServicePrincipal`
    // (`smithy_query(query="shape", shape="ServicePrincipal")`: `@pattern`
    // `^[\w+=,.@-]+$`, `@length(1, 128)`), including the three the issue did not cite.
    for (case, operation, body) in [
        (
            "the third-party service-linked recorder",
            "PutThirdPartyServiceLinkedConfigurationRecorder",
            json!({
                "ServicePrincipal": "not a valid principal!!",
                "ConnectorArn": "arn:aws:config:us-east-1:000000000000:connector/c-1",
                "ScopeConfiguration": { "scopeType": "tenant", "allRegions": true },
            }),
        ),
        (
            "the service-linked recorder",
            "PutServiceLinkedConfigurationRecorder",
            json!({ "ServicePrincipal": "not a valid principal!!" }),
        ),
        (
            "the service-linked recorder delete",
            "DeleteServiceLinkedConfigurationRecorder",
            json!({ "ServicePrincipal": "not a valid principal!!" }),
        ),
        (
            "the recorder describe filter",
            "DescribeConfigurationRecorders",
            json!({ "ServicePrincipal": "not a valid principal!!" }),
        ),
        (
            "the recorder status describe filter",
            "DescribeConfigurationRecorderStatus",
            json!({ "ServicePrincipal": "not a valid principal!!" }),
        ),
    ] {
        let emulator = Emulator::new();
        let message = refusal(&emulator, operation, &body, "ValidationException").await;
        assert_eq!(
            message, EXPECTED,
            "case {case:?}: expected {EXPECTED:?}, got {message:?}"
        );
    }
}

/// A member the input carries inside a WRAPPER structure is named by the name the
/// model declares it with — not by the path through the wrapper — with the model's
/// `@pattern` printed as the model declares it.
#[tokio::test]
async fn a_wrapped_members_refusal_names_the_member_the_model_declares() {
    // (case, the StoredQuery the request carried, the clause the service states).
    // `StoredQuery`'s members and their constraints come from
    // `smithy_query(query="shape", shape="StoredQuery")`: `QueryName` 1..64 /
    // `^[a-zA-Z0-9-_]+$`, `QueryId` 1..36 / `^\S+$`.
    let long_name = "q".repeat(65);
    for (case, stored_query, expected) in [
        (
            "a query name with spaces",
            json!({ "QueryName": "invalid query name" }),
            "1 validation error detected: Value 'invalid query name' at 'QueryName' \
             failed to satisfy constraint: Member must satisfy regular expression pattern: \
             ^[a-zA-Z0-9-_]+$"
                .to_string(),
        ),
        (
            "a query name over its length maximum",
            json!({ "QueryName": long_name.clone() }),
            format!(
                "1 validation error detected: Value '{long_name}' at 'QueryName' failed to \
                 satisfy constraint: Member must have length less than or equal to 64"
            ),
        ),
        (
            "a query id with a space",
            json!({ "QueryName": "valid-name", "QueryId": "a b" }),
            "1 validation error detected: Value 'a b' at 'QueryId' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: ^\\S+$"
                .to_string(),
        ),
        (
            "an absent query name",
            json!({ "QueryId": "q-1" }),
            "1 validation error detected: Value null at 'QueryName' failed to satisfy \
             constraint: Member must not be null"
                .to_string(),
        ),
    ] {
        let emulator = Emulator::new();
        let message = refusal(
            &emulator,
            "PutStoredQuery",
            &json!({ "StoredQuery": stored_query }),
            "ValidationException",
        )
        .await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// The values the snapshot-frequency type admits, listed as Config lists them. The five
/// values are `smithy_query(query="shape", shape="MaximumExecutionFrequency")`'s, which the
/// model declares `One_Hour, Three_Hours, Six_Hours, Twelve_Hours, TwentyFour_Hours` — not
/// the order Config lists them in.
const SNAPSHOT_FREQUENCIES: &str =
    "[TwentyFour_Hours, Twelve_Hours, Three_Hours, Six_Hours, One_Hour]";

/// The clause requirement a refusal of a value outside the snapshot-frequency type states.
fn frequency_requirement() -> String {
    format!("Member must satisfy enum value set: {SNAPSHOT_FREQUENCIES}")
}

/// The values the enum clause of `message` lists, in the order it listed them.
fn listed_value_set(case: &str, message: &str) -> Vec<String> {
    let (_, listed) = message
        .split_once("Member must satisfy enum value set: [")
        .unwrap_or_else(|| panic!("case {case:?}: expected an enum value set, got {message:?}"));
    let listed = listed.strip_suffix(']').unwrap_or_else(|| {
        panic!("case {case:?}: expected the set closed by a bracket, got {message:?}")
    });
    listed.split(", ").map(str::to_string).collect()
}

/// A value outside an enumerated type's set is refused with that set listed in the order
/// Config lists it, whatever the submitted value was.
#[tokio::test]
async fn an_out_of_set_frequency_lists_the_set_as_config_lists_it() {
    // The rows differ along the value submitted: a frequency the type does not carry, the
    // wrong casing of one it does, one spelled without the underscore, a value far longer
    // than any of them, and the empty string.
    for value in [
        "One_Minute",
        "one_hour",
        "TwentyFourHours",
        "Six_Hours_And_Then_Some_More_Hours",
        "",
    ] {
        let emulator = Emulator::new();
        let body = json!({
            "DeliveryChannel": {
                "name": "default",
                "s3BucketName": "config-bucket",
                "configSnapshotDeliveryProperties": { "deliveryFrequency": value },
            }
        });
        let message = refusal(&emulator, "PutDeliveryChannel", &body, "ValidationException").await;
        let expected = format!(
            "1 validation error detected: Value '{value}' at \
             'deliveryChannel.configSnapshotDeliveryProperties.deliveryFrequency' failed to \
             satisfy constraint: {}",
            frequency_requirement()
        );
        assert_eq!(
            message, expected,
            "case {value:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// ONE enumerated type, one listing: every member of this model that targets the
/// snapshot-frequency type states the same set in the same order, in whatever operation and
/// at whatever depth it sits.
#[tokio::test]
async fn one_type_is_listed_alike_wherever_a_member_carries_it() {
    // (case, operation, body). Every member targeting `MaximumExecutionFrequency`
    // (`smithy_query(query="shape", shape="ConfigSnapshotDeliveryProperties"|"ConfigRule"
    // |"OrganizationManagedRuleMetadata"|"OrganizationCustomRuleMetadata"
    // |"OrganizationCustomPolicyRuleMetadata")`), each carrying a different value outside
    // it, and each with every other member of its request well formed so the refusal counts
    // one error.
    for (case, operation, body) in [
        (
            "the delivery channel's snapshot frequency",
            "PutDeliveryChannel",
            json!({
                "DeliveryChannel": {
                    "name": "default",
                    "s3BucketName": "config-bucket",
                    "configSnapshotDeliveryProperties": { "deliveryFrequency": "One_Minute" },
                }
            }),
        ),
        (
            "a rule's maximum execution frequency",
            "PutConfigRule",
            json!({
                "ConfigRule": {
                    "ConfigRuleName": "rule-one",
                    "Source": {
                        "Owner": "AWS",
                        "SourceIdentifier": "S3_BUCKET_VERSIONING_ENABLED",
                    },
                    "MaximumExecutionFrequency": "Two_Hours",
                }
            }),
        ),
        (
            "an organization managed rule's",
            "PutOrganizationConfigRule",
            json!({
                "OrganizationConfigRuleName": "org-rule",
                "OrganizationManagedRuleMetadata": {
                    "RuleIdentifier": "IAM_PASSWORD_POLICY",
                    "MaximumExecutionFrequency": "twelve_hours",
                },
            }),
        ),
        (
            "an organization custom rule's",
            "PutOrganizationConfigRule",
            json!({
                "OrganizationConfigRuleName": "org-rule",
                "OrganizationCustomRuleMetadata": {
                    "LambdaFunctionArn":
                        "arn:aws:lambda:us-east-1:000000000000:function:evaluator",
                    "OrganizationConfigRuleTriggerTypes": ["ScheduledNotification"],
                    "MaximumExecutionFrequency": "",
                },
            }),
        ),
        (
            "an organization custom policy rule's",
            "PutOrganizationConfigRule",
            json!({
                "OrganizationConfigRuleName": "org-rule",
                "OrganizationCustomPolicyRuleMetadata": {
                    "PolicyRuntime": "guard-2.x.x",
                    "PolicyText": "rule always_true { 1 == 1 }",
                    "MaximumExecutionFrequency": "48_Hours",
                },
            }),
        ),
    ] {
        let emulator = Emulator::new();
        let message = refusal(&emulator, operation, &body, "ValidationException").await;
        assert!(
            message.starts_with("1 validation error detected: ")
                && message.ends_with(&frequency_requirement()),
            "case {case:?}: expected one clause stating {:?}, got {message:?}",
            frequency_requirement()
        );
    }
}

/// One position an enumerated type is carried at: the case, the operation and the request
/// body that carries a value outside the type.
type Position<'p> = (&'p str, &'p str, Value);

/// One enumerated type as this model carries it: the type's name, the values the model
/// declares for it, and every position a request can carry it at.
type CarriedType<'t> = (&'t str, &'t [&'t str], &'t [Position<'t>]);

/// Another enumerated type is listed the same way: the clause lists exactly the values the
/// MODEL declares for that type, in one order whichever member of whichever operation
/// carried the value outside it.
#[tokio::test]
async fn another_types_set_lists_the_models_values_in_one_order() {
    // (the type, its values as `smithy_query(query="shape", shape=<the type>)` declares
    // them, the positions that carry it: case, operation and body).
    let types: [CarriedType<'_>; 2] = [
        (
            "ComplianceType",
            &[
                "COMPLIANT",
                "NON_COMPLIANT",
                "NOT_APPLICABLE",
                "INSUFFICIENT_DATA",
            ],
            &[
                (
                    "a rule-compliance filter",
                    "DescribeComplianceByConfigRule",
                    json!({ "ComplianceTypes": ["NOT_A_COMPLIANCE_TYPE"] }),
                ),
                (
                    "a resource-compliance filter",
                    "DescribeComplianceByResource",
                    json!({ "ComplianceTypes": ["compliant"] }),
                ),
                (
                    "an external evaluation's verdict",
                    "PutExternalEvaluation",
                    json!({
                        "ConfigRuleName": "rule-one",
                        "ExternalEvaluation": {
                            "ComplianceResourceType": "AWS::::Account",
                            "ComplianceResourceId": "000000000000",
                            "ComplianceType": "MAYBE",
                            "OrderingTimestamp": 1_700_000_000,
                        },
                    }),
                ),
            ],
        ),
        (
            "ChronologicalOrder",
            &["Reverse", "Forward"],
            &[(
                "a configuration-history ordering",
                "GetResourceConfigHistory",
                json!({
                    "resourceType": "AWS::S3::Bucket",
                    "resourceId": "config-bucket",
                    "chronologicalOrder": "Sideways",
                }),
            )],
        ),
    ];

    for (declared_type, declared, positions) in types {
        let mut one_listing: Option<Vec<String>> = None;
        for (case, operation, body) in positions {
            let emulator = Emulator::new();
            let message = refusal(&emulator, operation, body, "ValidationException").await;
            let listed = listed_value_set(case, &message);
            assert!(
                listed.len() == declared.len()
                    && declared
                        .iter()
                        .all(|admitted| listed.iter().any(|value| value == admitted)),
                "case {case:?} ({declared_type}): expected the model's own values \
                 {declared:?}, got {listed:?}"
            );
            match &one_listing {
                Some(first) => assert_eq!(
                    &listed, first,
                    "case {case:?} ({declared_type}): expected the one listing {first:?}, got \
                     {listed:?}"
                ),
                None => one_listing = Some(listed),
            }
        }
    }
}
