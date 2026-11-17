// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `MasterUsername` values Amazon RDS reserves for the engine.
//!
//! The rule under test: a `MasterUsername` naming one of the accounts Amazon RDS keeps for
//! ITSELF on the database it provisions cannot be the master user, so the create is refused
//! with `InvalidParameterValue` and the sentence AWS words it in — and because the refusal is
//! decided before anything is written, the resource the request named exists nowhere
//! afterwards: a later read answers the modelled not-found fault rather than reporting a
//! resource AWS would never have created.
//!
//! The table spans what the rule generalises over: every reserved account this front door
//! publishes, each in another case (the engines fold an identifier's case), both operations
//! that take a `MasterUsername` for a resource of their own, and a username that is NOT
//! reserved as the control row that must still be accepted.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly as
//! the production router mounts it, each request is a real awsQuery form POST carrying the
//! SigV4 credential scope the router keys on, and every assertion reads the XML the emulator
//! answered with.

use service_com_amazonaws_rds_amazonrdsv19::state::RESERVED_MASTER_USERNAMES;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts};

/// The error code Amazon RDS answers a parameter value its member does not accept with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// A master user password every row carries, so the refusal under test is the only one in
/// play: it satisfies the engines' documented password policy.
const PASSWORD: &str = "Emulator-Probe-1";

/// Which resource a row creates with the `MasterUsername` under test.
#[derive(Debug, Clone, Copy)]
enum Subject {
    /// `CreateDBCluster`, read back through `DescribeDBClusters`.
    Cluster,
    /// `CreateDBInstance`, read back through `DescribeDBInstances`.
    Instance,
}

impl Subject {
    /// The create this subject is made by, and the members it needs beside the username.
    fn create(self, identifier: &str, username: &str) -> (&'static str, Vec<(String, String)>) {
        let mut params: Vec<(String, String)> = vec![
            ("MasterUsername".to_string(), username.to_string()),
            ("MasterUserPassword".to_string(), PASSWORD.to_string()),
        ];
        match self {
            Subject::Cluster => {
                params.push(("DBClusterIdentifier".to_string(), identifier.to_string()));
                params.push(("Engine".to_string(), "aurora-postgresql".to_string()));
                ("CreateDBCluster", params)
            }
            Subject::Instance => {
                params.push(("DBInstanceIdentifier".to_string(), identifier.to_string()));
                params.push(("Engine".to_string(), "postgres".to_string()));
                params.push(("DBInstanceClass".to_string(), "db.t3.micro".to_string()));
                params.push(("AllocatedStorage".to_string(), "20".to_string()));
                ("CreateDBInstance", params)
            }
        }
    }

    /// The read that answers whether the resource exists, the member it is named by, the
    /// error code its not-found fault carries and the sentence that fault words it in.
    fn read(self, identifier: &str) -> (&'static str, (String, String), &'static str, String) {
        match self {
            Subject::Cluster => (
                "DescribeDBClusters",
                (
                    "DBClusterIdentifier".to_string(),
                    identifier.to_string(),
                ),
                "DBClusterNotFoundFault",
                format!("DBCluster {identifier} not found."),
            ),
            Subject::Instance => (
                "DescribeDBInstances",
                (
                    "DBInstanceIdentifier".to_string(),
                    identifier.to_string(),
                ),
                "DBInstanceNotFound",
                format!("DBInstance {identifier} not found."),
            ),
        }
    }
}

/// Every reserved account, each under both subjects and in both an exact and an upper-case
/// spelling: the create is refused with AWS's sentence, and the resource it named is nowhere
/// afterwards.
#[tokio::test]
async fn a_reserved_master_username_is_refused_and_creates_nothing() {
    let router = emulator();

    for (index, reserved) in RESERVED_MASTER_USERNAMES.iter().enumerate() {
        // The engines fold an identifier's case, so a differently-cased spelling names the
        // same reserved account and is refused the same way, echoing the caller's own
        // spelling back.
        for (spelling, username) in [
            ("as published", (*reserved).to_string()),
            ("upper-cased", reserved.to_ascii_uppercase()),
        ] {
            for (position, subject) in [Subject::Cluster, Subject::Instance].iter().enumerate() {
                let identifier = format!("emu-reserved-{index}-{position}-{spelling}")
                    .replace(' ', "-");
                let case = format!("{subject:?} with MasterUsername {username:?} ({spelling})");

                let (action, params) = subject.create(&identifier, &username);
                let refused = expect_error(
                    &case,
                    call(&router, action, &params).await,
                    400,
                    INVALID_PARAMETER_VALUE,
                );
                assert_eq!(
                    text(&refused, "Message"),
                    format!(
                        "MasterUsername {username} cannot be used as it is a reserved word \
                         used by the engine"
                    ),
                    "case {case:?}: expected the reserved-word refusal naming the username, \
                     got {refused}"
                );
                assert_eq!(
                    text(&refused, "Type"),
                    "Sender",
                    "case {case:?}: the refusal is the caller's, got {refused}"
                );

                // The refused create wrote nothing: the identifier names no resource.
                let (read, (member, value), code, message) = subject.read(&identifier);
                let missing = expect_error(
                    &case,
                    call(&router, read, &[(member, value)]).await,
                    404,
                    code,
                );
                assert_eq!(
                    text(&missing, "Message"),
                    message,
                    "case {case:?}: expected the not-found fault naming the resource, got \
                     {missing}"
                );
            }
        }
    }
}

/// The control row: a username the engine does not reserve is accepted, and the resource it
/// created reports it back.
#[tokio::test]
async fn a_username_the_engine_does_not_reserve_is_accepted() {
    let router = emulator();

    for (subject, identifier, username) in [
        (Subject::Cluster, "emu-allowed-cluster", "rdsadministrator"),
        (Subject::Instance, "emu-allowed-instance", "pgadmin"),
    ] {
        let case = format!("{subject:?} with MasterUsername {username:?}");
        let (action, params) = subject.create(identifier, username);
        let created = expect_ok(&case, call(&router, action, &params).await);
        assert!(
            texts(&created, "MasterUsername").contains(&username.to_string()),
            "case {case:?}: the resource must report the master user it was created with, \
             got {created}"
        );

        let (read, (member, value), _, _) = subject.read(identifier);
        let described = expect_ok(&case, call(&router, read, &[(member, value)]).await);
        assert!(
            texts(&described, "MasterUsername").contains(&username.to_string()),
            "case {case:?}: the stored resource must report its master user, got {described}"
        );
    }
}
