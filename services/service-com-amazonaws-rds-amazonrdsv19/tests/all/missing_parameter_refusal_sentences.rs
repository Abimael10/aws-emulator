// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the sentence this front door refuses a request that left a
//! parameter the operation requires out with.
//!
//! The rule: a parameter the request never carried and a parameter carrying a value its
//! member does not accept are two different statements about the request, so this Query
//! front door words them differently under its one `InvalidParameterValue` code (HTTP 400)
//! — an ABSENT parameter is refused by naming the parameter the caller has to supply, with
//! nothing else of the request in the sentence, while a value that WAS sent is reported as
//! the constraint that value broke, with the value, the member it sat at and the
//! requirement.
//!
//! It is a rule of the front door rather than of any one operation: the refusal is raised
//! before a handler runs, so every operation the model marks a member `@required` on
//! answers it the same way. The table below therefore spans what the rule generalises
//! over — reports, creates, modifications, a lifecycle call and a purchase; a first
//! required member and a later one; a string member and a LIST member; and member names
//! spelled plainly, with a leading acronym and with an interior one — and every row is one
//! request that carries every required member of its operation but one.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each
//! assertion reads the XML the emulator itself answered with.

use crate::db_instances::{call, emulator, expect_error, text};

/// The Query API common error a parameter this front door was not given acceptably is
/// refused with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// One request that leaves out a member its operation's input marks `@required`
/// (`smithy_query(query="operation", operation="<the action>",
/// model_path="services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json")`
/// reports `required: true` for each member named below).
struct MissingParameterCase {
    case: &'static str,
    /// The `Action` the request names.
    action: &'static str,
    /// Every OTHER required member of that operation, so the one under test is the only
    /// thing the request is missing.
    params: &'static [(&'static str, &'static str)],
    /// The parameter the refusal has to name: the model's member name as a refusal of this
    /// front door spells a member — its first character lowered.
    parameter: &'static str,
}

/// A subnet of the account, the form `SubnetIds` takes.
const SUBNET_ID: &str = "subnet-0123456789abcdef0";

#[tokio::test]
async fn an_absent_parameter_is_refused_by_naming_the_parameter() {
    let router = emulator();
    let rows = [
        // The option report whose `Engine` "names the database engine to describe DB
        // instance options for" is its one required member.
        MissingParameterCase {
            case: "an option report with nothing at all",
            action: "DescribeOrderableDBInstanceOptions",
            params: &[],
            parameter: "engine",
        },
        // The same report, carrying another member of the same input: what decides the
        // refusal is the member that is MISSING, not how much the request carried.
        MissingParameterCase {
            case: "an option report carrying only the version filter",
            action: "DescribeOrderableDBInstanceOptions",
            params: &[("EngineVersion", "8.0.39")],
            parameter: "engine",
        },
        // A tag write, whose two required members are the resource and the tags: one row
        // for each, so a FIRST required member and a LATER one are both covered, and the
        // second of them is a LIST.
        MissingParameterCase {
            case: "a tag write with no resource",
            action: "AddTagsToResource",
            params: &[("Tags.Tag.1.Key", "owner"), ("Tags.Tag.1.Value", "platform")],
            parameter: "resourceName",
        },
        MissingParameterCase {
            case: "a tag write with no tags",
            action: "AddTagsToResource",
            params: &[(
                "ResourceName",
                "arn:aws:rds:us-west-2:000000000000:db:emu-missing-tags",
            )],
            parameter: "tags",
        },
        // A create with three required members: the name whose spelling starts with the
        // API's acronym, the description, and the subnet list.
        MissingParameterCase {
            case: "a subnet group create with no name",
            action: "CreateDBSubnetGroup",
            params: &[
                ("DBSubnetGroupDescription", "emulator subnets"),
                ("SubnetIds.SubnetIdentifier.1", SUBNET_ID),
            ],
            parameter: "dBSubnetGroupName",
        },
        MissingParameterCase {
            case: "a subnet group create with no description",
            action: "CreateDBSubnetGroup",
            params: &[
                ("DBSubnetGroupName", "emu-missing-description"),
                ("SubnetIds.SubnetIdentifier.1", SUBNET_ID),
            ],
            parameter: "dBSubnetGroupDescription",
        },
        MissingParameterCase {
            case: "a subnet group create with no subnets",
            action: "CreateDBSubnetGroup",
            params: &[
                ("DBSubnetGroupName", "emu-missing-subnets"),
                ("DBSubnetGroupDescription", "emulator subnets"),
            ],
            parameter: "subnetIds",
        },
        // A parameter-group create whose missing member is spelled plainly.
        MissingParameterCase {
            case: "a parameter group create with no description",
            action: "CreateDBParameterGroup",
            params: &[
                ("DBParameterGroupName", "emu-missing-description"),
                ("DBParameterGroupFamily", "mysql8.0"),
            ],
            parameter: "description",
        },
        // A lifecycle call, a modification and a report of a resource, each addressed by
        // the identifier the request did not carry.
        MissingParameterCase {
            case: "a cluster start with nothing at all",
            action: "StartDBCluster",
            params: &[],
            parameter: "dBClusterIdentifier",
        },
        MissingParameterCase {
            case: "an instance modification carrying only a setting",
            action: "ModifyDBInstance",
            params: &[("BackupRetentionPeriod", "7")],
            parameter: "dBInstanceIdentifier",
        },
        MissingParameterCase {
            case: "a log file report carrying only its filter",
            action: "DescribeDBLogFiles",
            params: &[("FilenameContains", "error")],
            parameter: "dBInstanceIdentifier",
        },
        // A purchase, whose required member carries an INTERIOR acronym.
        MissingParameterCase {
            case: "a reserved instance purchase carrying only the count",
            action: "PurchaseReservedDBInstancesOffering",
            params: &[("DBInstanceCount", "1")],
            parameter: "reservedDBInstancesOfferingId",
        },
    ];

    for row in &rows {
        let parameter = row.parameter;
        let body = expect_error(
            row.case,
            call(&router, row.action, row.params).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = format!("Please specify a value for parameter '{parameter}'.");
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_value_that_was_sent_is_refused_as_the_constraint_it_broke() {
    let router = emulator();
    // `ModifyDBInstance$AwsBackupRecoveryPointArn` carries a declared length of 43 to 350
    // and the pattern every backup recovery point ARN matches
    // (`smithy_query(query="operation", operation="ModifyDBInstance")`), so a value of that
    // form shorter than the minimum is a value the member does not accept — the OTHER half
    // of the rule: it was sent, so the refusal reports the constraint it broke rather than
    // asking for a parameter.
    let short_arn = "arn:aws:backup:u:000000000000:a:b";
    let body = expect_error(
        "a modification carrying a backup ARN shorter than the declared minimum",
        call(
            &router,
            "ModifyDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-short-backup-arn"),
                ("AwsBackupRecoveryPointArn", short_arn),
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&body, "Message");
    let expected = format!(
        "1 validation error detected: Value '{short_arn}' at 'awsBackupRecoveryPointArn' \
         failed to satisfy constraint: Member must have length greater than or equal to 43"
    );
    assert_eq!(
        message, expected,
        "case {:?}: expected {expected:?}, got {message:?}",
        "a value outside its member's declared length"
    );
}

#[tokio::test]
async fn the_absent_parameter_is_named_ahead_of_a_value_that_was_sent() {
    let router = emulator();
    // The same modification, now ALSO missing the identifier it addresses: a value can only
    // be held to its member's domain once the request carries the parameters the operation
    // requires, so the refusal asks for the missing one.
    let body = expect_error(
        "a modification missing its identifier and carrying a value outside a declared bound",
        call(
            &router,
            "ModifyDBInstance",
            &[("AwsBackupRecoveryPointArn", "arn:aws:backup:u:000000000000:a:b")],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&body, "Message");
    let expected = "Please specify a value for parameter 'dBInstanceIdentifier'.";
    assert_eq!(
        message, expected,
        "case {:?}: expected {expected:?}, got {message:?}",
        "a request both missing a parameter and carrying a refused value"
    );
}
