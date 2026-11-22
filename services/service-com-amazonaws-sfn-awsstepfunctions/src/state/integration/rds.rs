// SPDX-License-Identifier: CC0-1.0

//! Amazon RDS's own wire facts, which this crate reaches for the reason Amazon S3's and
//! Amazon CloudFront's are reached: a caller names an operation of it through the AWS SDK
//! integration FAMILY, and nothing else.
//!
//! Amazon RDS publishes no optimized integration of its own, so
//! `arn:aws:states:::rds:<action>` names no `Task` state's work — only the family's own
//! `aws-sdk:rds:<operation>` does, which is also how Amazon EventBridge Scheduler's
//! UNIVERSAL target names a call ([`super::aws_sdk_call`]). Its entry is therefore kept
//! out of [`super::APIS`], the table the optimized family is looked up in, and lives here
//! beside the resource its DB instance operations act on, so the one concern sits in one
//! file.
//!
//! Its protocol is `awsQuery`: the operation is named in an `Action` form field beside the
//! service's API version and the input's members are further fields
//! ([`super::Wire::Query`]), so every operation it models is reachable without a
//! per-operation binding.

use super::*;

/// Amazon RDS's ARN namespace, its signing name and its endpoint prefix, which are the
/// same label.
pub(super) const RDS_NAMESPACE: &str = "rds";

/// Amazon RDS, for the calls this crate makes to it: a `Task` state whose `Resource`
/// names one of its operations through the AWS SDK integration family, and an
/// EventBridge Scheduler universal target that names one the same way.
///
/// `services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/
/// com-amazonaws-rds-amazonrdsv19.json` (`smithy_query(query="service_info")`): awsQuery,
/// sigv4Name `rds`, endpointPrefix `rds`, arnNamespace `rds`, sdkId `RDS`, version
/// `2014-10-31`.
pub(super) static RDS_API: Api = Api {
    namespace: RDS_NAMESPACE,
    action_prefix: None,
    signing_name: RDS_NAMESPACE,
    endpoint_prefix: RDS_NAMESPACE,
    wire: Wire::Query {
        version: "2014-10-31",
        // No `(code, shape)` pair is stated for this service: every error of the
        // operations called here is bound to the query code that is its own shape name
        // without the `Fault` suffix the shape carries (`DBInstanceNotFoundFault` ->
        // `DBInstanceNotFound`), which the suffix rule already names a class for
        // ([`Api::exception_of`]).
        exceptions: &[],
    },
    sdk_id: "RDS",
    error_prefix: "RDS",
    // The AWS SDK names the service `Rds`, which its own `Rds` client carries — the SDK
    // id with the capitalisation that SDK gives it.
    sdk_error_prefix: "Rds",
    // The client class `AmazonRDSClient`, whose base exception is `AmazonRDSException`;
    // the model's service shape carries the API's own revision (`AmazonRDSv19`), which
    // the SDK's names do not.
    sdk_client: "AmazonRDS",
    sdk_exception: "AmazonRDSException",
    // No operation of this service models an access error of its own (`StopDBInstance`,
    // `StartDBInstance` and `RebootDBInstance` declare only this service's own faults,
    // `smithy_query(query="operation")`), so a call its caller may not make is answered
    // by AWS's authorization layer in the terms a QUERY-protocol service answers it in:
    // the Query code for an unauthorized call, under a Forbidden.
    access_denied_status: TARGET_FORBIDDEN_STATUS,
    access_denied_code: QUERY_ACCESS_DENIED_CODE,
    lowercases_refused_action: false,
    // `StopDBInstance`'s input: `DBInstanceIdentifier`, `DBSnapshotIdentifier`.
    member_case: MemberCase::Pascal,
    // Its model declares the `awsQuery` protocol itself rather than the compatibility
    // modifier, so an error of it is named by its own query code and there is no second
    // name to ask for.
    query_compatible: false,
    models_errors: true,
};

/// The Query code AWS's authorization layer answers an unauthorized call of a service
/// whose protocol is `awsQuery` with, which is the code such an answer names the class
/// by.
const QUERY_ACCESS_DENIED_CODE: &str = "AccessDenied";

/// A DB INSTANCE, named by the `DBInstanceIdentifier` the instance operations take.
///
/// The member is the instance's own identifier rather than its ARN, and that ARN writes
/// the identifier under the `db` resource type — "the Amazon Resource Name (ARN) for the
/// DB instance" (`DBInstance$DBInstanceArn` of that model, which this emulator's own RDS
/// service mints as `db:<identifier>`) — so the resource an access request over one is
/// taken on is minted from it.
pub(super) const RDS_DB_INSTANCE: TargetNaming = TargetNaming::Named {
    service: RDS_NAMESPACE,
    prefix: "db:",
};

/// The member of each instance operation's request that names the DB instance the call
/// acts on, as that model spells it.
pub(super) const DB_INSTANCE_IDENTIFIER_MEMBER: &str = "DBInstanceIdentifier";

/// The instance operations of this service whose call acts on ONE DB instance, as the AWS
/// SDK integration family names them (`<namespace>:<lowerCamelCase operation>`).
///
/// Each addresses the instance its `DBInstanceIdentifier` names — "the user-supplied
/// instance identifier" for the stop and the start, "the DB instance identifier" for the
/// reboot (`smithy_query(query="doc",shape="StopDBInstanceMessage,
/// StartDBInstanceMessage,RebootDBInstanceMessage",member="DBInstanceIdentifier",
/// model_path="services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/
/// com-amazonaws-rds-amazonrdsv19.json")`) — and publishes its permission under its own
/// name, so the action a call of one is subject to is `rds:<OperationName>` on that
/// instance's ARN. An operation of this service with no entry is one no access request is
/// formed for, and its call is made as before.
pub(super) const STOP_DB_INSTANCE_INTEGRATION: &str = "rds:stopDBInstance";
/// See [`STOP_DB_INSTANCE_INTEGRATION`].
pub(super) const START_DB_INSTANCE_INTEGRATION: &str = "rds:startDBInstance";
/// See [`STOP_DB_INSTANCE_INTEGRATION`].
pub(super) const REBOOT_DB_INSTANCE_INTEGRATION: &str = "rds:rebootDBInstance";

/// The model's OWN spelling of every input member of the instance operations above,
/// which is the spelling a request to this service has to carry
/// ([`super::model_member`]).
///
/// This service's model writes a member whose name opens with — or carries — an ACRONYM
/// with that acronym in capitals (`DBInstanceIdentifier`), while an AWS SDK names the
/// same member by lowercasing it (`dbInstanceIdentifier`), so the two spellings differ in
/// more than the first character and neither [`MemberCase`] produces the other. A member
/// whose name is plain words needs no entry, since case-folding its first character is
/// already both spellings of it.
///
/// `smithy_query(query="operation",operation="StopDBInstance,StartDBInstance,
/// RebootDBInstance",model_path="services/service-com-amazonaws-rds-amazonrdsv19/
/// smithy-model/com-amazonaws-rds-amazonrdsv19.json")`: `StopDBInstanceMessage` takes
/// `DBInstanceIdentifier`, `DBSnapshotIdentifier` and
/// `DetachFromDBCapacityReservation`, `StartDBInstanceMessage` takes
/// `DBInstanceIdentifier` and `DBCapacityReservationIdentifier`, and
/// `RebootDBInstanceMessage` takes `DBInstanceIdentifier` and `ForceFailover`.
pub(super) const RDS_MODEL_MEMBERS: &[&str] = &[
    DB_INSTANCE_IDENTIFIER_MEMBER,
    "DBSnapshotIdentifier",
    "DetachFromDBCapacityReservation",
    "DBCapacityReservationIdentifier",
    "ForceFailover",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A DB instance's ARN is minted from the identifier the request names, under the
    /// `db` resource type and in the Region the call is made in; an identifier already
    /// written as an ARN is the instance itself.
    #[test]
    fn a_db_instances_arn_is_minted_from_the_identifier() {
        let account = ::engine::emulator_account_id();
        // (case, the value the member carries, the Region the call is made in, the ARN)
        let rows: [(&str, &str, &str, Option<String>); 4] = [
            (
                "an identifier named in one Region",
                "orders",
                "eu-west-2",
                Some(format!("arn:aws:rds:eu-west-2:{account}:db:orders")),
            ),
            (
                "the same identifier named in another",
                "orders",
                "us-east-1",
                Some(format!("arn:aws:rds:us-east-1:{account}:db:orders")),
            ),
            (
                "an identifier already written as the instance's ARN",
                "arn:aws:rds:eu-west-2:000000000000:db:orders",
                "us-east-1",
                Some("arn:aws:rds:eu-west-2:000000000000:db:orders".to_string()),
            ),
            ("no identifier at all", "", "eu-west-2", None),
        ];
        for (case, named, region, expected) in rows {
            let arn = RDS_DB_INSTANCE.arn_of(named, region);
            assert_eq!(
                arn, expected,
                "case {case:?}: expected {expected:?}, got {arn:?}"
            );
        }
    }

    /// Every integration this module names an instance operation by is written the way
    /// the AWS SDK family names a call: the service's namespace and the operation in
    /// lowerCamelCase.
    #[test]
    fn each_instance_integration_names_an_operation_of_this_service() {
        for integration in [
            STOP_DB_INSTANCE_INTEGRATION,
            START_DB_INSTANCE_INTEGRATION,
            REBOOT_DB_INSTANCE_INTEGRATION,
        ] {
            let (service, operation) = integration
                .split_once(':')
                .unwrap_or_else(|| panic!("{integration} names a service and an operation"));
            assert_eq!(
                service, RDS_NAMESPACE,
                "case {integration:?}: expected the service {RDS_NAMESPACE:?}, got {service:?}"
            );
            assert!(
                operation.starts_with(char::is_lowercase),
                "case {integration:?}: expected a lowerCamelCase operation, got {operation:?}"
            );
        }
    }
}
