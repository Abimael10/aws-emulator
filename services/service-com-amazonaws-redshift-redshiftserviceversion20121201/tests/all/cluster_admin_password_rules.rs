// SPDX-License-Identifier: CC0-1.0

//! What an admin password a cluster write carries has to satisfy, end to end over
//! the awsQuery wire.
//!
//! The rule under test: **every constraint the service publishes for
//! `MasterUserPassword` is checked against the value the request carries before
//! the service looks at anything the account holds, and a password outside one of
//! them is refused with `InvalidParameterValue` naming that constraint — so a
//! refused create leaves no cluster behind for a later read or delete to answer
//! from.** Its companion: **a cluster is created with admin credentials or not at
//! all**, so a create that neither sets a password nor asks Amazon Redshift to
//! manage one is refused too. Its ordering half: **a request is refused on its
//! OWN contents — the values it carries and the resource names it references —
//! before the identifier it asks for is compared with what the account already
//! holds**, so a caller is never told to pick another cluster name for a defect
//! that is in the request.
//!
//! The constraints come from `smithy_query(query="doc",
//! shape="CreateClusterMessage", member="MasterUserPassword")`: "Must be between 8
//! and 64 characters in length", "Must contain at least one uppercase letter",
//! "Must contain at least one lowercase letter", "Must contain one number", and
//! "Can be any printable ASCII character (ASCII code 33-126) except `'` (single
//! quote), `"` (double quote), `\`, `/`, or `@`".
//!
//! The rows span the classes the rule generalises over rather than one request:
//! each published bound of the length band and one character past it, each missing
//! content class, every excluded character and both ways of leaving the printable
//! band — beside the passwords at the EDGE of each constraint that must be
//! ACCEPTED, because a rule that refuses everything is not the rule. Every row
//! drives a real HTTP request through the same `MultiServiceRouter` the production
//! router builds (the harness is shared with [`crate::cluster_lifecycle`]).

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};

/// What a password shorter than the published floor is refused with.
const TOO_SHORT: &str = "Invalid master password. Password should be atleast 8 characters long.";

/// What a password longer than the published ceiling is refused with.
const TOO_LONG: &str = "Invalid master password. Password should be atmost 64 characters long.";

/// What a password carrying no decimal digit is refused with.
const NO_DIGIT: &str = "The parameter MasterUserPassword must contain at least 1 decimal digit.";

/// What a password carrying no uppercase letter is refused with.
const NO_UPPERCASE: &str =
    "The parameter MasterUserPassword must contain at least 1 uppercase letter.";

/// What a password carrying no lowercase letter is refused with.
const NO_LOWERCASE: &str =
    "The parameter MasterUserPassword must contain at least 1 lowercase letter.";

/// What a password carrying a character the member does not admit is refused
/// with: the sentence names the admitted set, never the password.
const NOT_ADMITTED_CHARACTER: &str = r#"The parameter MasterUserPassword is not a valid password. Only printable ASCII characters except for '/', '@', '"', ' ', '\', ''' may be used."#;

/// The error code every one of these refusals carries.
const REFUSAL_CODE: &str = "InvalidParameterValue";

/// A password of exactly `length` characters that satisfies every OTHER
/// constraint: one of each content class, padded with a character the member
/// admits.
fn password_of_length(length: usize) -> String {
    let mut password = String::from("Aa1");
    while password.chars().count() < length {
        password.push('x');
    }
    password
}

#[tokio::test]
async fn a_cluster_is_created_with_admin_credentials_or_not_at_all() {
    let emulator = Emulator::new();

    // A create carries admin credentials one of two ways — the password it sets,
    // or Amazon Redshift managing them — and the rows span both, their absence,
    // and the combination that asks for both at once.
    let refused: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("neither member named at all", vec![]),
        (
            "the managed credentials declined, with no password set",
            vec![("ManageMasterPassword", "false")],
        ),
    ];
    for (index, (case, overrides)) in refused.iter().enumerate() {
        let identifier = format!("credentials-refused-{index:02}");
        let mut params: Vec<(&str, &str)> = vec![
            ("ClusterIdentifier", &identifier),
            ("NodeType", "ra3.xlplus"),
            ("MasterUsername", "admin"),
        ];
        params.extend(overrides.iter().copied());
        let answer = emulator.call("CreateCluster", &params).await;
        assert_eq!(
            (answer.status, answer.error_code().as_deref()),
            (400, Some(REFUSAL_CODE)),
            "case {case}: expected 400 {REFUSAL_CODE}, got {} / {:?} with body {}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        assert_eq!(
            answer.text("Message").as_deref(),
            Some(state::ADMIN_CREDENTIAL_REQUIRED),
            "case {case}: expected the sentence that names both ways of setting them, got {:?}",
            answer.text("Message")
        );
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", &identifier)])
            .await;
        assert_eq!(
            (described.status, described.error_code().as_deref()),
            (404, Some("ClusterNotFound")),
            "case {case}: expected the refused create to leave no cluster, got {} / {:?}",
            described.status,
            described.error_code()
        );
    }

    // Asking Amazon Redshift to manage them is the other way of carrying them:
    // the create is served, and the secret it manages them in is reported.
    let managed = emulator
        .call(
            "CreateCluster",
            &[
                ("ClusterIdentifier", "credentials-managed"),
                ("NodeType", "ra3.xlplus"),
                ("MasterUsername", "admin"),
                ("ManageMasterPassword", "true"),
            ],
        )
        .await;
    assert_eq!(
        managed.status,
        200,
        "case the managed credentials: expected the create to be served, got {} / {:?} with body {}",
        managed.status,
        managed.error_code(),
        managed.body
    );
    assert!(
        managed.text("MasterPasswordSecretArn").is_some(),
        "case the managed credentials: expected the secret they are managed in, got body {}",
        managed.body
    );

    // Both at once is neither: the request contradicts itself.
    let both = emulator
        .call(
            "CreateCluster",
            &[
                ("ClusterIdentifier", "credentials-both"),
                ("NodeType", "ra3.xlplus"),
                ("MasterUsername", "admin"),
                ("ManageMasterPassword", "true"),
                ("MasterUserPassword", "Managed-Passw0rd"),
            ],
        )
        .await;
    assert_eq!(
        both.status, 400,
        "case both ways at once: expected the create to be refused, got {} / {:?} with body {}",
        both.status,
        both.error_code(),
        both.body
    );
}

/// `CreateCluster` for `identifier` with the three required members and
/// `password`.
async fn create_with_password(emulator: &Emulator, identifier: &str, password: &str) -> Answer {
    emulator
        .call(
            "CreateCluster",
            &[
                ("ClusterIdentifier", identifier),
                ("NodeType", "ra3.xlplus"),
                ("MasterUsername", "admin"),
                ("MasterUserPassword", password),
            ],
        )
        .await
}

/// Every password the service must refuse, and the sentence it is refused with.
///
/// The length rows name each published bound of
/// [`state::MASTER_PASSWORD_LENGTH_BOUNDS`] and one character past it; the rest
/// span the content classes and both ways of leaving the printable ASCII band.
fn refused_rows() -> Vec<(&'static str, String, &'static str)> {
    let (floor, ceiling) = state::MASTER_PASSWORD_LENGTH_BOUNDS;
    let mut rows: Vec<(&'static str, String, &'static str)> = vec![
        (
            "one character short of the published floor",
            password_of_length(floor - 1),
            TOO_SHORT,
        ),
        ("a single character", "A".to_string(), TOO_SHORT),
        (
            "one character past the published ceiling",
            password_of_length(ceiling + 1),
            TOO_LONG,
        ),
        (
            "far past the published ceiling",
            password_of_length(ceiling * 2),
            TOO_LONG,
        ),
        (
            "a password carrying no decimal digit",
            "Password".to_string(),
            NO_DIGIT,
        ),
        (
            "a long password carrying no decimal digit",
            "No-Digit-Anywhere-In-Here".to_string(),
            NO_DIGIT,
        ),
        (
            "a password carrying no uppercase letter",
            "passw0rd-only".to_string(),
            NO_UPPERCASE,
        ),
        (
            "a password carrying no lowercase letter",
            "PASSW0RD-ONLY".to_string(),
            NO_LOWERCASE,
        ),
        (
            "a space, which the excluded set names",
            "Passw0rd xy".to_string(),
            NOT_ADMITTED_CHARACTER,
        ),
        (
            "a tab, which is below the printable band",
            "Passw0rd\txy".to_string(),
            NOT_ADMITTED_CHARACTER,
        ),
        (
            "a character beyond ASCII",
            "Passw0rd\u{e9}xy".to_string(),
            NOT_ADMITTED_CHARACTER,
        ),
    ];
    // One row per character the published set excludes, so no excluded character
    // is admitted by a rule that only happens to catch the others.
    for excluded in state::MASTER_PASSWORD_EXCLUDED_CHARACTERS {
        rows.push((
            "an excluded punctuation character",
            format!("Passw0rd{excluded}xy"),
            NOT_ADMITTED_CHARACTER,
        ));
    }
    rows
}

#[tokio::test]
async fn a_password_outside_a_published_constraint_is_refused_and_provisions_nothing() {
    // The band the length rows are built from is the one the service publishes.
    assert_eq!(
        state::MASTER_PASSWORD_LENGTH_BOUNDS,
        (8, 64),
        "the published length band"
    );
    let emulator = Emulator::new();

    for (index, (case, password, message)) in refused_rows().into_iter().enumerate() {
        let identifier = format!("pw-refused-{index:02}");
        let answer = create_with_password(&emulator, &identifier, &password).await;
        assert_eq!(
            (answer.status, answer.error_code().as_deref()),
            (400, Some(REFUSAL_CODE)),
            "case {case} ({password:?}): expected 400 {REFUSAL_CODE}, got {} / {:?} with body {}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        assert_eq!(
            answer.text("Message").as_deref(),
            Some(message),
            "case {case} ({password:?}): expected the message to name the broken constraint, got {:?}",
            answer.text("Message")
        );
        // The refused create provisioned NOTHING, so the reads that follow it
        // answer as they would for an identifier nobody ever asked for.
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", &identifier)])
            .await;
        assert_eq!(
            (described.status, described.error_code().as_deref()),
            (404, Some("ClusterNotFound")),
            "case {case}: expected the refused create to leave no cluster, got {} / {:?} with body {}",
            described.status,
            described.error_code(),
            described.body
        );
        assert_eq!(
            described.text("Message").as_deref(),
            Some(format!("Cluster {identifier} not found.").as_str()),
            "case {case}: expected the read to name the identifier it resolved nothing for, got {:?}",
            described.text("Message")
        );
        let deleted = emulator
            .call(
                "DeleteCluster",
                &[
                    ("ClusterIdentifier", &identifier),
                    ("SkipFinalClusterSnapshot", "true"),
                ],
            )
            .await;
        assert_eq!(
            (deleted.status, deleted.error_code().as_deref()),
            (404, Some("ClusterNotFound")),
            "case {case}: expected the delete to resolve no cluster, got {} / {:?} with body {}",
            deleted.status,
            deleted.error_code(),
            deleted.body
        );
    }
}

#[tokio::test]
async fn a_password_at_the_edge_of_every_constraint_is_accepted() {
    let emulator = Emulator::new();
    let (floor, ceiling) = state::MASTER_PASSWORD_LENGTH_BOUNDS;

    let rows: Vec<(&str, String)> = vec![
        ("exactly at the published floor", password_of_length(floor)),
        (
            "exactly at the published ceiling",
            password_of_length(ceiling),
        ),
        (
            "every punctuation character the band admits beside the excluded ones",
            // ASCII 33-126 in order, less the five characters the published set
            // excludes and the alphanumerics the classes above already cover.
            (33u8..=126)
                .map(char::from)
                .filter(|character| {
                    !character.is_ascii_alphanumeric()
                        && !state::MASTER_PASSWORD_EXCLUDED_CHARACTERS.contains(character)
                })
                .collect::<String>()
                + "Aa1",
        ),
        (
            "the first and last character of the printable band",
            "!Passw0rd~".to_string(),
        ),
    ];

    for (index, (case, password)) in rows.iter().enumerate() {
        // The ceiling row is as long as the band allows, so its length is part of
        // what the row asserts.
        assert!(
            password.chars().count() >= floor && password.chars().count() <= ceiling,
            "case {case}: the row must stay inside the published band, got {} characters",
            password.chars().count()
        );
        let identifier = format!("pw-accepted-{index:02}");
        let answer = create_with_password(&emulator, &identifier, password).await;
        assert_eq!(
            answer.status,
            200,
            "case {case}: expected the create to be served, got {} / {:?} with body {}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        // Read back through the implemented describe: the cluster the accepted
        // create provisioned is there, and the password it carried is not on the
        // wire.
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", &identifier)])
            .await;
        assert_eq!(
            described.text("ClusterIdentifier").as_deref(),
            Some(identifier.as_str()),
            "case {case}: expected the accepted create to be stored, got body {}",
            described.body
        );
        assert!(
            !described.body.contains(password.as_str()),
            "case {case}: expected the read to carry no password, got body {}",
            described.body
        );
    }
}

/// One row of the ordering table: the case, the members it carries beyond the
/// three required ones, and the `(status, code, message)` the service must
/// answer it with instead of the name collision.
struct Ordered {
    case: &'static str,
    overrides: Vec<(&'static str, &'static str)>,
    status: u16,
    code: &'static str,
    message: String,
}

#[tokio::test]
async fn a_request_is_refused_on_its_own_contents_before_the_account_is_consulted() {
    let emulator = Emulator::new();

    // One cluster the account really holds, so the duplicate-identifier lookup
    // has something to find.
    let held = "pw-order-held";
    let created = create_with_password(&emulator, held, "Order-Passw0rd").await;
    assert_eq!(
        created.status, 200,
        "case seed the held cluster: expected 200, got {} with body {}",
        created.status, created.body
    );

    // The SAME identifier, re-used by a request whose own contents the service
    // would not accept: each row must answer the defect in the request, not the
    // name collision.
    let rows: Vec<Ordered> = vec![
        Ordered {
            case: "a password carrying no decimal digit",
            overrides: vec![("MasterUserPassword", "Password")],
            status: 400,
            code: REFUSAL_CODE,
            message: NO_DIGIT.to_string(),
        },
        Ordered {
            case: "a password below the published floor",
            overrides: vec![("MasterUserPassword", "Pass1Aa")],
            status: 400,
            code: REFUSAL_CODE,
            message: TOO_SHORT.to_string(),
        },
        Ordered {
            case: "a parameter group name that resolves to nothing",
            overrides: vec![
                ("MasterUserPassword", "Order-Passw0rd"),
                ("ClusterParameterGroupName", "pw-order-no-such-group"),
            ],
            status: 404,
            code: "ClusterParameterGroupNotFound",
            message: "ClusterParameterGroup not found: pw-order-no-such-group".to_string(),
        },
        Ordered {
            case: "a subnet group name that resolves to nothing",
            overrides: vec![
                ("MasterUserPassword", "Order-Passw0rd"),
                ("ClusterSubnetGroupName", "pw-order-no-such-subnet-group"),
            ],
            status: 400,
            code: "ClusterSubnetGroupNotFoundFault",
            message: "Cluster subnet group 'pw-order-no-such-subnet-group' does not exist."
                .to_string(),
        },
        Ordered {
            case: "a node count the cluster type cannot describe",
            overrides: vec![
                ("MasterUserPassword", "Order-Passw0rd"),
                ("ClusterType", "multi-node"),
                ("NumberOfNodes", "1"),
            ],
            status: 400,
            code: REFUSAL_CODE,
            message: state::MULTI_NODE_COUNT_REFUSAL.to_string(),
        },
    ];

    for row in &rows {
        let Ordered {
            case,
            overrides,
            status,
            code,
            message,
        } = row;
        let mut params: Vec<(&str, &str)> = vec![
            ("ClusterIdentifier", held),
            ("NodeType", "ra3.xlplus"),
            ("MasterUsername", "admin"),
        ];
        params.extend(overrides.iter().copied());
        let answer = emulator.call("CreateCluster", &params).await;
        assert_eq!(
            (answer.status, answer.error_code().as_deref()),
            (*status, Some(*code)),
            "case {case}: expected {status} {code} rather than the name collision, got {} / {:?} with body {}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        assert_eq!(
            answer.text("Message").as_deref(),
            Some(message.as_str()),
            "case {case}: expected the message to name the defect in the request, got {:?}",
            answer.text("Message")
        );
    }

    // The duplicate-identifier lookup still answers a request the service has
    // nothing else to refuse: the ordering moved it, it did not remove it.
    let duplicate = create_with_password(&emulator, held, "Order-Passw0rd").await;
    assert_eq!(
        (duplicate.status, duplicate.error_code().as_deref()),
        (400, Some("ClusterAlreadyExists")),
        "case a valid repeat create: expected 400 ClusterAlreadyExists, got {} / {:?} with body {}",
        duplicate.status,
        duplicate.error_code(),
        duplicate.body
    );
}

#[tokio::test]
async fn the_node_count_refusal_says_whether_the_count_was_missing() {
    let emulator = Emulator::new();

    // The multi-node shape needs a count, and that count has a floor: a request
    // that NAMES one too small is told about the floor, while one that names
    // none is told the member is required as well.
    for (case, count, message) in [
        (
            "no count named at all",
            None,
            "Number of nodes must be supplied for cluster type multi-node and must be greater \
             than or equal to 2.",
        ),
        (
            "the one-node count",
            Some("1"),
            "Number of nodes for cluster type multi-node must be greater than or equal to 2.",
        ),
        (
            "no node at all",
            Some("0"),
            "Number of nodes for cluster type multi-node must be greater than or equal to 2.",
        ),
    ] {
        let identifier = format!(
            "nodes-{}",
            count
                .unwrap_or("none")
                .replace(|c: char| !c.is_alphanumeric(), "")
        );
        let mut params: Vec<(&str, &str)> = vec![
            ("ClusterIdentifier", &identifier),
            ("NodeType", "ra3.xlplus"),
            ("MasterUsername", "admin"),
            ("MasterUserPassword", "Nodes-Passw0rd"),
            ("ClusterType", "multi-node"),
        ];
        if let Some(count) = count {
            params.push(("NumberOfNodes", count));
        }
        let answer = emulator.call("CreateCluster", &params).await;
        assert_eq!(
            (answer.status, answer.error_code().as_deref()),
            (400, Some(REFUSAL_CODE)),
            "case {case}: expected 400 {REFUSAL_CODE}, got {} / {:?} with body {}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        assert_eq!(
            answer.text("Message").as_deref(),
            Some(message),
            "case {case}: expected the sentence for this half of the rule, got {:?}",
            answer.text("Message")
        );
    }
}

#[tokio::test]
async fn a_modify_applies_the_same_password_rule_as_the_create() {
    let emulator = Emulator::new();

    let identifier = "pw-modified";
    let created = create_with_password(&emulator, identifier, "Modify-Passw0rd").await;
    assert_eq!(
        created.status, 200,
        "case seed the cluster: expected 200, got {} with body {}",
        created.status, created.body
    );

    // The pending change the seeding create left in flight: a refused modify must
    // leave exactly this, whatever it asked for. The clock is stopped first, so
    // the provisioning span stays open across every call below however long the
    // host takes over them.
    emulator.stop_clock();
    let before = emulator
        .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
        .await;
    let pending_before = before.text("MasterUserPassword");

    // The same classes the create refuses, refused by the operation that CHANGES
    // the password — the one rule, not two.
    for (case, password, message) in [
        ("no decimal digit", "Password", NO_DIGIT),
        ("below the published floor", "Pass1Aa", TOO_SHORT),
        ("no uppercase letter", "passw0rd-only", NO_UPPERCASE),
        ("an excluded character", "Passw0rd@xy", NOT_ADMITTED_CHARACTER),
    ] {
        let answer = emulator
            .call(
                "ModifyCluster",
                &[
                    ("ClusterIdentifier", identifier),
                    ("MasterUserPassword", password),
                ],
            )
            .await;
        assert_eq!(
            (answer.status, answer.error_code().as_deref()),
            (400, Some(REFUSAL_CODE)),
            "case {case}: expected 400 {REFUSAL_CODE}, got {} / {:?} with body {}",
            answer.status,
            answer.error_code(),
            answer.body
        );
        assert_eq!(
            answer.text("Message").as_deref(),
            Some(message),
            "case {case}: expected the message to name the broken constraint, got {:?}",
            answer.text("Message")
        );
        // The refused modify changed nothing: the password the request carried
        // was never taken, and nothing it asked for is in flight.
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            described.text("MasterUserPassword"),
            pending_before,
            "case {case}: expected the refused modify to leave the cluster as it was, got body {}",
            described.body
        );
    }

    // A compliant password IS applied, and is reported as the mask rather than as
    // itself.
    let accepted = emulator
        .call(
            "ModifyCluster",
            &[
                ("ClusterIdentifier", identifier),
                ("MasterUserPassword", "An0ther-Passw0rd"),
            ],
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "case a compliant password: expected 200, got {} / {:?} with body {}",
        accepted.status,
        accepted.error_code(),
        accepted.body
    );
    assert_eq!(
        accepted.text("MasterUserPassword").as_deref(),
        Some(state::REDACTED_MASTER_USER_PASSWORD),
        "case a compliant password: expected the pending change to be reported as the mask, got {:?}",
        accepted.text("MasterUserPassword")
    );
}
