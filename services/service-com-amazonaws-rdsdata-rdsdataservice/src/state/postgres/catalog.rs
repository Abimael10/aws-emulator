// SPDX-License-Identifier: CC0-1.0

//! The SCHEMA a statement's names are resolved in, and the catalog a statement that reads
//! the database's own schema reads.
//!
//! PostgreSQL resolves an unqualified name in the schema search path, whose first entry is
//! `public` — "the default schema, in which user objects are created" — and answers "what
//! tables does this database hold?" out of the catalog schemas `information_schema` and
//! `pg_catalog`. The engine behind this data plane has ONE schema of stored objects and no
//! schema namespace of its own, so a statement that spells a name the way PostgreSQL does
//! has to be read the same way:
//!
//! * `public.<relation>` is the relation itself — the schema the engine's objects are in,
//!   so the qualification selects exactly what the bare name does, in every position a
//!   statement names a relation in (what a query reads from, what a definition, a delete or
//!   an assignment addresses, and a column reference qualified by both);
//! * `information_schema.<relation>` is a VIEW OF THE SCHEMA, so each one this module
//!   serves is rewritten into the query over the engine's own schema that answers it. Every
//!   value such a row carries is read out of that schema — the relation's name, its kind,
//!   its columns and the types they were declared with — so nothing about the database is
//!   invented, and a catalog relation this module does not serve is left as the caller
//!   wrote it, which the engine answers as the relation it has none of.
//!
//! A rewritten relation becomes a derived table of the caller's OWN statement rather than a
//! view stored in the database: nothing is added to the schema the caller reads back
//! (`information_schema.tables` would otherwise report the catalog itself), and the schema
//! introspection the rewrite uses is run by the statement, which is the one place this data
//! plane's authorizer admits it (`crate::state::confine_to_data_plane`).

use std::ops::ControlFlow;

use sqlparser::ast::{
    Expr, Ident, ObjectName, ObjectNamePart, Query, TableAlias, TableFactor, TableObject, VisitMut,
    VisitorMut,
};

/// The schema PostgreSQL creates every database with and resolves an unqualified name in
/// first, and so the schema the engine's own objects are reported in.
pub(crate) const PUBLIC_SCHEMA: &str = "public";

/// The schema whose relations describe the database itself — "a schema named
/// `information_schema` ... containing views that describe the objects of the current
/// database".
const INFORMATION_SCHEMA: &str = "information_schema";

/// The database PostgreSQL creates in every cluster, which is the catalog name a statement
/// run against this data plane's DEFAULT database reports: a request that names no
/// `database` addresses the cluster's own default one, and PostgreSQL's is `postgres`.
const DEFAULT_DATABASE: &str = "postgres";

/// The name the database a statement runs against is REPORTED under: the name the request's
/// own `database` member named, and the database PostgreSQL creates in every cluster for a
/// request that named none.
///
/// The ONE reading of it, so the catalog's `table_catalog` and the server's own
/// `current_database()` ([`crate::state::dialect_builtins`]) report one name.
pub(crate) fn catalog_name(database: &str) -> &str {
    if database.is_empty() {
        DEFAULT_DATABASE
    } else {
        database
    }
}

/// The engine's own schema table, which every catalog relation below is read out of: it
/// carries one row per table, view, index and trigger the database holds.
const SCHEMA_TABLE: &str = "sqlite_master";

/// The engine's own objects, which PostgreSQL's catalog has no row for: the schema table
/// itself and the sequence bookkeeping the engine keeps under the same reserved prefix.
const ENGINE_OBJECT_PREFIX: &str = "sqlite_";

/// Resolve the schema every name in `statement` is qualified by, against the one schema of
/// stored objects this data plane has and the catalog relations it serves for `database`.
///
/// DEPTH: the walk descends the statement tree, which [`super::plan`] has already bounded
/// before a tree exists at all — the parser's own recursion limit and the token pass over
/// the levels an operator chain asks for — exactly as the delimiting walk beside it is
/// bounded ([`super::names`]).
pub(super) fn resolve_schemas(statement: &mut sqlparser::ast::Statement, database: &str) {
    let named = catalog_name(database);
    // The database's name is CALLER-SUPPLIED text and the relations below report it as a
    // value, so it is written as ONE string literal whatever it holds: the standard
    // doubling of a quote, so a name carrying one states that character rather than ending
    // the literal and spelling something else in the query this builds.
    let catalog_name = named.replace('\'', "''");
    // The visitor cannot break, so the walk always completes.
    let _ = statement.visit(&mut Schemas { catalog_name });
}

/// The walk that resolves the qualifications. It visits the WHOLE statement, so a catalog
/// relation read inside a subquery or a common table expression is resolved like one in the
/// statement's own clauses.
struct Schemas {
    /// The name the catalog reports as the database a relation belongs to, already written
    /// as the body of a string literal.
    catalog_name: String,
}

impl VisitorMut for Schemas {
    type Break = ();

    /// A relation the statement READS FROM, which is the one position a catalog relation
    /// can be read in: a served one becomes the query that answers it.
    ///
    /// A `public.` qualification is not resolved here but in [`Self::pre_visit_relation`]
    /// and [`Self::pre_visit_statement`], which reach every position a statement names a
    /// relation in rather than this one alone.
    fn pre_visit_table_factor(&mut self, factor: &mut TableFactor) -> ControlFlow<Self::Break> {
        let TableFactor::Table { name, alias, .. } = factor else {
            return ControlFlow::Continue(());
        };
        let Some((schema, relation)) = qualified(name) else {
            return ControlFlow::Continue(());
        };
        if schema != INFORMATION_SCHEMA {
            return ControlFlow::Continue(());
        }
        let Some(query) = self.catalog_query(&relation) else {
            // A catalog relation this module does not serve: left exactly as the caller
            // wrote it, so the engine answers that the database holds no such relation.
            return ControlFlow::Continue(());
        };
        // A derived table is addressed by the alias the caller gave it, else by the
        // relation's own name — which is what a reference to `tables.table_name` names.
        let named = alias.take().unwrap_or_else(|| TableAlias {
            name: Ident::new(relation),
            columns: Vec::new(),
        });
        *factor = TableFactor::Derived {
            lateral: false,
            subquery: query,
            alias: Some(named),
        };
        ControlFlow::Continue(())
    }

    /// A relation the statement NAMES, wherever it names one: what a query reads from, and
    /// what a definition, a delete or an assignment addresses. The schema the engine's own
    /// objects are in selects exactly what the bare name does, so the qualification is
    /// resolved away in every one of those positions.
    fn pre_visit_relation(&mut self, relation: &mut ObjectName) -> ControlFlow<Self::Break> {
        resolve_public(relation);
        ControlFlow::Continue(())
    }

    /// The relation an INSERT addresses, which the statement carries as a table OBJECT
    /// rather than as a relation of its own, so the walk above does not reach it.
    fn pre_visit_statement(
        &mut self,
        statement: &mut sqlparser::ast::Statement,
    ) -> ControlFlow<Self::Break> {
        if let sqlparser::ast::Statement::Insert(insert) = statement {
            if let TableObject::TableName(name) = &mut insert.table {
                resolve_public(name);
            }
        }
        ControlFlow::Continue(())
    }

    /// A column reference qualified by the SCHEMA as well as by the relation
    /// (`public.t.c`, `information_schema.tables.table_name`): the schema is resolved
    /// away, leaving the reference the relation itself answers.
    fn pre_visit_expr(&mut self, expr: &mut Expr) -> ControlFlow<Self::Break> {
        if let Expr::CompoundIdentifier(parts) = expr {
            if parts.len() == 3 && is_resolved_schema(&parts[0]) {
                parts.remove(0);
            }
        }
        ControlFlow::Continue(())
    }
}

/// Take the schema the engine's own objects are in off a relation qualified by it, leaving
/// the name the engine resolves. A name qualified by any other schema is left as the caller
/// wrote it.
fn resolve_public(name: &mut ObjectName) {
    if let Some((schema, relation)) = qualified(name) {
        if schema == PUBLIC_SCHEMA {
            *name = ObjectName(vec![ObjectNamePart::Identifier(Ident::new(relation))]);
        }
    }
}

impl Schemas {
    /// The query over the engine's own schema that answers the `information_schema`
    /// relation `relation`, or `None` for one this module does not serve.
    fn catalog_query(&self, relation: &str) -> Option<Box<Query>> {
        let sql = match relation {
            "tables" => self.tables_query(),
            "columns" => self.columns_query(),
            _ => return None,
        };
        // The text is this module's own, and it is parsed by the same reader every
        // statement is: a rewrite that did not parse would be a defect here, so it is
        // reported as one rather than changing the statement into something else.
        match super::parse_query(&sql) {
            Some(query) => Some(query),
            None => {
                ::tracing::error!(%sql, "the catalog query this emulator builds does not parse");
                None
            }
        }
    }

    /// `information_schema.tables` — "one row for each table or view in the current
    /// database", with `table_type` `BASE TABLE` for a table and `VIEW` for a view.
    ///
    /// Every row is a row of the engine's own schema table, so a relation the database does
    /// not hold is in no answer.
    fn tables_query(&self) -> String {
        format!(
            "SELECT '{catalog}' AS table_catalog, '{PUBLIC_SCHEMA}' AS table_schema, \
             name AS table_name, \
             CASE type WHEN 'view' THEN 'VIEW' ELSE 'BASE TABLE' END AS table_type \
             FROM {SCHEMA_TABLE} \
             WHERE type IN ('table', 'view') AND name NOT LIKE '{ENGINE_OBJECT_PREFIX}%'",
            catalog = self.catalog_name
        )
    }

    /// `information_schema.columns` — "one row for each column in a table or view of the
    /// current database", in the position each relation declares it at, with the type it
    /// was declared with and whether it admits a null.
    ///
    /// The columns of a relation are read with the schema introspection the engine itself
    /// answers for that relation (`pragma_table_info`), so a column no relation declares is
    /// in no answer either.
    fn columns_query(&self) -> String {
        format!(
            "SELECT '{catalog}' AS table_catalog, '{PUBLIC_SCHEMA}' AS table_schema, \
             relation.name AS table_name, declared.name AS column_name, \
             declared.cid + 1 AS ordinal_position, \
             CASE declared.\"notnull\" WHEN 0 THEN 'YES' ELSE 'NO' END AS is_nullable, \
             lower(declared.type) AS data_type, declared.dflt_value AS column_default \
             FROM {SCHEMA_TABLE} AS relation \
             JOIN pragma_table_info(relation.name) AS declared \
             WHERE relation.type IN ('table', 'view') \
             AND relation.name NOT LIKE '{ENGINE_OBJECT_PREFIX}%'",
            catalog = self.catalog_name
        )
    }
}

/// Whether an identifier names a schema this module resolves away — the schema the engine's
/// objects are in, or the catalog schema whose relations are rewritten.
fn is_resolved_schema(part: &Ident) -> bool {
    let name = part.value.to_ascii_lowercase();
    name == PUBLIC_SCHEMA || name == INFORMATION_SCHEMA
}

/// The `(schema, relation)` a two-part name spells, folded to lower case the way
/// PostgreSQL folds an undelimited name; `None` for a name of any other shape.
///
/// A name the caller DELIMITED is folded here too: the engine compares identifiers without
/// regard to case whichever way they are written ([`super::names`]), so the two spellings
/// select the same relation in a statement this data plane runs, and the catalog's own
/// relation names are lower case in PostgreSQL.
fn qualified(name: &ObjectName) -> Option<(String, String)> {
    let [first, second] = &name.0[..] else {
        return None;
    };
    let (ObjectNamePart::Identifier(schema), ObjectNamePart::Identifier(relation)) =
        (first, second)
    else {
        return None;
    };
    Some((
        schema.value.to_ascii_lowercase(),
        relation.value.to_ascii_lowercase(),
    ))
}
