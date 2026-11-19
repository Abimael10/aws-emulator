// SPDX-License-Identifier: CC0-1.0

//! The two halves of RDS for PostgreSQL's Amazon S3 import, in ONE chain: the control-plane
//! association that decides WHETHER the engine has credentials for the feature, and the
//! `aws_s3` extension's own function on the instance's native endpoint that USES them.
//!
//! The rule this pins, asserted over a table of trust-policy forms rather than over the one
//! role that showed it:
//!
//! * An association for an engine feature is recorded ONLY for a role whose stored trust
//!   policy lets the service principal Amazon RDS acts under perform `sts:AssumeRole` on it.
//!   Every other document is refused `InvalidParameterValue` with the one fixed sentence that
//!   names nothing of the request, and records nothing — the resource reports no
//!   `AssociatedRoles` entry for it.
//! * The extension's function is part of the ENGINE, not of the association: with no
//!   association in force the identical `aws_s3.table_import_from_s3(...)` call still PARSES
//!   and RUNS, and fails where the extension reads the feature's credentials — the
//!   insufficient-privilege condition, never the syntax one — and loads nothing.
//! * The moment a role the principal may assume is associated for the feature and the report
//!   has it `ACTIVE`, that identical call imports the object: it answers the extension's own
//!   summary of what it imported, and the object's records are rows of the table.
//!
//! So the negative control (a role whose trust policy omits the principal cannot be
//! associated) and its positive control (a correctly trusted role makes the import work) are
//! decided over one instance, one object and one statement, with only the role's trust
//! document differing.
//!
//! Amazon S3 and IAM are mounted beside this service on ONE engine, exactly as the assembled
//! router mounts them: the object the import reads was written through Amazon S3's own
//! `PutObject` and every role is a role IAM holds, created with `CreateRole`/`PutRolePolicy`.
//! The client is the PostgreSQL protocol client the sibling endpoint suites drive, so what is
//! exercised is what a stock driver sends.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_error, text, REGION};
use crate::postgres_aws_s3_import_export::{
    associate, available_postgres, disassociate, get_object_policy, refusal, rows_of, run,
    session_with_the_extension, BUCKET,
};
use crate::rds_s3_integration::{
    create_bucket, emulator, instance_associated_roles, put_object, role_with_trust_document,
};

/// The feature name the import runs under, which `DescribeDBEngineVersions` publishes for the
/// PostgreSQL engines as a `SupportedFeatureNames` entry.
const FEATURE: &str = "s3Import";

/// The status an association that holds is reported under.
const ACTIVE: &str = "ACTIVE";

/// The sentence a role Amazon RDS cannot assume is refused with — entirely fixed text,
/// carrying neither the role ARN the request named nor the principal the document is missing.
const UNTRUSTED_ROLE: &str = "IAM role ARN value is invalid or does not include the \
                              required permissions for: AWS_ROLE_INTEGRATION";

/// What the extension answers when it cannot read the feature's credentials, under the token
/// it writes the feature with.
const MISSING_CREDENTIALS: &str = "credentials for the s3import feature could not be accessed";

/// The SQLSTATE a PostgreSQL server reports an insufficient privilege under, which is the
/// class the extension's credentials condition belongs to.
///
/// Source: the PostgreSQL error-code table — class 42, `insufficient_privilege`.
const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// And the SQLSTATE a statement its grammar cannot read is reported under
/// (`syntax_error`), which a call to a function the engine HAS is never answered with.
const SYNTAX_ERROR: &str = "42601";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One row of the trust-policy table: the document the role carries, and whether it lets the
/// principal Amazon RDS acts under assume that role.
struct TrustRow {
    case: &'static str,
    /// The trust policy, as a client sends it to `CreateRole`.
    document: &'static str,
    /// Whether that document grants `rds.amazonaws.com` the `sts:AssumeRole` the association
    /// needs.
    assumable: bool,
}

/// The trust-policy forms, spanning what the decision generalises over: which principal the
/// statement names, how many it names, which action it grants, and what effect it states.
///
/// Source for the form a working document takes: the trust policy of "Setting up access to an
/// Amazon S3 bucket" (RDS for PostgreSQL), which grants `sts:AssumeRole` to the service
/// principal. The rest is the policy language's own evaluation — an explicit `Deny` is final,
/// a statement about another action is about something else, and an action wildcard covers the
/// actions it prefixes — so the rows span those as well, each on a document no row the sibling
/// suite drives carries.
///
/// The two rows naming another service principal overlap what
/// `rds_s3_integration::a_role_that_does_not_trust_rds_cannot_be_associated` drives on the
/// control plane alone; they are kept because they are the class the issue's own negative
/// control belongs to, and here each of them is carried through to the ENGINE-side call.
const TRUST_ROWS: &[TrustRow] = &[
    TrustRow {
        case: "the documented trust policy",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
        assumable: true,
    },
    TrustRow {
        case: "a role trusting another service only",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
        assumable: false,
    },
    TrustRow {
        case: "a role trusting the snapshot-export principal only",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"export.rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
        assumable: false,
    },
    TrustRow {
        case: "a statement naming several service principals, one of them the right one",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":["lambda.amazonaws.com","rds.amazonaws.com"]},"Action":"sts:AssumeRole"}]}"#,
        assumable: true,
    },
    TrustRow {
        case: "a role granting the principal another STS action only",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"rds.amazonaws.com"},"Action":"sts:TagSession"}]}"#,
        assumable: false,
    },
    TrustRow {
        case: "a statement whose action wildcard covers the assume",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"rds.amazonaws.com"},"Action":"sts:*"}]}"#,
        assumable: true,
    },
    TrustRow {
        case: "a document that allows the assume and then explicitly denies it",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"rds.amazonaws.com"},"Action":"sts:AssumeRole"},{"Effect":"Deny","Principal":{"Service":"rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
        assumable: false,
    },
];

/// A role carrying `document` as its trust policy and the identity policy that covers the
/// object read, created through the shared fixture's one `CreateRole`/`PutRolePolicy` pair.
async fn role_trusting(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    role_with_trust_document(router, name, document, Some(get_object_policy())).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_s3_import_runs_exactly_where_the_associated_role_is_one_rds_may_assume() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-import-trust";
    create_bucket(&router, BUCKET, REGION).await;
    let key = "imports/data.csv";
    let body = "1,alpha\n2,beta\n";
    put_object(&router, BUCKET, key, body.as_bytes().to_vec()).await;
    let (address, port) = available_postgres(&router, identifier).await;
    let mut session = session_with_the_extension(&address, port).await;
    run(
        &mut session,
        "the table",
        "CREATE TABLE landing (id integer, name text)",
    )
    .await;
    // ONE statement for every row: only the role's trust document differs.
    let import = format!(
        "SELECT aws_s3.table_import_from_s3('landing', '', '(format csv)', '{BUCKET}', \
         '{key}', '{REGION}')"
    );
    let expected_rows = vec![
        vec![Some("1".to_string()), Some("alpha".to_string())],
        vec![Some("2".to_string()), Some("beta".to_string())],
    ];
    let expected_summary = format!(
        "{} rows imported into relation \"landing\" from file {key} of {} bytes",
        expected_rows.len(),
        body.len()
    );

    for (index, row) in TRUST_ROWS.iter().enumerate() {
        let role_arn = role_trusting(&router, &format!("emu-pg-trust-{index}"), row.document).await;
        if !row.assumable {
            let answered = expect_error(
                row.case,
                call(
                    &router,
                    "AddRoleToDBInstance",
                    &params![
                        "DBInstanceIdentifier" => identifier,
                        "RoleArn" => role_arn,
                        "FeatureName" => FEATURE,
                    ],
                )
                .await,
                400,
                "InvalidParameterValue",
            );
            let message = text(&answered, "Message");
            assert_eq!(
                message, UNTRUSTED_ROLE,
                "case {:?}: expected the fixed refusal {UNTRUSTED_ROLE:?}, got {message:?}",
                row.case
            );
            let reported = instance_associated_roles(&router, row.case, identifier).await;
            assert!(
                reported.is_empty(),
                "case {:?}: a refused association records nothing, got {reported:?}",
                row.case
            );

            // The function is the ENGINE's, so the call still runs: what it fails for is the
            // credentials it has not got, not the grammar.
            let answered = refusal(&mut session, row.case, &import).await;
            assert_eq!(
                answered.code, INSUFFICIENT_PRIVILEGE,
                "case {:?}: the call must run and fail under SQLSTATE \
                 {INSUFFICIENT_PRIVILEGE} rather than fail to parse under {SYNTAX_ERROR}, \
                 got {answered:?}",
                row.case
            );
            assert_eq!(
                answered.message, MISSING_CREDENTIALS,
                "case {:?}: expected the extension's credentials condition \
                 {MISSING_CREDENTIALS:?}, got {answered:?}",
                row.case
            );
            let loaded = rows_of(&mut session, row.case, "landing").await;
            assert!(
                loaded.is_empty(),
                "case {:?}: a refused import loads nothing, got {loaded:?}",
                row.case
            );
            continue;
        }

        // A role the principal may assume: the association is recorded, reaches `ACTIVE`, and
        // the identical call then imports the object.
        associate(&router, identifier, &role_arn, FEATURE).await;
        let reported = instance_associated_roles(&router, row.case, identifier).await;
        let expected_association =
            vec![(role_arn.clone(), FEATURE.to_string(), ACTIVE.to_string())];
        assert_eq!(
            reported, expected_association,
            "case {:?}: expected the association reported ACTIVE for the feature, got \
             {reported:?}",
            row.case
        );
        let answered = session.query(&import).await.unwrap_or_else(|refused| {
            panic!("case {:?}: {import:?} must run, got {refused:?}", row.case)
        });
        assert_eq!(
            answered.only(),
            Some(expected_summary.as_str()),
            "case {:?}: expected the import summary {expected_summary:?}, got {answered:?}",
            row.case
        );
        let loaded = rows_of(&mut session, row.case, "landing").await;
        assert_eq!(
            loaded, expected_rows,
            "case {:?}: expected the object's records as rows, got {loaded:?}",
            row.case
        );

        // The next row starts from the state this one started from: the table is emptied and
        // the association dropped, so a feature that takes ONE role takes the next row's.
        run(&mut session, row.case, "TRUNCATE TABLE landing").await;
        disassociate(&router, identifier, &role_arn, FEATURE).await;
    }
}

/// The same decision for the role Amazon RDS acts under, read the other way round: the role
/// that is associated for the feature is the one the ENGINE acts under, so a second role the
/// principal may assume cannot take the feature's one slot while it is held, and the import
/// keeps running under the role in force.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_feature_keeps_the_role_it_was_given_until_it_is_dropped() {
    let (router, _engine) = emulator();
    let identifier = "emu-pg-import-one-role";
    create_bucket(&router, BUCKET, REGION).await;
    let key = "imports/held.csv";
    let body = "7,gamma\n";
    put_object(&router, BUCKET, key, body.as_bytes().to_vec()).await;
    let in_force = role_trusting(&router, "emu-pg-in-force", TRUST_ROWS[0].document).await;
    let second = role_trusting(&router, "emu-pg-second", TRUST_ROWS[0].document).await;
    let (address, port) = available_postgres(&router, identifier).await;
    let mut session = session_with_the_extension(&address, port).await;
    run(
        &mut session,
        "the table",
        "CREATE TABLE landing (id integer, name text)",
    )
    .await;
    let import = format!(
        "SELECT aws_s3.table_import_from_s3('landing', '', '(format csv)', '{BUCKET}', \
         '{key}', '{REGION}')"
    );

    associate(&router, identifier, &in_force, FEATURE).await;
    let case = "a second role for a feature that has one";
    expect_error(
        case,
        call(
            &router,
            "AddRoleToDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "RoleArn" => second,
                "FeatureName" => FEATURE,
            ],
        )
        .await,
        400,
        "InvalidParameterValue",
    );
    let reported = instance_associated_roles(&router, case, identifier).await;
    assert_eq!(
        reported,
        vec![(in_force.clone(), FEATURE.to_string(), ACTIVE.to_string())],
        "case {case:?}: the refused association leaves the one in force alone, got {reported:?}"
    );
    run(&mut session, case, &import).await;
    let loaded = rows_of(&mut session, case, "landing").await;
    let expected = vec![vec![Some("7".to_string()), Some("gamma".to_string())]];
    assert_eq!(
        loaded, expected,
        "case {case:?}: expected the object's record as a row, got {loaded:?}"
    );

    // Dropped, the feature has no credentials again — and the slot is free for the very role
    // that was refused while it was held.
    disassociate(&router, identifier, &in_force, FEATURE).await;
    run(&mut session, case, "TRUNCATE TABLE landing").await;
    let case = "the association dropped";
    let answered = refusal(&mut session, case, &import).await;
    assert_eq!(
        answered.message, MISSING_CREDENTIALS,
        "case {case:?}: expected the extension's credentials condition, got {answered:?}"
    );
    let case = "the role that was refused while the slot was held";
    associate(&router, identifier, &second, FEATURE).await;
    let reported = instance_associated_roles(&router, case, identifier).await;
    assert_eq!(
        reported,
        vec![(second.clone(), FEATURE.to_string(), ACTIVE.to_string())],
        "case {case:?}: expected the new association reported ACTIVE, got {reported:?}"
    );
    run(&mut session, case, &import).await;
    let loaded = rows_of(&mut session, case, "landing").await;
    assert_eq!(
        loaded, expected,
        "case {case:?}: the import under the role now in force loads the object again, got \
         {loaded:?}"
    );
}
