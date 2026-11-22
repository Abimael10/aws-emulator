// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLER's query reach the AWS Glue Data Catalog, and what a query it
//! does not reach reports.
//!
//! Athena is a query service over a data catalog, and the metadata every statement needs is
//! read from that catalog with the CALLER's own identity — "running queries against an
//! external catalog requires `GetDataCatalog` permission to the catalog"
//! (`StartQueryExecution`), and the catalog's own actions are the Glue ones AWS's own
//! `AmazonAthenaFullAccess` policy grants beside `athena:*` (`glue:Get*`, `glue:List*` and
//! the writes a DDL statement performs). So a principal allowed to SUBMIT a query in a
//! workgroup ([`super::authorization`]) is not thereby allowed to read or write the catalog
//! the statement addresses: those actions are decided on their own, out of the same
//! identity-based policies, and a statement whose catalog actions the caller does not hold
//! runs no further.
//!
//! Three pieces:
//!
//! 1. **which actions** — what the statement NEEDS of the catalog, read off the plan
//!    ([`needed`]): a statement that reads relations resolves each one's database and table,
//!    and a statement the catalog itself performs performs that statement's own write or
//!    read. Each action is named on the resources Glue publishes it on, in the order the
//!    catalog is reached: a database before the table in it, and the catalog before both.
//! 2. **the decision** — [`super::authorization::first_refusal`], the SAME evaluation of the
//!    caller's identity-based policies this service's own gate uses, so one principal's
//!    documents answer alike for every action asked about.
//! 3. **what the caller is told** — [`insufficient_permissions`]: a query whose catalog
//!    access is refused is not refused at SUBMISSION. Athena takes the submission, mints the
//!    execution and settles it FAILED with this reason, because the catalog is reached while
//!    the statement is planned rather than while the request is validated. So the refusal is
//!    a `StateChangeReason` and an `AthenaError$ErrorMessage` of a terminal execution, and
//!    `GetQueryResults` on it answers with the error a query that did not finish
//!    successfully answers with. IAM's own sentence inside it is the shared one
//!    ([`::service_common::iam_authorization`]), so it is worded here exactly as every other
//!    gate of this tree words the same decision.
//!
//! Only a GLUE Data Catalog's actions are Glue's to authorize, and the ACCOUNT whose catalog
//! a statement reaches is the account the resources are named under: a registered `GLUE`
//! catalog naming another account's catalog id is reached with the caller's own identity on
//! THAT account's catalog, database and table ([`super::catalog_sql::glue_catalog_id`]). A
//! catalog whose metadata an external metastore serves is not Glue's to authorize, and a
//! catalog name this account registered nothing for fails to resolve on its own
//! ([`super::catalog_sql::Failure::catalog_not_found`]).

use ::service_common::iam_authorization as iam;

use super::authorization::{self, RefusedBy};
use super::catalog_sql::statement::{self, CatalogStatement, Plan, Reading, Relation};

/// The IAM action prefix of the AWS Glue Data Catalog, which is also the namespace every
/// Glue ARN carries.
const GLUE: &str = "glue";

/// The Glue action that reads ONE database's definition, which resolving a relation's
/// database is.
const GET_DATABASE: &str = "GetDatabase";

/// The Glue action that LISTS a catalog's databases, which `SHOW DATABASES` is.
const GET_DATABASES: &str = "GetDatabases";

/// The Glue action that reads ONE table's definition, which resolving a relation is.
const GET_TABLE: &str = "GetTable";

/// The Glue action that LISTS a database's tables, which `SHOW TABLES` and `SHOW VIEWS`
/// are.
const GET_TABLES: &str = "GetTables";

/// The Glue action that writes a database, which `CREATE DATABASE` is.
const CREATE_DATABASE: &str = "CreateDatabase";

/// The Glue action that removes a database, which `DROP DATABASE` is.
const DELETE_DATABASE: &str = "DeleteDatabase";

/// The Glue action that writes a table, which `CREATE EXTERNAL TABLE` and `CREATE VIEW` are
/// — a view is a table entry of the catalog.
const CREATE_TABLE: &str = "CreateTable";

/// The Glue action that removes a table, which `DROP TABLE` and `DROP VIEW` are.
const DELETE_TABLE: &str = "DeleteTable";

/// The Glue action that LISTS one table's partitions, which `SHOW PARTITIONS` is.
const GET_PARTITIONS: &str = "GetPartitions";

/// The Glue action that writes the partitions of a table, which a statement that writes a
/// PARTITIONED table's data registers them with.
const BATCH_CREATE_PARTITION: &str = "BatchCreatePartition";

/// The resource portion of the ARN of the catalog itself.
const CATALOG_RESOURCE: &str = "catalog";

/// The resource type a Glue database is addressed by in an ARN.
const DATABASE_RESOURCE_TYPE: &str = "database";

/// The resource type a Glue table is addressed by in an ARN.
const TABLE_RESOURCE_TYPE: &str = "table";

/// Why the caller's query may not reach the Data Catalog, as the execution's
/// `StateChangeReason`, or `None` when every catalog action the statement needs is allowed.
///
/// Call it from the handler, BEFORE the statement is run: the answer rests on the request's
/// own credential, which the run itself has no context to read. A statement that reaches no
/// catalog at all needs no decision and is never refused here.
///
/// TWO halves, in IAM's own order for a call on a resource of another account: the caller's
/// identity-based documents, and — for a catalog that belongs to ANOTHER ACCOUNT — that
/// account's own grant of the catalog, which it makes with a resource-based policy. A
/// cross-account read needs both ("you must grant permissions in the resource policy of the
/// account that owns the resource"), and this emulator holds no other account's Data Catalog
/// and therefore no grant it could have made, so a statement that reaches one is refused in
/// the second half ([`no_cross_account_grant`]).
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    reading: &Reading,
) -> Option<String> {
    let region = authorization::region_from_request(ctx.parts());
    let asked = needed(engine, reading, &region);
    if asked.is_empty() {
        return None;
    }
    if let Some(refused) = authorization::first_refusal(engine, ctx, &asked) {
        return Some(insufficient_permissions(&refused));
    }
    // The FIRST action the statement performs on a resource of another account, which is
    // where such a statement stops.
    let (action, resource) = asked
        .iter()
        .find(|(_, resource)| names_another_account(resource))?;
    let principal = authorization::caller_principal(engine, ctx);
    Some(no_cross_account_grant(&principal, action, resource))
}

/// Whether `resource` is the ARN of a resource of an account OTHER than the one this emulator
/// serves — the only account whose Data Catalog it holds, and so the only one that can have
/// granted anything.
fn names_another_account(resource: &str) -> bool {
    !resource.contains(&format!(":{}:", ::engine::emulator_account_id()))
}

/// The `(action, resource)` pairs the statement `reading` planned performs against the Data
/// Catalog, in the order it performs them.
///
/// Every pair names a Glue action and one ARN that action is performed on: Glue publishes a
/// database as `database/<name>` and a table as `table/<database>/<name>` under the
/// catalog's own ARN, and an action on either is performed on the catalog as well — so a
/// policy scoped to one database covers the statements that address it and no other.
fn needed<Eng: Send + Sync + 'static>(
    engine: &Eng,
    reading: &Reading,
    region: &str,
) -> Vec<(String, String)> {
    let mut asked: Vec<(String, String)> = Vec::new();
    match &reading.plan {
        // Text that is no statement of this language, and a statement the engine runs over
        // nothing the catalog holds, reach no catalog.
        Plan::Engine => {}
        Plan::Relations(relational) => {
            push_relations(&mut asked, engine, &relational.relations, region);
        }
        // A statement that WRITES its output reads the relations its query names exactly as
        // the read above does, and — where it defines a table — writes that table's own
        // entry, and the partitions of its data where it is partitioned.
        Plan::Written(writing) => {
            push_relations(&mut asked, engine, &writing.query.relations, region);
            push_destination(
                &mut asked,
                ::engine::emulator_account_id(),
                region,
                &writing.destination,
            );
        }
        Plan::Catalog(statement) => {
            let (action, resources) = catalog_statement_action(statement, region);
            push(&mut asked, action, resources);
        }
    }
    asked
}

/// Add the actions resolving each of `relations` performs: the database it names, and then
/// the table in it, which is the order the plan reaches them in.
fn push_relations<Eng: Send + Sync + 'static>(
    asked: &mut Vec<(String, String)>,
    engine: &Eng,
    relations: &[Relation],
    region: &str,
) {
    for relation in relations {
        // The catalog's own reporting schema is not a catalog entry: a statement
        // reads it to be told what the catalog holds, and what it is shown is each
        // database and table the reads below are decided on.
        if relation.is_information_schema() {
            continue;
        }
        // WHOSE catalog the relation lives in: a statement over a catalog no Glue
        // Data Catalog serves needs no Glue permission at all.
        let Some(account) = catalog_account(engine, &relation.catalog) else {
            continue;
        };
        push(
            asked,
            GET_DATABASE,
            database_arns(&account, region, &relation.database),
        );
        push(asked, GET_TABLE, table_arns(&account, region, relation));
    }
}

/// Add the actions a statement that WRITES its output performs on what it DEFINES: a
/// statement that defines a table writes that table's entry, and the partitions of its data
/// where it partitions it; one that writes objects and defines nothing reaches no catalog
/// entry of its own.
fn push_destination(
    asked: &mut Vec<(String, String)>,
    account: &str,
    region: &str,
    destination: &statement::WriteDestination,
) {
    let statement::WriteDestination::Table(table) = destination else {
        return;
    };
    push(asked, CREATE_TABLE, table_arns(account, region, &table.target));
    if !table.partitioned_by.is_empty() {
        push(
            asked,
            BATCH_CREATE_PARTITION,
            table_arns(account, region, &table.target),
        );
    }
}

/// The Glue action one statement the catalog itself performs IS, with the resources it is
/// performed on.
///
/// Each pairing is the one AWS documents between an Athena statement and the Data Catalog
/// action it performs: a database definition is a `CreateDatabase`, a drop a
/// `DeleteDatabase`, a table or view definition a `CreateTable`, a drop of either a
/// `DeleteTable`, and a listing the plural read of what it lists. A statement that READS one
/// entry is the singular read of that entry.
fn catalog_statement_action(
    statement: &CatalogStatement,
    region: &str,
) -> (&'static str, Vec<String>) {
    // A statement the CATALOG performs is performed on the catalog its target names, which
    // for every such statement is one this account holds: a definition or a drop in another
    // account's catalog is not something this service performs.
    let account = ::engine::emulator_account_id();
    match statement {
        CatalogStatement::CreateDatabase(database) => (
            CREATE_DATABASE,
            database_arns(account, region, &database.target.database),
        ),
        CatalogStatement::DropDatabase { target, .. } => (
            DELETE_DATABASE,
            database_arns(account, region, &target.database),
        ),
        CatalogStatement::ShowDatabases { .. } => {
            (GET_DATABASES, vec![catalog_arn(account, region)])
        }
        CatalogStatement::CreateExternalTable(table) => {
            (CREATE_TABLE, table_arns(account, region, &table.target))
        }
        CatalogStatement::CreateView(view) => {
            (CREATE_TABLE, table_arns(account, region, &view.target))
        }
        CatalogStatement::DropTable { target, .. }
        | CatalogStatement::DropView { target, .. } => {
            (DELETE_TABLE, table_arns(account, region, target))
        }
        CatalogStatement::ShowCreateView { target } => {
            (GET_TABLE, table_arns(account, region, target))
        }
        CatalogStatement::ShowRelations { schema, .. } => {
            (GET_TABLES, database_arns(account, region, &schema.database))
        }
        CatalogStatement::ShowPartitions { target } => {
            (GET_PARTITIONS, table_arns(account, region, target))
        }
    }
}

/// Add one action on each of `resources` to `asked`, skipping a pair already asked about —
/// a statement that names one relation twice asks about it once.
fn push(asked: &mut Vec<(String, String)>, action: &str, resources: Vec<String>) {
    for resource in resources {
        let pair = (format!("{GLUE}:{action}"), resource);
        if !asked.contains(&pair) {
            asked.push(pair);
        }
    }
}

/// The ACCOUNT whose Glue Data Catalog the data catalog `catalog` names, or `None` when it
/// names no Glue Data Catalog at all — a catalog an external metastore serves, or a name
/// nothing registered.
///
/// A Glue `CatalogId` IS an account id, so the catalog id the name resolves to is the account
/// its resources are published under ([`super::catalog_sql::glue_catalog_id`]). That is what
/// makes a CROSS-ACCOUNT catalog decided on the other account's resources: a caller holding
/// `glue:Get*` on its own catalog holds nothing on another account's, which is the permission
/// a cross-account read needs.
fn catalog_account<Eng: Send + Sync + 'static>(engine: &Eng, catalog: &str) -> Option<String> {
    super::catalog_sql::glue_catalog_id(engine, catalog)
}

/// The ARNs an action on the database `database` of `account` is performed on: the catalog,
/// and the database itself.
fn database_arns(account: &str, region: &str, database: &str) -> Vec<String> {
    vec![
        catalog_arn(account, region),
        glue_arn(
            account,
            region,
            &format!("{DATABASE_RESOURCE_TYPE}/{database}"),
        ),
    ]
}

/// The ARNs an action on one table is performed on: the catalog, the database that holds it,
/// and the table itself.
fn table_arns(account: &str, region: &str, relation: &Relation) -> Vec<String> {
    let mut arns = database_arns(account, region, &relation.database);
    arns.push(glue_arn(
        account,
        region,
        &format!(
            "{TABLE_RESOURCE_TYPE}/{}/{}",
            relation.database, relation.table
        ),
    ));
    arns
}

/// The ARN of `account`'s Glue Data Catalog in `region`.
fn catalog_arn(account: &str, region: &str) -> String {
    glue_arn(account, region, CATALOG_RESOURCE)
}

/// One Glue ARN of `account`.
///
/// Built through [`::engine::emulator_arn`] for this account's own resources, so the account
/// segment is the canonical one every ARN of this emulator carries; a resource of ANOTHER
/// account carries that account's id in the same form, which is the only thing that
/// distinguishes the two.
fn glue_arn(account: &str, region: &str, resource: &str) -> String {
    let local = ::engine::emulator_arn(GLUE, region, resource);
    if account == ::engine::emulator_account_id() {
        return local;
    }
    local.replace(
        &format!(":{}:", ::engine::emulator_account_id()),
        &format!(":{account}:"),
    )
}

/// What Athena says in front of IAM's own sentence when a query could not be planned
/// because the caller may not reach the catalog: the fixed text that says the QUERY is what
/// was refused, which is what tells this refusal from the one the request itself gets.
const QUERY_NOT_EXECUTED: &str = "Insufficient permissions to execute the query.";

/// What closes the default-deny form of that reason: the separator that would precede the
/// CONDITION clause IAM appends where a condition decided the deny. Where none did, the
/// clause is empty and the separator is all that follows — which is how the refusal is
/// recorded, down to that character.
const CONDITION_CLAUSE_SEPARATOR: &str = " ";

/// The reason a query whose catalog access IAM refused settles FAILED with.
///
/// Athena states it as the sentence that says the QUERY could not be executed, followed by
/// IAM's OWN sentence about the refusal — who asked, which action, which resource, and which
/// of IAM's two ways of reaching a deny refused it. That sentence and both of its reason
/// clauses are the shared ones ([`::service_common::iam_authorization`]), so this service and
/// every other one that reaches the same decision word it alike; what belongs to Athena is
/// the sentence in front of it and the separator it closes with.
fn insufficient_permissions(refused: &authorization::Refusal) -> String {
    let authorization::Refusal {
        principal,
        action,
        resource,
        how,
    } = refused;
    let (because, closing) = match how {
        RefusedBy::NoAllow => (
            iam::no_identity_policy_allows(action),
            CONDITION_CLAUSE_SEPARATOR,
        ),
        RefusedBy::ExplicitDeny => (iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(), ""),
    };
    let refusal = iam::not_authorized_by_identity_policy(principal, action, resource, &because);
    format!("{QUERY_NOT_EXECUTED} {refusal}{closing}")
}

/// The reason a query that reaches another ACCOUNT's Data Catalog settles FAILED with when
/// that account has granted it nothing.
///
/// The same shape [`insufficient_permissions`] states, over the OTHER half of a cross-account
/// decision: the sentence that says the query could not be executed, then IAM's own sentence
/// for a resource whose owner grants the caller nothing — who asked, which action, which
/// resource — which is the shared one ([`::service_common::iam_authorization`]) so every
/// service that reaches this decision words it alike. It closes with the same separator a
/// refusal no policy allowed carries ([`CONDITION_CLAUSE_SEPARATOR`]).
fn no_cross_account_grant(principal: &str, action: &str, resource: &str) -> String {
    let refusal = iam::not_authorized_by_resource_policy(principal, action, resource);
    format!("{QUERY_NOT_EXECUTED} {refusal}{CONDITION_CLAUSE_SEPARATOR}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::catalog_sql::statement::{self, QueryContext};

    /// The context the statements below resolve an unqualified name against.
    fn context() -> QueryContext {
        QueryContext {
            catalog: crate::state::GLUE_DATA_CATALOG_NAME.to_string(),
            database: "logs".to_string(),
        }
    }

    /// The `(action, resource)` pairs the statement `sql` needs of the catalog, with the
    /// catalog resolution left out — what the statement itself asks for, in the account whose
    /// catalog it reaches.
    fn asked_of(account: &str, sql: &str) -> Vec<(String, String)> {
        let reading = statement::read(sql, &context());
        let region = "us-east-1";
        // The actions resolving one list of relations performs, which both a read and a
        // statement that writes its output perform.
        let relation_actions = |relations: &[Relation]| {
            let mut asked = Vec::new();
            for relation in relations {
                if relation.is_information_schema() {
                    continue;
                }
                push(
                    &mut asked,
                    GET_DATABASE,
                    database_arns(account, region, &relation.database),
                );
                push(&mut asked, GET_TABLE, table_arns(account, region, relation));
            }
            asked
        };
        match &reading.plan {
            Plan::Engine => Vec::new(),
            Plan::Relations(relational) => relation_actions(&relational.relations),
            Plan::Written(writing) => {
                let mut asked = relation_actions(&writing.query.relations);
                push_destination(&mut asked, account, region, &writing.destination);
                asked
            }
            Plan::Catalog(statement) => {
                let mut asked = Vec::new();
                let (action, resources) = catalog_statement_action(statement, region);
                push(&mut asked, action, resources);
                asked
            }
        }
    }

    /// The same, for a statement over the catalog this account holds.
    fn asked(sql: &str) -> Vec<(String, String)> {
        asked_of(::engine::emulator_account_id(), sql)
    }

    /// The actions of `asked`, in order and without their resources.
    fn actions(sql: &str) -> Vec<String> {
        asked(sql)
            .into_iter()
            .map(|(action, _)| action)
            .collect::<Vec<String>>()
            .into_iter()
            .fold(Vec::new(), |mut kept: Vec<String>, action| {
                if kept.last() != Some(&action) {
                    kept.push(action);
                }
                kept
            })
    }

    /// RULE: what a statement needs of the Data Catalog is read off its PLAN — the rows span
    /// the statement kinds that reach the catalog (a read of relations, each definition, each
    /// drop, each listing) and the ones that reach none.
    #[test]
    fn the_catalog_actions_asked_about_are_the_ones_the_statement_performs() {
        // (case, the statement, the Glue actions it needs, in order)
        let cases: Vec<(&str, &str, Vec<&str>)> = vec![
            (
                "a read of one relation",
                "SELECT * FROM logs.events",
                vec!["glue:GetDatabase", "glue:GetTable"],
            ),
            (
                "a count over an unqualified relation",
                "SELECT count(*) FROM events",
                vec!["glue:GetDatabase", "glue:GetTable"],
            ),
            (
                "a join over two relations",
                "SELECT * FROM logs.events e JOIN logs.hits h ON e.id = h.id",
                vec!["glue:GetDatabase", "glue:GetTable"],
            ),
            (
                "a projection the catalog resolves nothing for",
                "SELECT 1 AS one",
                Vec::new(),
            ),
            (
                "a database definition",
                "CREATE DATABASE shop",
                vec!["glue:CreateDatabase"],
            ),
            (
                "a database dropped",
                "DROP DATABASE shop",
                vec!["glue:DeleteDatabase"],
            ),
            (
                "the databases listed",
                "SHOW DATABASES",
                vec!["glue:GetDatabases"],
            ),
            (
                "an external table defined",
                "CREATE EXTERNAL TABLE logs.events (id bigint) LOCATION 's3://b/e/'",
                vec!["glue:CreateTable"],
            ),
            (
                "a table dropped",
                "DROP TABLE logs.events",
                vec!["glue:DeleteTable"],
            ),
            (
                "a view defined",
                "CREATE VIEW logs.recent AS SELECT 1 AS n",
                vec!["glue:CreateTable"],
            ),
            (
                "a view dropped",
                "DROP VIEW logs.recent",
                vec!["glue:DeleteTable"],
            ),
            (
                "a view's definition read",
                "SHOW CREATE VIEW logs.recent",
                vec!["glue:GetTable"],
            ),
            (
                "the tables of a database listed",
                "SHOW TABLES IN logs",
                vec!["glue:GetTables"],
            ),
        ];

        for (case, sql, expected) in &cases {
            let actual = actions(sql);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the actions {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a Glue action is performed on the catalog, on the database it addresses and —
    /// for a table action — on the table, so the ARNs asked about are those, in that order;
    /// and a statement reading two relations asks about each table of its own, once.
    #[test]
    fn the_resources_asked_about_are_the_catalog_the_database_and_the_table() {
        let account = ::engine::emulator_account_id();
        let actual = asked("SELECT * FROM logs.events");
        let expected: Vec<(String, String)> = vec![
            (
                "glue:GetDatabase".to_string(),
                format!("arn:aws:glue:us-east-1:{account}:catalog"),
            ),
            (
                "glue:GetDatabase".to_string(),
                format!("arn:aws:glue:us-east-1:{account}:database/logs"),
            ),
            (
                "glue:GetTable".to_string(),
                format!("arn:aws:glue:us-east-1:{account}:catalog"),
            ),
            (
                "glue:GetTable".to_string(),
                format!("arn:aws:glue:us-east-1:{account}:database/logs"),
            ),
            (
                "glue:GetTable".to_string(),
                format!("arn:aws:glue:us-east-1:{account}:table/logs/events"),
            ),
        ];
        assert_eq!(
            actual, expected,
            "case \"a read of one relation\": expected {expected:?}, got {actual:?}"
        );

        // Two relations of one database: the database is asked about once, and each table
        // of its own.
        let joined = asked("SELECT * FROM logs.events e JOIN logs.hits h ON e.id = h.id");
        for table in ["table/logs/events", "table/logs/hits"] {
            let arn = format!("arn:aws:glue:us-east-1:{account}:{table}");
            assert!(
                joined.contains(&("glue:GetTable".to_string(), arn.clone())),
                "case \"a join over two relations\": expected a read of {arn:?}, got {joined:?}"
            );
        }
        let databases = joined
            .iter()
            .filter(|(action, resource)| {
                action == "glue:GetDatabase" && resource.ends_with(":database/logs")
            })
            .count();
        assert_eq!(
            databases, 1,
            "case \"a join over two relations\": expected the database asked about once, got \
             {databases} in {joined:?}"
        );
    }

    /// RULE: the ACCOUNT an ARN names is the account whose Data Catalog the statement reaches,    /// so a statement over a catalog of another account is decided on THAT account's catalog,
    /// database and table — which is what a caller holding grants on its own catalog alone
    /// does not hold.
    #[test]
    fn the_account_asked_about_is_the_one_whose_catalog_the_statement_reaches() {
        // The rows differ along whose catalog the statement reaches: this account's, and two
        // other accounts'.
        for account in [
            ::engine::emulator_account_id(),
            "100000000000",
            "210987654321",
        ] {
            let actual = asked_of(account, "SELECT * FROM logs.events");
            let expected: Vec<(String, String)> = vec![
                (
                    "glue:GetDatabase".to_string(),
                    format!("arn:aws:glue:us-east-1:{account}:catalog"),
                ),
                (
                    "glue:GetDatabase".to_string(),
                    format!("arn:aws:glue:us-east-1:{account}:database/logs"),
                ),
                (
                    "glue:GetTable".to_string(),
                    format!("arn:aws:glue:us-east-1:{account}:catalog"),
                ),
                (
                    "glue:GetTable".to_string(),
                    format!("arn:aws:glue:us-east-1:{account}:database/logs"),
                ),
                (
                    "glue:GetTable".to_string(),
                    format!("arn:aws:glue:us-east-1:{account}:table/logs/events"),
                ),
            ];
            assert_eq!(
                actual, expected,
                "case {account:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the reason names the caller, the action and the resource IAM refused, and which
    /// of IAM's two ways of reaching a deny refused it.
    #[test]
    fn the_reason_names_the_caller_the_action_and_the_resource() {
        let principal =
            "arn:aws:sts::000000000000:assumed-role/limited/athena-session".to_string();
        let action = "glue:GetDatabase".to_string();
        let resource = "arn:aws:glue:us-east-1:000000000000:catalog".to_string();
        // (case, how IAM refused, the reason the execution reports)
        let cases: Vec<(&str, RefusedBy, String)> = vec![
            (
                "no policy allows the action",
                RefusedBy::NoAllow,
                format!(
                    "Insufficient permissions to execute the query. User: {principal} is not \
                     authorized to perform: {action} on resource: {resource} because no \
                     identity-based policy allows the {action} action "
                ),
            ),
            (
                "a statement denies it explicitly",
                RefusedBy::ExplicitDeny,
                format!(
                    "Insufficient permissions to execute the query. User: {principal} is not \
                     authorized to perform: {action} on resource: {resource} with an explicit \
                     deny in an identity-based policy"
                ),
            ),
        ];

        for (case, how, expected) in &cases {
            let actual = insufficient_permissions(&authorization::Refusal {
                principal: principal.clone(),
                action: action.clone(),
                resource: resource.clone(),
                how: *how,
            });
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected the reason {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a statement that reaches another ACCOUNT's Data Catalog is refused on that
    /// account's own grant — the half of a cross-account decision the resource owner makes —
    /// so the reason names the caller, the action and the resource of that account, in IAM's
    /// own wording for a resource whose owner grants nothing. The rows differ along which
    /// account's resource is addressed and which of it the statement reached.
    #[test]
    fn a_cross_account_catalog_is_refused_on_the_grant_its_owner_never_made() {
        let principal =
            "arn:aws:sts::000000000000:assumed-role/analyst/athena-session".to_string();
        for (account, action, resource_type) in [
            ("100000000000", "glue:GetDatabase", "catalog"),
            ("100000000000", "glue:GetTable", "table/logs/events"),
            ("210987654321", "glue:GetDatabase", "database/logs"),
        ] {
            let resource = format!("arn:aws:glue:us-east-1:{account}:{resource_type}");
            assert!(
                names_another_account(&resource),
                "case {resource:?}: expected a resource of another account"
            );
            let actual = no_cross_account_grant(&principal, action, &resource);
            let expected = format!(
                "Insufficient permissions to execute the query. User: {principal} is not \
                 authorized to perform: {action} on resource: {resource} because no \
                 resource-based policy allows the {action} action "
            );
            assert_eq!(
                actual, expected,
                "case {resource:?}: expected the reason {expected:?}, got {actual:?}"
            );
        }
        // This account's own resources are not cross-account, so nothing here decides them.
        let own = format!(
            "arn:aws:glue:us-east-1:{}:catalog",
            ::engine::emulator_account_id()
        );
        assert!(
            !names_another_account(&own),
            "case {own:?}: this account's own catalog is not another account's"
        );
    }
}
