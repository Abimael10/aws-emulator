// SPDX-License-Identifier: CC0-1.0

//! The statement as the SQL ENGINE is asked to run it: the client's own tokens, with the
//! values this SESSION carries substituted for the session functions and system variables
//! that name them, and with the two dialects' own syntax written in the form the engine
//! parses.
//!
//! Two kinds of rewriting live here, and nothing else:
//!
//! * SUBSTITUTION of a session value — what a server does with `current_user`: the value
//!   comes from the connection, not from the statement. Only WORD tokens are replaced, so a
//!   string literal that happens to spell one is left exactly as the client wrote it.
//! * TRANSLATION of a dialect's syntax for the same operation into the engine's: the
//!   PostgreSQL `::` cast is the engine's `CAST(… AS …)`, and a MySQL table's storage
//!   engine, character set and auto-increment column are declared differently. The
//!   statement still does what the client asked; only the spelling changes.
//!
//! Nothing here answers a statement. A statement this module cannot translate is handed to
//! the engine as the client wrote it, and the engine's own refusal is what the client is
//! told.

use sqlparser::tokenizer::Token;

use super::sql_engine::{EngineError, EngineErrorKind};
use super::wire_session::{server_version_banner, SessionFacts};
use super::EngineWire;

/// The statement as the engine is asked to run it; see this module's own notes.
pub fn render_for_engine(facts: &SessionFacts, tokens: &[Token]) -> String {
    render_at(facts, tokens, 0)
}

/// How a reference to a system variable is written in the MySQL family's grammar: `@@name`,
/// or `@@` on its own followed by the name.
///
/// `pub(super)` so the server that decides what an ASSIGNMENT to one means reads the same
/// prefix this pass does ([`super::wire_session`]).
pub(super) const MYSQL_SYSTEM_VARIABLE_PREFIX: &str = "@@";

/// The refusal a statement written in a form the SESSION'S OWN dialect does not define is
/// answered with, or `None` for a statement its dialect reads.
///
/// Two forms are refused here, and both are the same rule: the two engines are not one
/// generic SQL front end, so what one family defines the other does not have.
///
/// * The `@@<name>` reference is the MySQL family's spelling of a system variable, and the
///   PostgreSQL grammar has no such form: a statement carrying one does not parse there. The
///   refusal is this server's rather than the embedded engine's so that it carries
///   PostgreSQL's own sentence for a token its grammar cannot read.
/// * A FUNCTION only the other family defines (`current_setting` and the `pg_*` functions on
///   PostgreSQL, `UTC_TIMESTAMP` and `LAST_INSERT_ID` and the rest on MySQL) is refused as
///   the engine refuses an undefined function, in that engine's own words. The embedded
///   engine has both families' functions registered on one connection, which is exactly what
///   a client probing which engine it is talking to must not see.
#[must_use]
pub fn dialect_refusal(facts: &SessionFacts, tokens: &[Token]) -> Option<EngineError> {
    if facts.wire == EngineWire::Postgres {
        if let Some(named) = mysql_system_variable_at(tokens) {
            return Some(EngineError::new(
                EngineErrorKind::Syntax,
                format!("syntax error at or near \"{named}\""),
            ));
        }
    }
    tokens
        .iter()
        .enumerate()
        .filter_map(|(at, token)| match token {
            // Only a word that is CALLED is a function reference; a column or an alias that
            // happens to share a function's name is the identifier it is.
            Token::Word(word)
                if next_significant(tokens, at + 1)
                    .and_then(|next| tokens.get(next))
                    .is_some_and(|next| matches!(next, Token::LParen)) =>
            {
                Some(word.value.clone())
            }
            _ => None,
        })
        .find(|called| other_familys_function(facts.wire, called))
        .map(|called| undefined_function(facts, &called))
}

/// Whether `function` is one a session on `wire` does not have because it belongs to the
/// OTHER engine family; see [`dialect_refusal`].
fn other_familys_function(wire: EngineWire, function: &str) -> bool {
    if super::engine_time::is_defined_on(function, wire) == Some(false) {
        return true;
    }
    // The MySQL SESSION function, which no PostgreSQL server has.
    if wire == EngineWire::Postgres && super::mysql_insert_id::is_mysql_only_function(function) {
        return true;
    }
    // The PostgreSQL SERVER functions, which no MySQL server has. `md5` is absent from this:
    // both families define it, under that name, over the same bytes.
    wire == EngineWire::MySql && super::pg_server::is_postgres_only_function(function)
}

/// The system-variable reference a statement carries, as the client wrote it, or `None` where
/// it carries none.
///
/// A reference is the `@@` prefix IMMEDIATELY followed by the variable's NAME — `@@name` as
/// one token, or `@@` followed by the name (and by a scope and a period). The bare operator is
/// not one: `@@` is also PostgreSQL's own text-search match operator, and a statement written
/// in its grammar is the statement it is.
fn mysql_system_variable_at(tokens: &[Token]) -> Option<String> {
    for (at, token) in tokens.iter().enumerate() {
        let text = token.to_string();
        let Some(rest) = text.strip_prefix(MYSQL_SYSTEM_VARIABLE_PREFIX) else {
            continue;
        };
        // Only where a VALUE can begin: a reference is an expression of its own, so one with
        // an operand to its left is the binary operator PostgreSQL defines instead.
        if !begins_an_expression(tokens, at) {
            continue;
        }
        // The name may be joined to the prefix, or be the next token.
        if !rest.is_empty() {
            return Some(text);
        }
        if let Some(Token::Word(word)) = next_significant(tokens, at + 1).and_then(|next| tokens.get(next)) {
            return Some(format!("{MYSQL_SYSTEM_VARIABLE_PREFIX}{}", word.value));
        }
    }
    None
}

/// Whether a VALUE can begin at `at`: nothing precedes it, or what does cannot end an
/// expression.
///
/// A keyword, a comma, an opening parenthesis and an operator all leave the next token to
/// begin a value; an identifier, a literal, a number and a closing parenthesis are values
/// themselves, so what follows one is an operator applied to it.
fn begins_an_expression(tokens: &[Token], at: usize) -> bool {
    let before = (0..at)
        .rev()
        .find(|index| !matches!(tokens.get(*index), Some(Token::Whitespace(_))))
        .and_then(|index| tokens.get(index));
    match before {
        None => true,
        Some(Token::Word(word)) => word.keyword != ::sqlparser::keywords::Keyword::NoKeyword,
        Some(
            Token::RParen
            | Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::Number(..)
            | Token::Placeholder(_),
        ) => false,
        Some(_) => true,
    }
}

/// What the session's own engine answers for a function it does not have.
fn undefined_function(facts: &SessionFacts, called: &str) -> EngineError {
    match facts.wire {
        // `ER_SP_DOES_NOT_EXIST`: MySQL names the schema it looked for the function in,
        // which is the database the session is on.
        EngineWire::MySql => EngineError::new(
            EngineErrorKind::UndefinedObject,
            format!("FUNCTION {}.{called} does not exist", facts.database),
        )
        .with_mysql_error(1305, "42000"),
        EngineWire::Postgres => EngineError::new(
            EngineErrorKind::UndefinedObject,
            format!("function {called}() does not exist"),
        )
        .with_sqlstate("42883"),
    }
}

/// How deep a NESTED catalog-function call this rewriting follows.
///
/// The only rewriting here that reads an expression inside another one is the catalog
/// accessor, whose arguments are rendered by this same pass ([`call_arguments`]): the depth
/// bounds that nesting, so a statement cannot drive this pass to the depth of its own
/// parentheses. Past it the statement is handed to the engine as the client wrote it, which
/// is this module's documented answer for anything it cannot translate.
const MAX_CALL_DEPTH: usize = 16;

/// [`render_for_engine`], `depth` calls into the arguments of a nested catalog-function
/// call.
fn render_at(facts: &SessionFacts, tokens: &[Token], depth: usize) -> String {
    // The pieces of the rendered statement, kept separate rather than concatenated,
    // because two of the rewritings below look BACK at what has already been emitted: a
    // cast takes the expression before the `::`, and an auto-increment column takes the
    // type before the keyword.
    let mut out: Vec<String> = Vec::new();
    let mut index = 0usize;
    // Whether a catalog-accessor call is still followed: a call whose parentheses never
    // close turns this off, so an unreadable statement is walked once rather than once per
    // call it names.
    let mut follow_calls = true;
    // Where a MySQL `CREATE TABLE`'s COLUMN-DEFINITION list closes, for the statements that
    // have one. Everything after it is table options, which the engine declares differently
    // (see `MYSQL_TABLE_OPTIONS`); everything before it is the client's own column list.
    let column_list_close = match facts.wire {
        EngineWire::MySql => mysql_column_list_close(tokens),
        EngineWire::Postgres => None,
    };
    // Where a MySQL `CREATE TABLE`'s table-constraint clauses are, and what each becomes in
    // the engine's own grammar ([`mysql_table_constraints`]).
    let mut table_constraints = match column_list_close {
        Some(close) => mysql_table_constraints(tokens, close),
        None => Vec::new(),
    };
    // The PRIMARY KEY of an auto-increment column, where the client declared it as a clause
    // of its own: the engine declares that key — and the counter behind it — on the column
    // itself, so the clause leaves the list ([`mysql_auto_increment_key_clause`]).
    let auto_increment_key_clause = column_list_close
        .and_then(|close| mysql_auto_increment_key_clause(tokens, close))
        .inspect(|clause| table_constraints.push(clause.rewrite()));
    // Whether the auto-increment column being declared is still waiting for the PRIMARY KEY
    // its counter is declared with, which the client may write after the keyword.
    let mut counter_pending = false;
    while index < tokens.len() {
        let token = &tokens[index];
        let text = token.to_string();
        // The column list is closed: the table is declared, and what follows is the
        // options.
        if Some(index) == column_list_close {
            out.push(text);
            break;
        }
        // A table-constraint clause of a MySQL `CREATE TABLE`, written as the engine
        // declares the same constraint.
        if let Some(rewrite) = table_constraints
            .iter()
            .find(|rewrite| rewrite.from == index)
        {
            match &rewrite.declared {
                Some(declared) => out.push(declared.clone()),
                // A clause the engine declares nothing for leaves the list, and the comma
                // that separated it from the clause before goes with it.
                None => drop_the_separating_comma(&mut out),
            }
            index = rewrite.until;
            continue;
        }
        // A MySQL system variable: `@@name`, `@@scope.name`, or `@@` followed by either.
        // Only on the MySQL wire: the form is that family's own, and the other wire's
        // statements never reach this pass ([`dialect_refusal`]).
        if let Some(name) = text
            .strip_prefix(MYSQL_SYSTEM_VARIABLE_PREFIX)
            .filter(|_| facts.wire == EngineWire::MySql)
        {
            let (name, consumed) = system_variable_name(tokens, index, name);
            out.push(quote_value(facts.setting(&name)));
            index += consumed;
            continue;
        }
        // The PostgreSQL cast operator: `<expression>::<type>`, which is the engine's
        // `CAST(<expression> AS <type>)`.
        if matches!(token, Token::DoubleColon) {
            let operand = take_operand(&mut out);
            let (target, consumed) = cast_target(tokens, index + 1);
            out.push(format!("CAST({operand} AS {target})"));
            index += 1 + consumed;
            continue;
        }
        match token {
            Token::Word(word) if word.quote_style.is_none() => {
                let lowered = word.value.to_ascii_lowercase();
                // `pg_catalog.<relation>`: the catalog schema is on every session's
                // search path, so the qualified name and the bare one are the same
                // relation — the qualifier is dropped and the relation looked up as
                // itself.
                if facts.wire == EngineWire::Postgres
                    && lowered == PG_CATALOG_SCHEMA
                    && matches!(tokens.get(index + 1), Some(Token::Period))
                {
                    index += 2;
                    continue;
                }
                // A MySQL auto-increment column is the engine's own rowid column, which it
                // declares by the column's TYPE rather than with a keyword — and whose
                // KEYS come from a counter of its own, which the engine keeps for a column
                // declared `AUTOINCREMENT`.
                if facts.wire == EngineWire::MySql && lowered == "auto_increment" {
                    widen_to_rowid_type(&mut out);
                    counter_pending = !declare_the_counter(&mut out);
                    // The key the client declared as a clause of its own is declared here
                    // instead, because that is where the engine declares the counter.
                    if counter_pending && auto_increment_key_clause.is_some() {
                        out.push(" PRIMARY KEY AUTOINCREMENT".to_string());
                        counter_pending = false;
                    }
                    index += 1;
                    continue;
                }
                // The CHARACTER SET and the COLLATION a MySQL column is declared with: the
                // engine stores text as UTF-8 and names its collations differently, so the
                // clause is not part of the declaration it is handed. The column holds the
                // same characters either way — which is what a client reading its rows back
                // sees — and a statement that names one is not refused for the spelling.
                if facts.wire == EngineWire::MySql {
                    if let Some(consumed) = mysql_character_set_clause(tokens, index) {
                        index += consumed;
                        continue;
                    }
                }
                let next = next_significant(tokens, index + 1);
                let called = matches!(next.and_then(|at| tokens.get(at)), Some(Token::LParen));
                // A CATALOG-ACCESSOR function (`pg_get_userbyid`, `pg_has_role`) is the
                // lookup in the catalog it reads: the call is replaced by that query over
                // the catalog this server rebuilds from its own state, with the client's own
                // arguments inside it (each rendered by this same pass, so a session value
                // or a cast in one is translated too).
                //
                // WHICH function it is is decided before anything is rendered, so no other
                // call's arguments are walked; the nesting this follows is bounded
                // ([`MAX_CALL_DEPTH`]); the arguments it rendered are EMITTED even where the
                // call shape has no lookup, so no span is rendered twice; and a call whose
                // parentheses never close stops this pass from following any further call in
                // the statement, so one unreadable statement is walked once.
                if facts.wire == EngineWire::Postgres
                    && called
                    && follow_calls
                    && depth < MAX_CALL_DEPTH
                    && super::pg_server::catalog_function(&lowered).is_some()
                {
                    match call_arguments(facts, tokens, next.unwrap_or(index + 1), depth + 1) {
                        Some((arguments, consumed)) => {
                            out.push(
                                super::pg_server::catalog_function_lookup(
                                    &lowered,
                                    &arguments,
                                    &text_literal(&facts.user),
                                )
                                // A call shape the function does not take is emitted as the
                                // call it is, with its arguments as they were rendered: the
                                // engine then answers for it.
                                .unwrap_or_else(|| {
                                    format!("{}({})", word.value, arguments.join(", "))
                                }),
                            );
                            index = consumed;
                            continue;
                        }
                        None => follow_calls = false,
                    }
                }
                match session_function(facts, &lowered, called) {
                    // A function written with an empty argument list is substituted
                    // together with it.
                    Some(value) => {
                        out.push(value);
                        index += 1;
                        if called {
                            let close = next.and_then(|at| next_significant(tokens, at + 1));
                            if matches!(close.and_then(|at| tokens.get(at)), Some(Token::RParen)) {
                                index = close.map_or(index, |at| at + 1);
                            }
                        }
                        continue;
                    }
                    None => out.push(text),
                }
            }
            _ => out.push(text),
        }
        // The PRIMARY KEY the auto-increment column was waiting for, which the client may
        // write anywhere in that column's definition: the counter is declared with it, and a
        // definition that ends without one declares none.
        if counter_pending
            && (matches!(token, Token::Comma) || declare_the_counter(&mut out))
        {
            counter_pending = false;
        }
        index += 1;
    }
    out.concat()
}

/// The schema PostgreSQL's system catalogs live in, which is on every session's search path
/// — so `pg_catalog.pg_settings` and `pg_settings` name the same relation.
const PG_CATALOG_SCHEMA: &str = "pg_catalog";

/// `value` as a SQL TEXT literal, with the quotes inside it doubled, so a value a session
/// carries reaches the engine as one literal whatever it contains.
fn text_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The ARGUMENTS of a call whose opening parenthesis is at `open`, each rendered for the
/// engine by this same pass at `depth`, and the index just past the call's closing
/// parenthesis.
///
/// The arguments are split on the commas of the call ITSELF, so a comma inside a nested call
/// or a parenthesised expression stays part of the argument it is in.
///
/// `None` for a call whose parentheses do not close, which is a statement the engine is
/// handed as the client wrote it so that its own parse error is what the client reads.
fn call_arguments(
    facts: &SessionFacts,
    tokens: &[Token],
    open: usize,
    depth: usize,
) -> Option<(Vec<String>, usize)> {
    let mut nesting = 0usize;
    let mut arguments: Vec<String> = Vec::new();
    let mut from = open + 1;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::LParen => nesting += 1,
            Token::Comma if nesting == 1 => {
                arguments.push(render_at(facts, tokens.get(from..at)?, depth));
                from = at + 1;
            }
            Token::RParen => {
                nesting -= 1;
                if nesting == 0 {
                    let last = tokens.get(from..at)?;
                    if !last.is_empty() || !arguments.is_empty() {
                        arguments.push(render_at(facts, last, depth));
                    }
                    return Some((arguments, at + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Where a MySQL `CREATE TABLE … (<columns>) <options>` statement's COLUMN-DEFINITION list
/// closes: the index of the parenthesis that ends it, after which only table options can
/// follow.
///
/// `None` for every other statement, which is handed to the engine WHOLE — in particular
/// for `CREATE TABLE … AS SELECT …` and `CREATE TABLE … (<columns>) SELECT …`, where what
/// follows the table name (or the column list) is a QUERY and not options: dropping it
/// would create the table from something other than what the client asked for. The two
/// conditions are what tell them apart: the column list has to be the first thing after
/// the table name, and a statement that carries a `SELECT` at all is a create-table-as-
/// select whatever its shape.
fn mysql_column_list_close(tokens: &[Token]) -> Option<usize> {
    if !leads_with_create_table(tokens) {
        return None;
    }
    let significant: Vec<usize> = (0..tokens.len())
        .filter(|at| !matches!(tokens.get(*at), Some(Token::Whitespace(_))))
        .collect();
    let word_at = |at: usize| match tokens.get(at) {
        Some(Token::Word(word)) => Some(word.value.to_ascii_uppercase()),
        _ => None,
    };
    // A query anywhere in the statement makes it a create-table-as-select.
    if significant
        .iter()
        .any(|at| word_at(*at).is_some_and(|word| word == "SELECT"))
    {
        return None;
    }
    let mut cursor = significant
        .iter()
        .position(|at| word_at(*at).is_some_and(|word| word == "TABLE"))?
        + 1;
    // The optional `IF NOT EXISTS`, then the table's own name, which may be qualified by
    // the database it is in.
    while significant
        .get(cursor)
        .and_then(|at| word_at(*at))
        .is_some_and(|word| matches!(word.as_str(), "IF" | "NOT" | "EXISTS"))
    {
        cursor += 1;
    }
    significant.get(cursor).and_then(|at| word_at(*at))?;
    cursor += 1;
    if matches!(
        significant.get(cursor).and_then(|at| tokens.get(*at)),
        Some(Token::Period)
    ) {
        cursor += 2;
    }
    // The column list has to start RIGHT HERE for the rest to be options.
    let open = *significant.get(cursor)?;
    if !matches!(tokens.get(open), Some(Token::LParen)) {
        return None;
    }
    let mut depth = 0usize;
    for at in significant.iter().skip(cursor) {
        match tokens.get(*at) {
            Some(Token::LParen) => depth += 1,
            Some(Token::RParen) => {
                depth -= 1;
                if depth == 0 {
                    return Some(*at);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the statement is a `CREATE TABLE` — including the `TEMPORARY` form — which is
/// the only statement whose trailing words are table options.
fn leads_with_create_table(tokens: &[Token]) -> bool {
    let words: Vec<String> = tokens
        .iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)))
        .take(4)
        .map(|token| match token {
            Token::Word(word) => word.value.to_ascii_uppercase(),
            other => other.to_string(),
        })
        .collect();
    words.first().is_some_and(|first| first == "CREATE")
        && words.iter().take(4).any(|word| word == "TABLE")
}

/// One TABLE-CONSTRAINT clause of a MySQL `CREATE TABLE`, and what the engine is handed in
/// its place.
struct TableConstraintRewrite {
    /// The index of the clause's first significant token.
    from: usize,
    /// The index the rendering carries on at: the separator after the clause, or — for a
    /// clause that leaves the list first — just past that separator.
    until: usize,
    /// How the engine declares the same constraint, or `None` for one it declares nothing
    /// for.
    declared: Option<String>,
}

/// The CLAUSES of the column list of a MySQL `CREATE TABLE` that closes at `close`: the
/// significant tokens of each, and the index of what ends it — the comma that separates it
/// from the next, or the parenthesis that closes the list.
///
/// One clause is one column definition or one table constraint, which is the unit both of
/// the rewritings below read.
fn mysql_column_list_clauses(tokens: &[Token], close: usize) -> Vec<(Vec<usize>, usize)> {
    let significant: Vec<usize> = (0..close)
        .filter(|at| !matches!(tokens.get(*at), Some(Token::Whitespace(_))))
        .collect();
    // Where the column list opens: the parenthesis the one at `close` matches, which is the
    // first one of the statement that is still open there.
    let mut depth = 0usize;
    let mut open = None;
    for at in &significant {
        match tokens.get(*at) {
            Some(Token::LParen) => {
                depth += 1;
                if depth == 1 {
                    open = Some(*at);
                    break;
                }
            }
            Some(Token::RParen) => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let Some(open) = open else {
        return Vec::new();
    };
    let mut clauses = Vec::new();
    let mut depth = 0usize;
    let mut clause: Vec<usize> = Vec::new();
    for at in significant.iter().copied().filter(|at| *at > open) {
        match tokens.get(at) {
            Some(Token::LParen) => depth += 1,
            Some(Token::RParen) => depth = depth.saturating_sub(1),
            Some(Token::Comma) if depth == 0 => {
                clauses.push((std::mem::take(&mut clause), at));
                continue;
            }
            _ => {}
        }
        clause.push(at);
    }
    clauses.push((clause, close));
    clauses
}

/// The TABLE-CONSTRAINT clauses of the MySQL `CREATE TABLE` whose column list closes at
/// `close`, each with what the engine is handed for it.
///
/// MySQL declares a table constraint with its own `KEY`/`INDEX` vocabulary, where the engine
/// (and the SQL standard) declares the same constraint with `CONSTRAINT <name>`:
///
/// * `UNIQUE [KEY|INDEX] [<name>] (<columns>)` is a NAMED UNIQUE CONSTRAINT — "a UNIQUE
///   index … creates a constraint such that all values in the index must be distinct" — so
///   it is declared as `CONSTRAINT <name> UNIQUE (<columns>)`, which is the same rule under
///   the same name. The name matters past the DDL: it is what the server names in the
///   refusal of a row that violates it (`super::constraint_names`).
/// * `[KEY|INDEX] <name> (<columns>)`, and the `FULLTEXT`/`SPATIAL` forms, declare an INDEX
///   and no constraint: MySQL's manual separates the two, and an index changes which rows a
///   query reads rather than which rows the table admits. The engine has no inline index
///   declaration, so the clause leaves the table definition and the table it creates holds
///   exactly the rows MySQL's would.
///
/// Every other clause — a column definition, `PRIMARY KEY`, `FOREIGN KEY`, `CHECK` — is
/// already written in a form the engine declares, and carries no entry here.
fn mysql_table_constraints(tokens: &[Token], close: usize) -> Vec<TableConstraintRewrite> {
    let mut rewrites = Vec::new();
    for (position, (clause, at)) in mysql_column_list_clauses(tokens, close)
        .into_iter()
        .enumerate()
    {
        let Some(declared) = table_constraint_declaration(tokens, &clause) else {
            continue;
        };
        let from = clause.first().copied().unwrap_or(at);
        // A clause that LEAVES the list first takes the separator after it with it, so
        // the list the engine reads has no empty entry where it was. Only a separator:
        // the parenthesis that closes the list is the list's own.
        let separates = matches!(tokens.get(at), Some(Token::Comma));
        let until = if declared.is_none() && position == 0 && separates {
            at + 1
        } else {
            at
        };
        rewrites.push(TableConstraintRewrite {
            from,
            until,
            declared,
        });
    }
    rewrites
}

/// The PRIMARY KEY clause of a MySQL `CREATE TABLE` that is over its AUTO_INCREMENT column
/// alone, where the client declared that key as a clause of its own rather than on the
/// column itself.
///
/// MySQL requires the two to go together — "there can be only one AUTO_INCREMENT column per
/// table, it must be indexed" — and lets the index be declared either way. The engine
/// declares the COUNTER behind that column's keys as part of the column's own primary key,
/// which is the only place its grammar takes it, so a key declared as a clause is declared
/// there instead and the clause leaves the list.
struct AutoIncrementKeyClause {
    /// The index of the clause's first significant token.
    from: usize,
    /// The index the rendering carries on at: the separator after the clause.
    until: usize,
}

impl AutoIncrementKeyClause {
    /// The clause as a rewrite that takes it OUT of the list, which is how the walk reads
    /// one.
    fn rewrite(&self) -> TableConstraintRewrite {
        TableConstraintRewrite {
            from: self.from,
            until: self.until,
            declared: None,
        }
    }
}

/// See [`AutoIncrementKeyClause`]; `None` where the statement declares no such clause.
fn mysql_auto_increment_key_clause(
    tokens: &[Token],
    close: usize,
) -> Option<AutoIncrementKeyClause> {
    let word_at = |at: usize| match tokens.get(at) {
        Some(Token::Word(word)) => Some(word.value.clone()),
        _ => None,
    };
    let clauses = mysql_column_list_clauses(tokens, close);
    // The column the keyword is on: the name the definition carrying it opens with.
    let mut column = None;
    for (clause, _) in &clauses {
        let carries = clause.iter().any(|at| {
            word_at(*at).is_some_and(|word| word.eq_ignore_ascii_case("auto_increment"))
        });
        if carries {
            column = clause.first().copied().and_then(word_at);
            break;
        }
    }
    let column = column?;
    // The clause that declares the primary key over that one column, and nothing else.
    for (clause, at) in &clauses {
        let words: Vec<Option<String>> = clause.iter().map(|at| word_at(*at)).collect();
        let named = |position: usize| words.get(position).cloned().flatten();
        // The clause may name the constraint, which the key is declared without.
        let position = if named(0).is_some_and(|word| word.eq_ignore_ascii_case("constraint")) {
            2
        } else {
            0
        };
        if !named(position).is_some_and(|word| word.eq_ignore_ascii_case("primary"))
            || !named(position + 1).is_some_and(|word| word.eq_ignore_ascii_case("key"))
        {
            continue;
        }
        let over: Vec<usize> = clause.get(position + 2..)?.to_vec();
        let [open, named_column, closing] = over.as_slice() else {
            continue;
        };
        if !matches!(tokens.get(*open), Some(Token::LParen))
            || !matches!(tokens.get(*closing), Some(Token::RParen))
            || !word_at(*named_column).is_some_and(|word| word.eq_ignore_ascii_case(&column))
        {
            continue;
        }
        return Some(AutoIncrementKeyClause {
            from: clause.first().copied().unwrap_or(*at),
            until: *at,
        });
    }
    None
}

/// Declare the engine's own COUNTER on the primary key of the column definition being
/// emitted, answering whether that definition carries one at all.
///
/// The engine takes `AUTOINCREMENT` as part of a column's `PRIMARY KEY`, so the keyword is
/// inserted right after it rather than appended: what it adds is that the counter never hands
/// out a key it has handed out before, which is what MySQL's own counter does — "the value of
/// an AUTO_INCREMENT column is not reused" after the row holding it is deleted.
fn declare_the_counter(out: &mut Vec<String>) -> bool {
    let from = out
        .iter()
        .rposition(|piece| {
            let piece = piece.trim();
            piece == "," || piece == "("
        })
        .map_or(0, |at| at + 1);
    let mut primary = false;
    for at in from..out.len() {
        let piece = out[at].trim();
        if piece.is_empty() {
            continue;
        }
        if piece.eq_ignore_ascii_case("key") && primary {
            out.insert(at + 1, " AUTOINCREMENT".to_string());
            return true;
        }
        primary = piece.eq_ignore_ascii_case("primary");
    }
    false
}

/// How the engine declares the table-constraint clause whose significant tokens are at
/// `clause`: `Some(Some(..))` for a clause it declares differently, `Some(None)` for one it
/// declares nothing for, and `None` for a clause that is already in its own grammar.
///
/// See [`mysql_table_constraints`] for which clause is which.
fn table_constraint_declaration(
    tokens: &[Token],
    clause: &[usize],
) -> Option<Option<String>> {
    let token_at = |position: usize| clause.get(position).and_then(|at| tokens.get(*at));
    let word_at = |position: usize| match token_at(position) {
        Some(Token::Word(word)) if word.quote_style.is_none() => {
            Some(word.value.to_ascii_uppercase())
        }
        _ => None,
    };
    let mut position = 0usize;
    // An explicitly named constraint: the name the client gave it, and the kind that
    // follows.
    let mut named: Option<String> = None;
    if word_at(0).as_deref() == Some("CONSTRAINT") {
        named = token_at(1).map(ToString::to_string);
        position = 2;
    }
    match word_at(position).as_deref() {
        Some("UNIQUE") => {}
        Some("KEY" | "INDEX" | "FULLTEXT" | "SPATIAL") => return Some(None),
        _ => return None,
    }
    position += 1;
    // The `KEY` or `INDEX` noun MySQL writes after `UNIQUE`, which the declaration does not
    // carry.
    if matches!(word_at(position).as_deref(), Some("KEY" | "INDEX")) {
        position += 1;
    }
    // The index's own name, where the clause names one before the column list.
    if let Some(Token::Word(word)) = token_at(position) {
        if named.is_none() {
            named = Some(word.to_string());
        }
        position += 1;
    }
    // The columns the constraint is over, exactly as the client wrote them.
    let columns: String = clause
        .get(position..)?
        .iter()
        .map(|at| tokens[*at].to_string())
        .collect();
    if !columns.starts_with('(') {
        return None;
    }
    Some(Some(match named {
        Some(named) => format!("CONSTRAINT {named} UNIQUE {columns}"),
        None => format!("UNIQUE {columns}"),
    }))
}

/// Drop the separator before the clause that has just left a `CREATE TABLE`'s column list,
/// so the list the engine reads has no empty entry where the clause was.
fn drop_the_separating_comma(out: &mut Vec<String>) {
    while out.last().is_some_and(|piece| piece.trim().is_empty()) {
        out.pop();
    }
    if out.last().map(String::as_str) == Some(",") {
        out.pop();
    }
}

/// The column types a MySQL `AUTO_INCREMENT` column may be declared with, and which the
/// engine needs declared as its own rowid type for the column to be assigned values.
///
/// MySQL's `AUTO_INCREMENT` column "generates a unique identity for new rows" and must be
/// an integer column. The engine assigns those values for the column it calls `INTEGER
/// PRIMARY KEY`, so a column declared with any of MySQL's integer widths is declared to it
/// as `INTEGER` — the same column, holding the same values, assigned the same way.
const MYSQL_INTEGER_TYPES: [&str; 6] = [
    "tinyint",
    "smallint",
    "mediumint",
    "int",
    "integer",
    "bigint",
];

/// Declare the column being defined with the engine's rowid type, by rewriting the most
/// recent integer type word that was emitted. A declaration with no integer type before the
/// keyword is left alone — the engine then refuses it, as MySQL does.
fn widen_to_rowid_type(out: &mut [String]) {
    for piece in out.iter_mut().rev() {
        if MYSQL_INTEGER_TYPES
            .iter()
            .any(|known| piece.eq_ignore_ascii_case(known))
        {
            *piece = "INTEGER".to_string();
            return;
        }
        // Only the column currently being declared: a comma or an opening parenthesis ends
        // the walk back, so one column's keyword never rewrites another's type.
        if piece == "," || piece == "(" {
            return;
        }
    }
}

/// How many tokens the CHARACTER SET or COLLATE clause at `at` spans, or `None` where no
/// such clause starts there.
///
/// MySQL writes a column's character set as `CHARACTER SET <name>` or `CHARSET <name>` and
/// its collation as `COLLATE <name>`, each optionally with an `=`; see the call site for why
/// the clause is not part of the declaration the engine is handed.
fn mysql_character_set_clause(tokens: &[Token], at: usize) -> Option<usize> {
    let word_at = |index: usize| match tokens.get(index) {
        Some(Token::Word(word)) if word.quote_style.is_none() => {
            Some(word.value.to_ascii_lowercase())
        }
        _ => None,
    };
    let mut cursor = match word_at(at)?.as_str() {
        // `CHARACTER SET <name>`: the noun is two words.
        "character" => {
            let set = next_significant(tokens, at + 1)
                .filter(|index| word_at(*index).as_deref() == Some("set"))?;
            set + 1
        }
        "charset" | "collate" => at + 1,
        _ => return None,
    };
    // The optional `=` a table or column option may be written with.
    if let Some(equals) = next_significant(tokens, cursor) {
        if matches!(tokens.get(equals), Some(Token::Eq)) {
            cursor = equals + 1;
        }
    }
    // The name itself, which the clause has to carry for this to be one.
    let named = next_significant(tokens, cursor)?;
    match tokens.get(named) {
        Some(Token::Word(_) | Token::SingleQuotedString(_) | Token::DoubleQuotedString(_)) => {
            Some(named + 1 - at)
        }
        _ => None,
    }
}

/// The name a MySQL system-variable reference carries, and how many tokens it spans.
///
/// The reference is written `@@name`, `@@scope.name` or `@@` followed by either, and the
/// scope (`GLOBAL`, `SESSION`, `LOCAL`) says WHERE the variable is read, not which variable
/// it is — so it is stripped, whether the tokenizer handed it over joined to the name or as
/// its own token followed by `.` and the name.
fn system_variable_name(tokens: &[Token], at: usize, suffix: &str) -> (String, usize) {
    let (mut name, mut consumed) = if suffix.is_empty() {
        (
            tokens
                .get(at + 1)
                .map(ToString::to_string)
                .unwrap_or_default(),
            2,
        )
    } else {
        (suffix.to_string(), 1)
    };
    for scope in ["global", "session", "local"] {
        if let Some(rest) = name.to_ascii_lowercase().strip_prefix(&format!("{scope}.")) {
            return (rest.to_string(), consumed);
        }
        if name.eq_ignore_ascii_case(scope) {
            // `@@session` `.` `name`: the scope word, the separator and the name.
            if matches!(tokens.get(at + consumed), Some(Token::Period)) {
                name = tokens
                    .get(at + consumed + 1)
                    .map(ToString::to_string)
                    .unwrap_or_default();
                consumed += 2;
            }
            return (name, consumed);
        }
    }
    (name, consumed)
}

/// The expression a `::` casts, taken back off what has already been emitted.
///
/// The operand of the cast operator is the PRIMARY expression to its left: a parenthesised
/// group (with the function name in front of it, where it is a call), or a single literal,
/// placeholder or possibly qualified identifier. Taking exactly that keeps the rest of the
/// statement — the operators and operands around it — as the client wrote it.
fn take_operand(out: &mut Vec<String>) -> String {
    while out.last().is_some_and(|piece| piece.trim().is_empty()) {
        out.pop();
    }
    let Some(last) = out.pop() else {
        return String::new();
    };
    let mut parts = vec![last.clone()];
    if last == ")" {
        let mut depth = 1usize;
        while depth > 0 {
            let Some(piece) = out.pop() else { break };
            if piece == ")" {
                depth += 1;
            } else if piece == "(" {
                depth -= 1;
            }
            parts.push(piece);
        }
        if out
            .last()
            .is_some_and(|piece| piece.chars().all(|c| c.is_alphanumeric() || c == '_'))
        {
            if let Some(name) = out.pop() {
                parts.push(name);
            }
        }
    } else {
        while out.last().map(String::as_str) == Some(".") {
            if let Some(separator) = out.pop() {
                parts.push(separator);
            }
            if let Some(qualifier) = out.pop() {
                parts.push(qualifier);
            }
        }
    }
    parts.reverse();
    parts.concat()
}

/// The type a `::` casts TO, as the engine spells it, and how many tokens it spans.
///
/// The type is read as the client wrote it — a name, optionally with the precision and scale
/// a numeric type carries — and then named in the engine's own type vocabulary
/// ([`engine_type`]).
fn cast_target(tokens: &[Token], from: usize) -> (String, usize) {
    let mut consumed = 0usize;
    let mut words: Vec<String> = Vec::new();
    let mut arguments = String::new();
    let mut at = from;
    while let Some(token) = tokens.get(at) {
        match token {
            Token::Whitespace(_) if words.is_empty() => {}
            Token::Word(word) => {
                // A multi-word type name (`double precision`, `timestamp with time zone`)
                // is read whole; a word that starts the rest of the expression is not part
                // of the type.
                if !words.is_empty() && !TYPE_CONTINUATIONS.contains(&word.value.to_ascii_lowercase().as_str()) {
                    break;
                }
                words.push(word.value.to_ascii_lowercase());
            }
            Token::LParen if !words.is_empty() && arguments.is_empty() => {
                let mut depth = 1usize;
                arguments.push('(');
                let mut inner = at + 1;
                while depth > 0 {
                    let Some(token) = tokens.get(inner) else { break };
                    match token {
                        Token::LParen => depth += 1,
                        Token::RParen => depth -= 1,
                        _ => {}
                    }
                    arguments.push_str(&token.to_string());
                    inner += 1;
                }
                consumed += inner - at - 1;
                at = inner - 1;
            }
            Token::Whitespace(_) => break,
            _ => break,
        }
        consumed += 1;
        at += 1;
    }
    let base = engine_type(&words.join(" "));
    (format!("{base}{arguments}"), consumed)
}

/// The words that CONTINUE a type name rather than starting the next expression.
const TYPE_CONTINUATIONS: [&str; 6] = ["precision", "with", "without", "time", "zone", "varying"];

/// The engine's type for the PostgreSQL type `named`.
///
/// Source: the PostgreSQL "Data Types" chapter's type names and aliases on one side, and the
/// engine's own storage classes on the other — the engine holds exact numbers, integers,
/// floating point, text and byte strings, so each PostgreSQL type is cast to the class that
/// holds its values. A type name this does not carry is cast to text, which is the form the
/// wire reports a value it has no narrower type for.
fn engine_type(named: &str) -> &'static str {
    match named {
        "numeric" | "decimal" | "money" => "NUMERIC",
        "smallint" | "int" | "integer" | "bigint" | "int2" | "int4" | "int8" | "oid" => "INTEGER",
        "real" | "float4" | "float8" | "double precision" | "double" => "REAL",
        "boolean" | "bool" => "NUMERIC",
        "bytea" => "BLOB",
        _ => "TEXT",
    }
}

/// The index of the next token that is not whitespace, at or after `from`.
pub fn next_significant(tokens: &[Token], from: usize) -> Option<usize> {
    tokens
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, token)| !matches!(token, Token::Whitespace(_)))
        .map(|(index, _)| index)
}

/// The value a session function returns, as a SQL literal, or `None` for a word that is
/// not one.
///
/// `called` says whether the word is followed by an argument list, which is what separates
/// the two kinds: the forms the SQL standard spells as KEYWORDS (`current_user`,
/// `session_user`) carry no parentheses, while the rest are functions and are substituted
/// only where they are CALLED — so a column or alias that happens to be named `version`
/// stays the column it is.
///
/// The functions this server runs in the ENGINE rather than substituting here
/// ([`super::pg_server::SERVER_FUNCTIONS`]) are deliberately absent: a statement that calls
/// one is planned and run like any other statement, which is what keeps its column name and
/// its type the ones PostgreSQL reports.
fn session_function(facts: &SessionFacts, lowered: &str, called: bool) -> Option<String> {
    let text = |value: &str| Some(format!("'{}'", value.replace('\'', "''")));
    if super::pg_server::is_server_function(lowered) {
        return None;
    }
    match lowered {
        // A MySQL session's identity is an ACCOUNT, `user@host`: `CURRENT_USER()` reports
        // the account whose privileges the session holds — for the RDS master user, the
        // `'<master>'@'%'` account RDS grants — and `USER()` reports the account name with
        // the host the client actually connected from, which is the endpoint's own host.
        //
        // `USER` and `SESSION_USER` are MySQL FUNCTIONS and not keywords of its grammar —
        // only `CURRENT_USER` may be written without parentheses there — so they are
        // substituted only where they are called. A bare `USER` is the identifier it is: the
        // noun of `CREATE USER`, and the `User` column of the server's own grant tables.
        "current_user" | "current_role" if facts.wire == EngineWire::MySql => {
            text(&facts.mysql_account())
        }
        "user" | "session_user" if facts.wire == EngineWire::MySql => {
            if !called {
                return None;
            }
            text(&format!("{}@{}", facts.user, facts.endpoint_host))
        }
        "current_user" | "session_user" | "user" | "current_role" => text(&facts.user),
        "current_catalog" => text(&facts.database),
        "current_schema" if !called => text(match facts.wire {
            EngineWire::Postgres => "public",
            EngineWire::MySql => &facts.database,
        }),
        _ if !called => None,
        "current_database" | "database" | "schema" => text(&facts.database),
        "current_schema" => text(match facts.wire {
            EngineWire::Postgres => "public",
            EngineWire::MySql => &facts.database,
        }),
        "connection_id" => Some(facts.connection_id.to_string()),
        "version" => text(&server_version_banner(facts)),
        _ => None,
    }
}

/// `value` as a SQL literal, or `NULL` where the session has no such value.
///
/// A system variable whose value is a WHOLE NUMBER is a numeric variable of the server
/// (`read_only`, `max_connections`), and both engines answer one as a number rather than as
/// text — which is the type the wire then reports for the column, and what a client reads it
/// back as.
///
/// A BOOLEAN system variable of a MySQL server is one of those: `SHOW VARIABLES` renders its
/// value as `ON` or `OFF`, and a `@@`-reference to the same variable answers 1 or 0, which is
/// what a typed driver decodes as an integer. The two readings are the same state written the
/// way each statement class writes it.
///
/// Every other value is text, quoted so the statement the engine runs carries it as one
/// literal whatever it contains.
fn quote_value(value: Option<String>) -> String {
    match value {
        Some(value) if value.trim().parse::<i64>().is_ok() => value.trim().to_string(),
        Some(value) if value.trim().eq_ignore_ascii_case("on") => "1".to_string(),
        Some(value) if value.trim().eq_ignore_ascii_case("off") => "0".to_string(),
        Some(value) => format!("'{}'", value.replace('\'', "''")),
        None => "NULL".to_string(),
    }
}
