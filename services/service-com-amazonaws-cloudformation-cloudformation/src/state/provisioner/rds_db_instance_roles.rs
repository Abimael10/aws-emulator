// SPDX-License-Identifier: CC0-1.0

//! `AWS::RDS::DBInstance`'s half of the role-association record: the calls a DB INSTANCE's
//! `AssociatedRoles` are made and read with, taken by the one walk beside it
//! ([`super::rds_role_associations`]), which is where the rule they serve is stated.
//!
//! The DB instance is the type with no other way to carry the property at all:
//! `CreateDBInstance` has no `AssociatedRoles` member and `ModifyDBInstance` has none either,
//! so EVERY association an instance's declaration asks for is made by the call that
//! associates one. The cluster's record sits beside its own arms
//! ([`super::rds_db_clusters::DB_CLUSTER_ROLE_CALLS`]).

use super::*;

/// How a DB instance's associations are made and read: `AddRoleToDBInstance` makes one, and
/// `DescribeDBInstances` reports the ones in place under the element
/// `DBInstance$AssociatedRoles`' own member fixes (`smithy_query(query="shape",
/// shape="DBInstanceRoles")`).
pub(crate) const DB_INSTANCE_ROLE_CALLS: rds_role_associations::RoleAssociationCalls =
    rds_role_associations::RoleAssociationCalls {
        identifier_member: "DBInstanceIdentifier",
        associate: "AddRoleToDBInstance",
        describe: "DescribeDBInstances",
        entry_element: "DBInstanceRole",
        not_found: DB_INSTANCE_NOT_FOUND,
    };
