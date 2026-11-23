// SPDX-License-Identifier: CC0-1.0

//! The helpers a resolver BUILDS a relational-database request with: the statement builders
//! and the result reading the `@aws-appsync/utils/rds` module publishes to APPSYNC_JS code,
//! and `$util.rds` publishes to a mapping template.
//!
//! One reading of each helper, here, for both runtimes: a statement an APPSYNC_JS resolver
//! builds and the same statement built by a Velocity template are rendered by this module,
//! so the two cannot disagree about the SQL a request carries or about the rows an answer
//! holds. The runtimes reach it through a host function
//! ([`crate::ops::evaluate_code::appsync_js`]) and through `$util.rds`
//! ([`super::super::mapping_template::utilities`]); neither carries a copy.
//!
//! What a builder produces is a DESCRIPTOR of one statement — what it selects, inserts,
//! updates or deletes, with the VALUES it compares and assigns held as they stand — and
//! `createMySQLStatement` / `createPgStatement` render those descriptors into the request
//! object the data source is called with: the statements as SQL text, with every value bound
//! to a named placeholder in `variableMap` rather than written into the text, so a value that
//! happens to carry a quote or a semicolon is a value and never part of the statement. The
//! two renderings differ only in how an identifier is quoted — the dialect of the engine the
//! addressed cluster runs.
//!
//! Nothing here calls anything or reads any state: every helper is a pure transformation of
//! the arguments the resolver handed it.

use ::serde_json::{json, Map as JsonMap, Value as Json};

/// One call of a helper this module publishes, as the runtime that evaluated the resolver
/// hands it over: the helper's name and the arguments the call was made with.
///
/// `Ok(None)` is a name this module publishes no helper under, which the caller reports as
/// the helper nothing implements; `Err` is a call this module REFUSES — the argument it was
/// handed is not one the helper acts on — which the resolver's own code raises.
pub fn call(helper: &str, args: &[Json]) -> Result<Option<Json>, String> {
    let first = || args.first().cloned().unwrap_or(Json::Null);
    match helper {
        SELECT | INSERT | UPDATE | REMOVE => Ok(Some(described(helper, &first())?)),
        SQL => Ok(Some(described_sql(args)?)),
        "createMySQLStatement" => Ok(Some(request(args, Dialect::MySql)?)),
        "createPgStatement" => Ok(Some(request(args, Dialect::Postgres)?)),
        "toJsonObject" => Ok(Some(super::records::rows(&first())?)),
        // The same rows, already serialized, which is what a template inlines into the value
        // it renders.
        "toJsonString" => Ok(Some(Json::String(
            super::records::rows(&first())?.to_string(),
        ))),
        _ => Ok(None),
    }
}

/// The helpers that describe one statement each, by the names the module exports them under.
const SELECT: &str = "select";
const INSERT: &str = "insert";
const UPDATE: &str = "update";
const REMOVE: &str = "remove";
const SQL: &str = "sql";

/// The descriptor one builder answers with: the kind of statement it describes and the
/// INPUT it was built from, held as it stands so the rendering reads the values the resolver
/// passed rather than a re-serialization of them.
///
/// Carried under a member of this module's own ([`STATEMENT_MEMBER`]), so a value that
/// reaches `createMySQLStatement` / `createPgStatement` without being built by one of these
/// builders is refused rather than rendered as an empty statement.
fn described(kind: &str, input: &Json) -> Result<Json, String> {
    let members = input
        .as_object()
        .ok_or_else(|| format!("{A_BUILDER_TAKES_AN_OBJECT} {kind} takes one."))?;
    if !members.contains_key(TABLE_MEMBER) {
        return Err(format!("{A_STATEMENT_NAMES_A_TABLE} {kind}."));
    }
    Ok(json!({STATEMENT_MEMBER: {KIND_MEMBER: kind, INPUT_MEMBER: input}}))
}

/// The descriptor the `sql` tagged template answers with: the literal parts of the template
/// and the values interpolated between them, in the order the template writes them.
fn described_sql(args: &[Json]) -> Result<Json, String> {
    let parts = args.first().and_then(Json::as_array).cloned();
    let Some(parts) = parts else {
        return Err(A_TEMPLATE_CARRIES_ITS_PARTS.to_string());
    };
    let values = args.get(1).and_then(Json::as_array).cloned().unwrap_or_default();
    Ok(json!({
        STATEMENT_MEMBER: {
            KIND_MEMBER: SQL,
            PARTS_MEMBER: parts,
            VALUES_MEMBER: values,
        }
    }))
}

/// The members a descriptor is made of.
const STATEMENT_MEMBER: &str = "__appsyncRdsStatement";
const KIND_MEMBER: &str = "kind";
const INPUT_MEMBER: &str = "input";
const PARTS_MEMBER: &str = "parts";
const VALUES_MEMBER: &str = "values";

/// The member a hinted value carries its type hint under, which `typeHint.<TYPE>(value)`
/// wraps a value in, and the member holding the value itself.
const TYPE_HINT_MEMBER: &str = "__appsyncRdsTypeHint";
const HINTED_VALUE_MEMBER: &str = "value";

/// The request object `createMySQLStatement` / `createPgStatement` answer with: the
/// statements rendered in `dialect`, the values each placeholder stands for, and the type
/// hint of every value that carries one.
///
/// The statements are rendered through ONE binder, so no two of them can bind different
/// values to the same placeholder name.
fn request(args: &[Json], dialect: Dialect) -> Result<Json, String> {
    if args.is_empty() {
        return Err(A_REQUEST_CARRIES_A_STATEMENT.to_string());
    }
    if args.len() > MAX_STATEMENTS {
        return Err(format!(
            "{TOO_MANY_STATEMENTS} {MAX_STATEMENTS}, and this request carries {}.",
            args.len()
        ));
    }
    let mut renderer = Renderer::new(dialect);
    let mut statements = Vec::with_capacity(args.len());
    for argument in args {
        statements.push(renderer.statement(argument)?);
    }
    Ok(json!({
        VERSION_MEMBER: STATEMENT_VERSION,
        STATEMENTS_MEMBER: statements,
        VARIABLE_MAP_MEMBER: Json::Object(renderer.variables),
        VARIABLE_TYPE_HINT_MAP_MEMBER: Json::Object(renderer.hints),
    }))
}

/// The members of the request object a relational-database data source is called with, which
/// is also what [`super::answered`] reads it back out of.
pub const VERSION_MEMBER: &str = "version";
pub const STATEMENTS_MEMBER: &str = "statements";
pub const VARIABLE_MAP_MEMBER: &str = "variableMap";
pub const VARIABLE_TYPE_HINT_MAP_MEMBER: &str = "variableTypeHintMap";

/// The version such a request object states: the one AppSync's relational-database request
/// carries.
pub const STATEMENT_VERSION: &str = "2018-05-29";

/// How many statements ONE request carries.
///
/// A bound on the work one field resolution starts: each statement is a call against the
/// service that owns the database, so a request naming more of them than this is refused
/// rather than run.
pub const MAX_STATEMENTS: usize = 2;

/// The SQL dialect a request is rendered in: the engine the addressed cluster runs, which is
/// which `create…Statement` the resolver built the request with.
///
/// It decides how an IDENTIFIER is written, and nothing else: MySQL quotes one with a
/// backtick and PostgreSQL with a double quote (each doubled to escape itself inside a
/// quoted name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    MySql,
    Postgres,
}

impl Dialect {
    /// The character this dialect quotes an identifier with.
    fn quote(self) -> char {
        match self {
            Self::MySql => '`',
            Self::Postgres => '"',
        }
    }
}

/// The rendering of one request: the dialect its statements are written in, and the values
/// bound so far with the hints they carry.
struct Renderer {
    dialect: Dialect,
    bound: usize,
    variables: JsonMap<String, Json>,
    hints: JsonMap<String, Json>,
}

impl Renderer {
    /// A rendering that has bound nothing yet.
    fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            bound: 0,
            variables: JsonMap::new(),
            hints: JsonMap::new(),
        }
    }

    /// One statement of the request, from the descriptor a builder produced.
    fn statement(&mut self, described: &Json) -> Result<String, String> {
        let described = described
            .get(STATEMENT_MEMBER)
            .ok_or_else(|| A_REQUEST_RENDERS_BUILT_STATEMENTS.to_string())?;
        let kind = described
            .get(KIND_MEMBER)
            .and_then(Json::as_str)
            .unwrap_or_default();
        if kind == SQL {
            return self.interpolated(described);
        }
        let input = described.get(INPUT_MEMBER).cloned().unwrap_or(Json::Null);
        let member = |name: &str| input.get(name).cloned().unwrap_or(Json::Null);
        let table = self.table(&member(TABLE_MEMBER))?;
        match kind {
            SELECT => {
                let mut text = format!(
                    "SELECT {} FROM {table}",
                    self.columns(&member(COLUMNS_MEMBER))?
                );
                text.push_str(&self.filter(&member(WHERE_MEMBER))?);
                text.push_str(&self.ordering(&member(ORDER_BY_MEMBER))?);
                text.push_str(&bounded("LIMIT", &member(LIMIT_MEMBER))?);
                text.push_str(&bounded("OFFSET", &member(OFFSET_MEMBER))?);
                Ok(text)
            }
            INSERT => {
                let assigned = self.assignments(&member(VALUES_MEMBER))?;
                let columns: Vec<String> =
                    assigned.iter().map(|(column, _)| column.clone()).collect();
                let bound: Vec<String> =
                    assigned.iter().map(|(_, value)| value.clone()).collect();
                Ok(format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    columns.join(", "),
                    bound.join(", ")
                ))
            }
            UPDATE => {
                let assigned: Vec<String> = self
                    .assignments(&member(VALUES_MEMBER))?
                    .into_iter()
                    .map(|(column, value)| format!("{column} = {value}"))
                    .collect();
                let mut text = format!("UPDATE {table} SET {}", assigned.join(", "));
                text.push_str(&self.filter(&member(WHERE_MEMBER))?);
                Ok(text)
            }
            REMOVE => {
                let mut text = format!("DELETE FROM {table}");
                text.push_str(&self.filter(&member(WHERE_MEMBER))?);
                Ok(text)
            }
            other => Err(format!("{NO_SUCH_STATEMENT}{other}")),
        }
    }

    /// The text of a statement the `sql` tagged template wrote: its literal parts, with every
    /// interpolated value bound to a placeholder between them.
    fn interpolated(&mut self, described: &Json) -> Result<String, String> {
        let parts = described
            .get(PARTS_MEMBER)
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let values = described
            .get(VALUES_MEMBER)
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        let mut text = String::new();
        for (at, part) in parts.iter().enumerate() {
            let Some(literal) = part.as_str() else {
                return Err(A_TEMPLATE_CARRIES_ITS_PARTS.to_string());
            };
            text.push_str(literal);
            if let Some(value) = values.get(at) {
                text.push_str(&self.bind(value));
            }
        }
        if text.trim().is_empty() {
            return Err(A_STATEMENT_IS_NOT_EMPTY.to_string());
        }
        Ok(text)
    }

    /// The table a statement is about, as the dialect writes its name.
    fn table(&self, named: &Json) -> Result<String, String> {
        let written = self.identifier(named);
        written.filter(|name| !name.is_empty()).ok_or_else(|| {
            format!("{A_STATEMENT_NAMES_A_TABLE} {TABLE_MEMBER} carries {named}.")
        })
    }

    /// The columns a selection reports: every column of the table where it names none or
    /// names them all, and otherwise exactly the ones it names, in that order.
    fn columns(&self, named: &Json) -> Result<String, String> {
        match named {
            Json::Null => Ok(ALL_COLUMNS.to_string()),
            Json::String(text) if text.trim() == ALL_COLUMNS => Ok(ALL_COLUMNS.to_string()),
            Json::Array(columns) if columns.is_empty() => Ok(ALL_COLUMNS.to_string()),
            Json::Array(columns) => {
                let mut written = Vec::with_capacity(columns.len());
                for column in columns {
                    written.push(self.column(column)?);
                }
                Ok(written.join(", "))
            }
            single => Ok(self.column(single)?),
        }
    }

    /// `ORDER BY` as the request's `orderBy` states it, and nothing at all where it states
    /// none.
    fn ordering(&self, ordered: &Json) -> Result<String, String> {
        let entries = match ordered {
            Json::Null => return Ok(String::new()),
            Json::Array(entries) if entries.is_empty() => return Ok(String::new()),
            Json::Array(entries) => entries.clone(),
            single => vec![single.clone()],
        };
        let mut written = Vec::with_capacity(entries.len());
        for entry in &entries {
            let column = self.column(entry.get(COLUMN_MEMBER).unwrap_or(entry))?;
            let direction = match entry.get(DIRECTION_MEMBER).and_then(Json::as_str) {
                None => ASCENDING,
                Some(stated) => {
                    let stated = stated.trim().to_ascii_uppercase();
                    if stated == ASCENDING {
                        ASCENDING
                    } else if stated == DESCENDING {
                        DESCENDING
                    } else {
                        return Err(format!(
                            "{ONE_OF_THE_DIRECTIONS} {ASCENDING}, {DESCENDING}, and this \
                             request carries {stated:?}."
                        ));
                    }
                }
            };
            written.push(format!("{column} {direction}"));
        }
        Ok(format!(" ORDER BY {}", written.join(", ")))
    }

    /// The columns a write assigns, each with the placeholder its value is bound to, in the
    /// order the request states them.
    fn assignments(&mut self, values: &Json) -> Result<Vec<(String, String)>, String> {
        let Some(members) = values.as_object().filter(|members| !members.is_empty()) else {
            return Err(format!("{A_WRITE_ASSIGNS_VALUES} {VALUES_MEMBER}."));
        };
        let mut assigned = Vec::with_capacity(members.len());
        for (column, value) in members {
            assigned.push((
                self.column(&Json::String(column.clone()))?,
                self.bind(value),
            ));
        }
        Ok(assigned)
    }

    /// The `WHERE` clause the request's own condition states, and nothing at all where it
    /// states none — which is a statement about every row of the table.
    fn filter(&mut self, condition: &Json) -> Result<String, String> {
        let written = self.condition(condition, 0)?;
        match written.is_empty() {
            true => Ok(String::new()),
            false => Ok(format!(" WHERE {written}")),
        }
    }

    /// One condition: the clauses its members state, joined with `AND`, with `and`, `or` and
    /// `not` naming a group of conditions rather than a column.
    ///
    /// `depth` is how many groups deep in the request's own condition this one sits, and a
    /// condition past [`MAX_CONDITION_DEPTH`] is REFUSED rather than walked: the nesting is
    /// the resolver's to choose and walking it is this rendering's own recursion, so the bound
    /// is what keeps a request from choosing how much stack one field resolution takes.
    fn condition(&mut self, condition: &Json, depth: usize) -> Result<String, String> {
        if depth > MAX_CONDITION_DEPTH {
            return Err(format!("{TOO_DEEP_A_CONDITION} {MAX_CONDITION_DEPTH}."));
        }
        let members = match condition {
            Json::Null => return Ok(String::new()),
            Json::Object(members) if members.is_empty() => return Ok(String::new()),
            Json::Object(members) => members.clone(),
            other => return Err(format!("{A_CONDITION_IS_AN_OBJECT} {other}.")),
        };
        let mut clauses = Vec::with_capacity(members.len());
        for (name, stated) in &members {
            let clause = match name.as_str() {
                AND_GROUP => self.group(stated, "AND", depth)?,
                OR_GROUP => self.group(stated, "OR", depth)?,
                NOT_GROUP => {
                    let inner = self.condition(stated, depth.saturating_add(1))?;
                    match inner.is_empty() {
                        true => String::new(),
                        false => format!("NOT ({inner})"),
                    }
                }
                _ => self.compared(name, stated)?,
            };
            if !clause.is_empty() {
                clauses.push(clause);
            }
        }
        Ok(clauses.join(" AND "))
    }

    /// A GROUP of conditions, joined with `joiner` and parenthesised, so the grouping the
    /// request states is the grouping the statement carries.
    ///
    /// Every condition of the group sits one level deeper than the group itself, which is what
    /// [`Self::condition`] bounds.
    fn group(&mut self, group: &Json, joiner: &str, depth: usize) -> Result<String, String> {
        let Some(entries) = group.as_array() else {
            return Err(format!("{A_GROUP_IS_A_LIST} {group}."));
        };
        let mut clauses = Vec::with_capacity(entries.len());
        for entry in entries {
            let clause = self.condition(entry, depth.saturating_add(1))?;
            if !clause.is_empty() {
                clauses.push(clause);
            }
        }
        match clauses.is_empty() {
            true => Ok(String::new()),
            false => Ok(format!("({})", clauses.join(&format!(" {joiner} ")))),
        }
    }

    /// Every comparison one condition states about one column, joined with `AND`.
    fn compared(&mut self, column: &str, stated: &Json) -> Result<String, String> {
        let Some(operators) = stated.as_object() else {
            return Err(format!(
                "{A_COMPARISON_NAMES_AN_OPERATOR} {column:?} carries {stated}."
            ));
        };
        let written = self.column(&Json::String(column.to_string()))?;
        let mut clauses = Vec::with_capacity(operators.len());
        for (operator, operand) in operators {
            clauses.push(self.clause(&written, operator, operand)?);
        }
        Ok(clauses.join(" AND "))
    }

    /// ONE comparison of one column: the operator the condition names, over the operand it
    /// carries.
    ///
    /// The operators are the ones a relational-database condition states — the orderings,
    /// the two text searches, the prefix, the range, whether the column holds a value at all
    /// and the length of what it holds, each written as the SQL that asks it. An operator
    /// this module publishes none of is refused, so a condition reaching for one fails
    /// loudly rather than selecting every row.
    fn clause(
        &mut self,
        column: &str,
        operator: &str,
        operand: &Json,
    ) -> Result<String, String> {
        let compared = |renderer: &mut Self, sql: &str| {
            format!("{column} {sql} {}", renderer.bind(operand))
        };
        match operator {
            "eq" => Ok(compared(self, "=")),
            "ne" => Ok(compared(self, "<>")),
            "gt" => Ok(compared(self, ">")),
            "ge" => Ok(compared(self, ">=")),
            "lt" => Ok(compared(self, "<")),
            "le" => Ok(compared(self, "<=")),
            "beginsWith" => {
                let bound = self.bind(&prefixed(operand, "", LIKE_WILDCARD)?);
                Ok(format!("{column} LIKE {bound}"))
            }
            "contains" => {
                let bound = self.bind(&prefixed(operand, LIKE_WILDCARD, LIKE_WILDCARD)?);
                Ok(format!("{column} LIKE {bound}"))
            }
            "notContains" => {
                let bound = self.bind(&prefixed(operand, LIKE_WILDCARD, LIKE_WILDCARD)?);
                Ok(format!("{column} NOT LIKE {bound}"))
            }
            "between" => {
                let (low, high) = pair(operand)?;
                Ok(format!(
                    "{column} BETWEEN {} AND {}",
                    self.bind(&low),
                    self.bind(&high)
                ))
            }
            "attributeExists" => match operand.as_bool() {
                Some(true) => Ok(format!("{column} IS NOT NULL")),
                Some(false) => Ok(format!("{column} IS NULL")),
                None => Err(format!("{AN_EXISTENCE_IS_A_FLAG} {operand}.")),
            },
            // The LENGTH of what the column holds, compared the way the column itself is:
            // the operand is a comparison of its own.
            "size" => {
                let sized = format!("LENGTH({column})");
                let Some(operators) = operand.as_object() else {
                    return Err(format!("{A_SIZE_NAMES_AN_OPERATOR} {operand}."));
                };
                let mut clauses = Vec::with_capacity(operators.len());
                for (operator, inner) in operators {
                    if operator == "size" {
                        return Err(format!("{A_SIZE_NAMES_AN_OPERATOR} {operand}."));
                    }
                    clauses.push(self.clause(&sized, operator, inner)?);
                }
                Ok(clauses.join(" AND "))
            }
            other => Err(format!("{NO_SUCH_OPERATOR}{other}")),
        }
    }

    /// One column of a statement, as the dialect writes its name.
    fn column(&self, named: &Json) -> Result<String, String> {
        self.identifier(named)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("{A_COLUMN_IS_A_NAME} {named}."))
    }

    /// `named` as the identifier it names, quoted as this dialect quotes one — and `None`
    /// for a value that names no identifier at all.
    ///
    /// A name written in PARTS (a table qualified by its schema) is quoted part by part and
    /// joined, so each part names one object: the parts are the members of a list, or the
    /// dot-separated segments of a single name.
    fn identifier(&self, named: &Json) -> Option<String> {
        let parts: Vec<String> = match named {
            Json::String(text) => text.split('.').map(str::to_string).collect(),
            Json::Array(parts) => parts
                .iter()
                .filter_map(|part| part.as_str().map(str::to_string))
                .collect(),
            _ => return None,
        };
        let quote = self.dialect.quote();
        let written: Vec<String> = parts
            .iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .map(|part| format!("{quote}{}{quote}", part.replace(quote, &format!("{quote}{quote}"))))
            .collect();
        (!written.is_empty()).then(|| written.join("."))
    }

    /// Bind `value` to a placeholder of its own and answer the placeholder.
    ///
    /// Every value a statement compares or assigns travels this way — in `variableMap`,
    /// never in the statement's text — so the text a request carries is decided by the
    /// resolver's own code and never by the values it was called with. A value wrapped in a
    /// TYPE HINT binds the value it wraps and states the hint beside it.
    fn bind(&mut self, value: &Json) -> String {
        let (value, hint) = hinted(value);
        self.bound = self.bound.saturating_add(1);
        let name = format!(":{PLACEHOLDER_PREFIX}{}", self.bound);
        self.variables.insert(name.clone(), value);
        if let Some(hint) = hint {
            self.hints.insert(name.clone(), Json::String(hint));
        }
        name
    }
}

/// The value a hinted value wraps and the hint it states, and the value itself with no hint
/// where it is not wrapped.
fn hinted(value: &Json) -> (Json, Option<String>) {
    let hint = value
        .get(TYPE_HINT_MEMBER)
        .and_then(Json::as_str)
        .map(str::to_string);
    match hint {
        Some(hint) => (
            value
                .get(HINTED_VALUE_MEMBER)
                .cloned()
                .unwrap_or(Json::Null),
            Some(hint),
        ),
        None => (value.clone(), None),
    }
}

/// `operand` as the text a `LIKE` comparison matches with, wrapped in the wildcards the
/// comparison places around it.
fn prefixed(operand: &Json, before: &str, after: &str) -> Result<Json, String> {
    let text = match operand {
        Json::String(text) => text.clone(),
        Json::Number(number) => number.to_string(),
        other => return Err(format!("{A_MATCH_IS_TEXT} {other}.")),
    };
    Ok(Json::String(format!("{before}{text}{after}")))
}

/// The two bounds a range comparison states.
fn pair(operand: &Json) -> Result<(Json, Json), String> {
    match operand.as_array() {
        Some(bounds) if bounds.len() == 2 => Ok((bounds[0].clone(), bounds[1].clone())),
        _ => Err(format!("{A_RANGE_IS_TWO_BOUNDS} {operand}.")),
    }
}

/// A `LIMIT` or an `OFFSET` clause, from the count the request states — and nothing at all
/// where it states none.
///
/// The count is written into the statement rather than bound, because it is a bound on the
/// rows and not a value the statement compares: it has to be a non-negative whole number,
/// and anything else is refused rather than written.
fn bounded(clause: &str, count: &Json) -> Result<String, String> {
    match count {
        Json::Null => Ok(String::new()),
        Json::Number(number) => match number.as_i64().filter(|count| *count >= 0) {
            Some(count) => Ok(format!(" {clause} {count}")),
            None => Err(format!("{A_BOUND_IS_A_WHOLE_COUNT} {clause} carries {count}.")),
        },
        other => Err(format!("{A_BOUND_IS_A_WHOLE_COUNT} {clause} carries {other}.")),
    }
}

/// The members a statement builder reads its input out of.
const TABLE_MEMBER: &str = "table";
const COLUMNS_MEMBER: &str = "columns";
const WHERE_MEMBER: &str = "where";
const ORDER_BY_MEMBER: &str = "orderBy";
const LIMIT_MEMBER: &str = "limit";
const OFFSET_MEMBER: &str = "offset";
const COLUMN_MEMBER: &str = "column";
const DIRECTION_MEMBER: &str = "dir";

/// The names a condition groups other conditions under, rather than naming a column.
const AND_GROUP: &str = "and";
const OR_GROUP: &str = "or";
const NOT_GROUP: &str = "not";

/// What a selection naming no column reports: every column of the table.
const ALL_COLUMNS: &str = "*";

/// The two directions an ordering is in.
const ASCENDING: &str = "ASC";
const DESCENDING: &str = "DESC";

/// What an ordering in no direction of its own states.
const ONE_OF_THE_DIRECTIONS: &str = "An ordering is in one of the directions";

/// The wildcard a `LIKE` comparison matches any text with.
const LIKE_WILDCARD: &str = "%";

/// How many GROUPS deep one condition of a statement may nest.
///
/// A bound on the work — and on the stack — one field resolution can be made to take: the
/// nesting is the resolver's own to choose, and walking it is this rendering's recursion, so a
/// condition past this is refused rather than walked. Set far above any condition a resolver
/// writes, and a `size` comparison nests no further at all ([`Renderer::clause`]).
const MAX_CONDITION_DEPTH: usize = 16;

/// The prefix of every placeholder a rendering binds a value to, which `variableMap` keys
/// each value by.
const PLACEHOLDER_PREFIX: &str = "appsync_";

/// The sentences a refused helper call states. Each names the member or the value to
/// correct, and interpolates only what the resolver's own code passed.
const A_BUILDER_TAKES_AN_OBJECT: &str =
    "A statement is built from an object stating what it is about, and";
const A_STATEMENT_NAMES_A_TABLE: &str = "A statement names the table it is about, and so must";
const A_STATEMENT_IS_NOT_EMPTY: &str = "A statement carries SQL text, and this one carries none.";
const A_TEMPLATE_CARRIES_ITS_PARTS: &str =
    "An interpolated statement is written as a template whose parts are text.";
const A_REQUEST_CARRIES_A_STATEMENT: &str =
    "A relational database request carries at least one statement, and this one carries none.";
const TOO_MANY_STATEMENTS: &str =
    "A relational database request carries at most";
const A_REQUEST_RENDERS_BUILT_STATEMENTS: &str =
    "A relational database request renders statements built by select, insert, update, \
     remove or sql, and this one carries a value that is none of them.";
const A_WRITE_ASSIGNS_VALUES: &str = "A write states the value of every column it sets, in";
const A_CONDITION_IS_AN_OBJECT: &str =
    "A condition is an object naming the columns it compares, and this one is";
const A_GROUP_IS_A_LIST: &str = "A group of conditions is a list of them, and this one is";
const TOO_DEEP_A_CONDITION: &str = "A condition nests at most this many groups deep:";
const A_COMPARISON_NAMES_AN_OPERATOR: &str =
    "A comparison names the operator it compares with, and the condition on";
const A_COLUMN_IS_A_NAME: &str = "A column is named by text, and this one is";
const A_MATCH_IS_TEXT: &str = "A text match compares against text, and this one compares against";
const A_RANGE_IS_TWO_BOUNDS: &str =
    "A range compares against its two bounds, and this one compares against";
const AN_EXISTENCE_IS_A_FLAG: &str =
    "Whether a column holds a value is asked with true or false, and this condition asks it \
     with";
const A_SIZE_NAMES_AN_OPERATOR: &str =
    "The length of a column is compared the way the column is, with an operator of its own, \
     and this condition carries";
const A_BOUND_IS_A_WHOLE_COUNT: &str =
    "A row bound is a whole count of rows, and the one in";
const NO_SUCH_OPERATOR: &str = "No such comparison: ";
const NO_SUCH_STATEMENT: &str = "No such statement: ";

#[cfg(test)]
mod tests {
    use super::*;

    /// Render one statement built by `helper` out of `input`, in MySQL's dialect.
    fn rendered(helper: &str, input: Json) -> Result<(String, Json), String> {
        let described = call(helper, &[input])?.expect("the helper is published");
        let request = call("createMySQLStatement", &[described])?
            .expect("the helper is published");
        let statement = request[STATEMENTS_MEMBER][0]
            .as_str()
            .unwrap_or_default()
            .to_string();
        Ok((statement, request[VARIABLE_MAP_MEMBER].clone()))
    }

    /// RULE: a built statement names its table and columns as the dialect quotes an
    /// identifier, and every value it compares or assigns travels in `variableMap` under a
    /// placeholder of its own — never in the statement's text.
    ///
    /// The rows span the four statement kinds, the comparisons a condition states, a
    /// grouped condition, an ordering, a row bound, and a value carrying a quote.
    #[test]
    fn a_built_statement_binds_every_value_it_carries() {
        let rows: &[(&str, &str, Json, &str, Json)] = &[
            (
                "a selection of every column by one comparison",
                SELECT,
                json!({"table": "widgets", "where": {"id": {"eq": 1}}}),
                "SELECT * FROM `widgets` WHERE `id` = :appsync_1",
                json!({":appsync_1": 1}),
            ),
            (
                "a selection of named columns, ordered and bounded",
                SELECT,
                json!({
                    "table": "widgets",
                    "columns": ["id", "name"],
                    "orderBy": [{"column": "name", "dir": "desc"}],
                    "limit": 10,
                    "offset": 5,
                }),
                "SELECT `id`, `name` FROM `widgets` ORDER BY `name` DESC LIMIT 10 OFFSET 5",
                json!({}),
            ),
            (
                "a table qualified by its schema",
                SELECT,
                json!({"table": "app.widgets"}),
                "SELECT * FROM `app`.`widgets`",
                json!({}),
            ),
            (
                "a prefix match",
                SELECT,
                json!({"table": "widgets", "where": {"name": {"beginsWith": "wid"}}}),
                "SELECT * FROM `widgets` WHERE `name` LIKE :appsync_1",
                json!({":appsync_1": "wid%"}),
            ),
            (
                "a range, grouped with a prefix match so the order is the request's own",
                SELECT,
                json!({
                    "table": "widgets",
                    "where": {"and": [
                        {"id": {"between": [1, 9]}},
                        {"name": {"beginsWith": "wid"}},
                    ]},
                }),
                "SELECT * FROM `widgets` WHERE (`id` BETWEEN :appsync_1 AND :appsync_2 AND \
                 `name` LIKE :appsync_3)",
                json!({":appsync_1": 1, ":appsync_2": 9, ":appsync_3": "wid%"}),
            ),
            (
                "a grouped condition",
                SELECT,
                json!({
                    "table": "widgets",
                    "where": {"or": [{"id": {"eq": 1}}, {"id": {"eq": 2}}]},
                }),
                "SELECT * FROM `widgets` WHERE (`id` = :appsync_1 OR `id` = :appsync_2)",
                json!({":appsync_1": 1, ":appsync_2": 2}),
            ),
            (
                "whether a column holds a value at all",
                SELECT,
                json!({"table": "widgets", "where": {"name": {"attributeExists": true}}}),
                "SELECT * FROM `widgets` WHERE `name` IS NOT NULL",
                json!({}),
            ),
            (
                "how long what a column holds is",
                SELECT,
                json!({"table": "widgets", "where": {"name": {"size": {"gt": 3}}}}),
                "SELECT * FROM `widgets` WHERE LENGTH(`name`) > :appsync_1",
                json!({":appsync_1": 3}),
            ),
            (
                "an insert whose value carries a quote",
                INSERT,
                json!({"table": "widgets", "values": {"name": "it's mine"}}),
                "INSERT INTO `widgets` (`name`) VALUES (:appsync_1)",
                json!({":appsync_1": "it's mine"}),
            ),
            (
                "an update of one row",
                UPDATE,
                json!({
                    "table": "widgets",
                    "values": {"name": "renamed"},
                    "where": {"id": {"eq": 7}},
                }),
                "UPDATE `widgets` SET `name` = :appsync_1 WHERE `id` = :appsync_2",
                json!({":appsync_1": "renamed", ":appsync_2": 7}),
            ),
            (
                "a delete of the rows a comparison matches",
                REMOVE,
                json!({"table": "widgets", "where": {"name": {"contains": "old"}}}),
                "DELETE FROM `widgets` WHERE `name` LIKE :appsync_1",
                json!({":appsync_1": "%old%"}),
            ),
        ];
        for (case, helper, input, expected, variables) in rows {
            let (statement, bound) = rendered(helper, input.clone())
                .unwrap_or_else(|error| panic!("case {case:?}: expected a statement, got {error}"));
            let expected = expected.split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(
                statement, expected,
                "case {case:?}: expected {expected:?}, got {statement:?}"
            );
            assert_eq!(
                &bound, variables,
                "case {case:?}: expected the values {variables}, got {bound}"
            );
        }
    }

    /// RULE: the dialect a request is rendered in decides how an identifier is quoted, and
    /// nothing else about the statement.
    #[test]
    fn the_dialect_decides_how_an_identifier_is_quoted() {
        let described = call(SELECT, &[json!({"table": "widgets", "columns": ["id"]})])
            .expect("the builder is published")
            .expect("the builder is published");
        for (case, helper, expected) in [
            ("MySQL", "createMySQLStatement", "SELECT `id` FROM `widgets`"),
            ("PostgreSQL", "createPgStatement", "SELECT \"id\" FROM \"widgets\""),
        ] {
            let request = call(helper, std::slice::from_ref(&described))
                .expect("the builder is published")
                .expect("the builder is published");
            let statement = request[STATEMENTS_MEMBER][0].as_str().unwrap_or_default();
            assert_eq!(
                statement, expected,
                "case {case:?}: expected {expected:?}, got {statement:?}"
            );
            assert_eq!(
                request[VERSION_MEMBER].as_str(),
                Some(STATEMENT_VERSION),
                "case {case:?}: a request states its version, got {request}"
            );
        }
    }

    /// RULE: an interpolated statement carries the template's own text with every
    /// interpolated value bound, and a type hint states the type of the value it wraps.
    #[test]
    fn an_interpolated_statement_binds_the_values_between_its_parts() {
        let described = call(
            SQL,
            &[
                json!(["select * from widgets where id = ", " and due < "]),
                json!([4, {TYPE_HINT_MEMBER: "TIMESTAMP", HINTED_VALUE_MEMBER: "2026-01-02"}]),
            ],
        )
        .expect("the builder is published")
        .expect("the builder is published");
        let request = call("createPgStatement", &[described])
            .expect("the builder is published")
            .expect("the builder is published");
        assert_eq!(
            request[STATEMENTS_MEMBER][0].as_str(),
            Some("select * from widgets where id = :appsync_1 and due < :appsync_2"),
            "expected the template's own text with its values bound, got {request}"
        );
        assert_eq!(
            request[VARIABLE_MAP_MEMBER],
            json!({":appsync_1": 4, ":appsync_2": "2026-01-02"}),
            "expected both values bound, got {request}"
        );
        assert_eq!(
            request[VARIABLE_TYPE_HINT_MAP_MEMBER],
            json!({":appsync_2": "TIMESTAMP"}),
            "expected the hinted value to state its type, got {request}"
        );
    }

    /// RULE: a call this module cannot act on is refused, naming what to correct — and a
    /// helper it publishes none of is reported as no helper rather than as a fault.
    #[test]
    fn a_call_this_module_cannot_act_on_is_refused() {
        let rows: &[(&str, &str, Vec<Json>)] = &[
            ("a builder handed no object", SELECT, vec![json!("widgets")]),
            ("a statement naming no table", SELECT, vec![json!({"columns": ["id"]})]),
            (
                "a comparison this module publishes none of",
                SELECT,
                vec![json!({"table": "t", "where": {"id": {"sorta": 1}}})],
            ),
            (
                "a condition that is not an object",
                SELECT,
                vec![json!({"table": "t", "where": "id = 1"})],
            ),
            (
                "a comparison carrying no operator",
                SELECT,
                vec![json!({"table": "t", "where": {"id": 1}})],
            ),
            (
                "a row bound that is no count",
                SELECT,
                vec![json!({"table": "t", "limit": "ten"})],
            ),
            ("a write assigning nothing", INSERT, vec![json!({"table": "t"})]),
            (
                "a condition nesting one group deeper than one may",
                SELECT,
                vec![json!({"table": "t", "where": nested(MAX_CONDITION_DEPTH + 1)})],
            ),
        ];
        for (case, helper, args) in rows {
            // The refusal is stated either where the statement is BUILT or where it is
            // rendered: both are the resolver's own call, and either way nothing is
            // rendered.
            let built = call(helper, args);
            let refused = match built {
                Err(_) => true,
                Ok(described) => {
                    let described = described.expect("the builder is published");
                    call("createMySQLStatement", &[described]).is_err()
                }
            };
            assert!(refused, "case {case:?}: expected the call to be refused");
        }
        let unknown = call("createOracleStatement", &[]);
        assert!(
            matches!(unknown, Ok(None)),
            "a helper this module publishes none of is reported as no helper, got {unknown:?}"
        );
        // And the condition one group SHALLOWER than the bound renders, so the bound refuses
        // the nesting past it rather than nesting at all.
        let deep = rendered(
            SELECT,
            json!({"table": "t", "where": nested(MAX_CONDITION_DEPTH)}),
        );
        assert!(
            deep.is_ok(),
            "a condition within the nesting bound renders, got {deep:?}"
        );
    }

    /// A condition nesting `depth` groups deep, each holding the next, with one comparison at
    /// the bottom.
    fn nested(depth: usize) -> Json {
        let mut condition = json!({"id": {"eq": 1}});
        for _ in 0..depth {
            condition = json!({AND_GROUP: [condition]});
        }
        condition
    }
}
