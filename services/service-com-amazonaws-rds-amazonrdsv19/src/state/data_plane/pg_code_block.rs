// SPDX-License-Identifier: CC0-1.0

//! The anonymous CODE BLOCK a PostgreSQL client sends as a statement — `DO $$ … $$` — and
//! the server MESSAGES its `RAISE` statements produce.
//!
//! `DO` "executes an anonymous code block ... as if it were a function body returning void",
//! and the one thing a block does that no query can is talk to the server's own message
//! channels: `RAISE <level> '<format>'` writes a message at that level, which the engine
//! then writes to its log, sends to the client, or both, exactly as the two message floors
//! say ([`super::log_export::Severity`]). That is how a client makes the engine log a line —
//! the step a log export is observed through — so a server that refuses `DO` outright has no
//! engine log at all.
//!
//! What this module serves, and what it does not:
//!
//! * the block's own frame: an optional `LANGUAGE plpgsql`, the dollar-quoted (or
//!   single-quoted) body, and the `BEGIN … END` the body's statements sit in;
//! * `RAISE [level] '<format>' [, expression …] [USING option = expression …]`, with the
//!   format's `%` placeholders filled by the expressions — each of them EVALUATED BY THE
//!   ENGINE, as a `SELECT` of that expression, so nothing here computes a value itself;
//! * `RAISE [level] SQLSTATE '<code>' [USING …]`, which raises under the code the statement
//!   named — the client's own five-character SQLSTATE, carried through to the wire;
//! * every other statement of the body is a SQL statement, run by the engine exactly as it
//!   would be outside the block, in the session's own transaction;
//! * a `RAISE` at the `EXCEPTION` level — the level a `RAISE` with no level named is —
//!   ABORTS the block, and the client is answered with that error under `P0001`
//!   (`raise_exception`) or the SQLSTATE the statement named, which is what makes an
//!   exception observably different from a logged message.
//!
//! PL/pgSQL's procedural half is NOT served and says so: a block that declares variables or
//! carries control flow (`IF`, `LOOP`, `FOR`, `WHILE`, `CASE`, an assignment, an exception
//! handler) is refused as a capability this endpoint's engine does not have, with the
//! statement that was not served named — never run as something else and never silently
//! skipped. The same holds for a `RAISE` that names one of PostgreSQL's CONDITION NAMES,
//! whose catalog this endpoint does not carry.
//!
//! Everything one client statement makes this endpoint run is bounded by ONE budget
//! ([`BlockBudget`]), charged per statement and per message at every nesting level: the
//! per-body and per-depth limits would otherwise compose, so a few kilobytes of nested blocks
//! could buy the process millions of executions.

use sqlparser::tokenizer::Token;

use super::log_export::{LogLine, Severity};
use super::sql_engine::{Cell, EngineError, EngineErrorKind, InstanceDatabase, ResultSet};
use super::wire_session::{
    self, SessionFacts, StatementAnswer, WireNotice,
};
use super::EngineWire;

/// The SQLSTATE a `RAISE EXCEPTION` that names none raises under: `P0001`
/// (`raise_exception`), the code PostgreSQL defines for a user-raised exception.
pub const RAISE_EXCEPTION_SQLSTATE: &str = "P0001";

/// The SQLSTATE a logged message carries, which is the successful-completion class: a
/// message is not a failure.
pub const SUCCESSFUL_COMPLETION_SQLSTATE: &str = "00000";

/// The only procedural language this endpoint runs a block in.
const PLPGSQL: &str = "plpgsql";

/// How many statements one block's body may carry, so a body is bounded before it is built.
pub const MAX_BLOCK_STATEMENTS: usize = 64;

/// How deep blocks may nest: a block's body may itself carry a `DO`, and this is where that
/// stops.
pub const MAX_BLOCK_DEPTH: u8 = 4;

/// How many statements ONE statement a client sent may run in total, across the blocks nested
/// in it and the expressions their messages are filled from.
///
/// The per-body and per-depth bounds above would COMPOSE if they were the only ones — a body
/// of blocks, each of bodies of blocks, multiplies — so the bound that actually holds is this
/// one total, charged per statement run at any depth ([`BlockBudget::take_statement`]).
pub const MAX_STATEMENTS_PER_REQUEST: u32 = 256;

/// How many bytes of MESSAGES one statement a client sent may accumulate, across the same
/// nesting.
///
/// Every `RAISE` appends a message the client supplied to the answer, so this is what keeps a
/// block from growing the process: past it the statement is refused rather than served
/// ([`BlockBudget::take_message`]).
pub const MAX_MESSAGE_BYTES_PER_REQUEST: usize = 32 * 1024;

/// What one message costs the budget beyond its own bytes: the record that carries it on each
/// channel, so a flood of EMPTY messages is bounded by the count rather than by their length.
const MESSAGE_OVERHEAD_BYTES: usize = 64;

/// What ONE statement a client sent may still spend: how deep its blocks are nested, how many
/// statements they may still run, and how many bytes of messages they may still accumulate.
///
/// Threaded through [`wire_session::run_statement_at`] rather than kept per block, because the
/// work a client can buy is the TOTAL across the nesting and not the per-level limit.
#[derive(Debug)]
pub struct BlockBudget {
    /// How many blocks the statement being served is already inside.
    depth: u8,
    /// How many statements the budget still allows.
    statements: u32,
    /// How many bytes of messages it still allows.
    message_bytes: usize,
}

impl Default for BlockBudget {
    fn default() -> Self {
        Self {
            depth: 0,
            statements: MAX_STATEMENTS_PER_REQUEST,
            message_bytes: MAX_MESSAGE_BYTES_PER_REQUEST,
        }
    }
}

impl BlockBudget {
    /// Charge one statement, or report the budget as spent.
    pub(super) fn take_statement(&mut self) -> Result<(), EngineError> {
        match self.statements.checked_sub(1) {
            Some(left) => {
                self.statements = left;
                Ok(())
            }
            None => Err(EngineError::new(
                EngineErrorKind::LimitExceeded,
                format!(
                    "one statement may run at most {MAX_STATEMENTS_PER_REQUEST} statements in \
                     this emulator, code blocks included"
                ),
            )),
        }
    }

    /// Charge one message of `bytes`, or report the budget as spent.
    fn take_message(&mut self, bytes: usize) -> Result<(), EngineError> {
        match self
            .message_bytes
            .checked_sub(bytes.saturating_add(MESSAGE_OVERHEAD_BYTES))
        {
            Some(left) => {
                self.message_bytes = left;
                Ok(())
            }
            None => Err(EngineError::new(
                EngineErrorKind::LimitExceeded,
                format!(
                    "one statement may raise at most {MAX_MESSAGE_BYTES_PER_REQUEST} bytes of \
                     messages in this emulator"
                ),
            )),
        }
    }

    /// Enter one more block, or report the nesting as too deep.
    fn enter(&mut self) -> Result<(), EngineError> {
        if self.depth >= MAX_BLOCK_DEPTH {
            return Err(EngineError::new(
                EngineErrorKind::LimitExceeded,
                format!("this endpoint runs code blocks nested at most {MAX_BLOCK_DEPTH} deep"),
            ));
        }
        self.depth += 1;
        Ok(())
    }

    /// Leave the block [`Self::enter`] entered.
    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

/// Serve `DO <body>` for a PostgreSQL session, or `None` for a statement that is not one.
///
/// `tokens` are the statement's SIGNIFICANT tokens and `block` is what the client's statement
/// may still spend ([`BlockBudget`]).
pub fn code_block(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    words: &[String],
    tokens: &[Token],
    block: &mut BlockBudget,
) -> Result<Option<StatementAnswer>, EngineError> {
    if facts.wire != EngineWire::Postgres || words.first().map(String::as_str) != Some("DO") {
        return Ok(None);
    }
    block.enter()?;
    let served = served_block(database, facts, tokens, block);
    block.leave();
    served.map(Some)
}

/// Run one block's body, with its own nesting level already charged to `block`.
fn served_block(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
    block: &mut BlockBudget,
) -> Result<StatementAnswer, EngineError> {
    let frame = block_frame(tokens)?;
    if let Some(language) = frame.language {
        if !language.eq_ignore_ascii_case(PLPGSQL) {
            // PostgreSQL's own sentence for a language with no inline handler.
            return Err(EngineError::new(
                EngineErrorKind::FeatureNotSupported,
                format!("language \"{language}\" does not support inline code execution"),
            ));
        }
    }
    let mut answer = StatementAnswer::new("DO", ResultSet::changed(0, 0));
    for statement in body_statements(&frame.body)? {
        if let Err(error) = run_body_statement(database, facts, &statement, block, &mut answer) {
            // What the block said BEFORE it failed travels with the refusal: the messages are
            // the server's, and the statement they belong to is the one that failed.
            return Err(error.with_messages(answer.notices, answer.log_lines));
        }
    }
    Ok(answer)
}

/// The frame of a `DO` statement: the language it names, where it names one, and the body
/// between its quotes.
struct BlockFrame {
    language: Option<String>,
    body: String,
}

/// Read the frame above off the statement's tokens.
///
/// `DO` takes the body and an optional `LANGUAGE <name>` in either order, which is what the
/// two passes here allow; a statement that carries no body at all ended before it said what
/// to run.
fn block_frame(tokens: &[Token]) -> Result<BlockFrame, EngineError> {
    let mut language = None;
    let mut body = None;
    let mut naming_language = false;
    for token in tokens.iter().skip(1) {
        match token {
            Token::Word(word) => {
                if word.value.eq_ignore_ascii_case("LANGUAGE") {
                    naming_language = true;
                    continue;
                }
                if naming_language {
                    language = Some(word.value.clone());
                    naming_language = false;
                    continue;
                }
                return Err(syntax_error_at(&word.value));
            }
            Token::DollarQuotedString(quoted) => {
                if naming_language {
                    language = Some(quoted.value.clone());
                    naming_language = false;
                    continue;
                }
                body = Some(quoted.value.clone());
            }
            Token::SingleQuotedString(text) => {
                if naming_language {
                    language = Some(text.clone());
                    naming_language = false;
                    continue;
                }
                body = Some(text.clone());
            }
            Token::SemiColon | Token::Whitespace(_) => {}
            other => return Err(syntax_error_at(&other.to_string())),
        }
    }
    match body {
        Some(body) => Ok(BlockFrame { language, body }),
        None => Err(EngineError::new(
            EngineErrorKind::Syntax,
            "syntax error at end of input",
        )),
    }
}

/// The statements a block's body carries, in order, with the `BEGIN`/`END` that frames them
/// taken off.
///
/// Split on the body's own TOKENS at the top level, so a semicolon inside a string literal
/// or a parenthesised list does not end a statement, and each statement is handed on as the
/// client wrote it (whitespace included), which is what the engine is given.
fn body_statements(body: &str) -> Result<Vec<String>, EngineError> {
    let tokens = wire_session::statement_tokens(EngineWire::Postgres, body)?;
    let mut statements: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for token in tokens {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => depth = depth.saturating_sub(1),
            Token::SemiColon if depth == 0 => {
                if !current.trim().is_empty() {
                    statements.push(current.trim().to_string());
                }
                current = String::new();
                continue;
            }
            _ => {}
        }
        current.push_str(&token.to_string());
    }
    if !current.trim().is_empty() {
        statements.push(current.trim().to_string());
    }
    // The block's frame: `BEGIN` opens the body's statement list and `END` closes it, and
    // the first statement carries the `BEGIN` because no semicolon follows that keyword.
    if let Some(first) = statements.first_mut() {
        if let Some(rest) = strip_leading_word(first, "BEGIN") {
            *first = rest;
        }
    }
    statements.retain(|statement| !statement.trim().is_empty());
    if statements
        .last()
        .is_some_and(|last| last.trim().eq_ignore_ascii_case("END"))
    {
        statements.pop();
    }
    if statements.len() > MAX_BLOCK_STATEMENTS {
        return Err(EngineError::new(
            EngineErrorKind::LimitExceeded,
            format!("one code block may carry at most {MAX_BLOCK_STATEMENTS} statements in this emulator"),
        ));
    }
    Ok(statements)
}

/// `statement` without its leading `word`, or `None` when it does not start with that word.
fn strip_leading_word(statement: &str, word: &str) -> Option<String> {
    let trimmed = statement.trim_start();
    let rest = trimmed.get(..word.len()).filter(|lead| lead.eq_ignore_ascii_case(word))
        .and_then(|_| trimmed.get(word.len()..))?;
    if rest
        .chars()
        .next()
        .is_some_and(|next| next.is_alphanumeric() || next == '_')
    {
        return None;
    }
    Some(rest.trim().to_string())
}

/// Run ONE statement of a block's body: a `RAISE`, or a SQL statement the engine runs.
fn run_body_statement(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    statement: &str,
    block: &mut BlockBudget,
    answer: &mut StatementAnswer,
) -> Result<(), EngineError> {
    let tokens = wire_session::significant_tokens(EngineWire::Postgres, statement)?;
    let leading = tokens
        .first()
        .and_then(|token| match token {
            Token::Word(word) => Some(word.value.to_ascii_uppercase()),
            _ => None,
        })
        .unwrap_or_default();
    if leading == "RAISE" {
        return raise(database, facts, &tokens, block, answer);
    }
    if let Some(refusal) = procedural_refusal(&leading, &tokens) {
        return Err(refusal);
    }
    // Everything else is SQL, run by the engine in this session exactly as it would be
    // outside the block — and charged to the same budget, which is what bounds a body of
    // blocks. Its rows are not the block's: `DO` returns void, so what is kept is what the
    // server said about the statement.
    let ran = wire_session::run_statement_at(database, facts, statement, &[], block)?;
    answer.notices.extend(ran.notices);
    answer.log_lines.extend(ran.log_lines);
    answer.databases_changed |= ran.databases_changed;
    Ok(())
}

/// The refusal for a statement of PL/pgSQL's PROCEDURAL half, which this endpoint's engine
/// does not run, or `None` for a statement that is SQL.
///
/// Named one by one rather than guessed at, so a statement that IS SQL is never refused as
/// procedural: these are the keywords PL/pgSQL's own statement list defines, plus an
/// assignment to a variable, which is the one form that is not introduced by a keyword.
fn procedural_refusal(leading: &str, tokens: &[Token]) -> Option<EngineError> {
    const PROCEDURAL: [&str; 17] = [
        "DECLARE", "IF", "CASE", "LOOP", "WHILE", "FOR", "FOREACH", "EXIT", "CONTINUE",
        "RETURN", "PERFORM", "EXECUTE", "GET", "OPEN", "FETCH", "ASSERT", "EXCEPTION",
    ];
    let assignment = matches!(tokens.get(1), Some(Token::Assignment))
        || matches!(
            (tokens.get(1), tokens.get(2)),
            (Some(Token::Colon), Some(Token::Eq))
        );
    if !PROCEDURAL.contains(&leading) && !assignment {
        return None;
    }
    let named = match assignment {
        true => "an assignment".to_string(),
        false => format!("`{leading}`"),
    };
    Some(EngineError::new(
        EngineErrorKind::FeatureNotSupported,
        format!(
            "this endpoint runs the SQL statements and the RAISE statements of a code block; \
             {named} is not served"
        ),
    ))
}

/// Serve one `RAISE`: record what the server said, and abort the block when the level is
/// `EXCEPTION`.
fn raise(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
    block: &mut BlockBudget,
    answer: &mut StatementAnswer,
) -> Result<(), EngineError> {
    // A `RAISE` is a statement of the body like any other, so it is charged the same.
    block.take_statement()?;
    let raised = read_raise(database, facts, tokens, block)?;
    // What this message costs: charged once, whichever channels carry it, so a block cannot
    // grow the process by raising without end.
    block.take_message(raised.message.len())?;
    // The EXCEPTION level is a failure of the statement, which is what the client is told;
    // the log line the engine writes for it is the one the refusal itself produces, so it
    // is not recorded twice.
    if raised.severity == Severity::Error {
        return Err(EngineError::new(EngineErrorKind::DataError, raised.message)
            .with_sqlstate(raised.sqlstate));
    }
    if raised.severity.written_to_the_server_log(facts) {
        answer
            .log_lines
            .push(LogLine::new(raised.severity, raised.message.clone()));
    }
    if raised.severity.reported_to_the_client(facts) {
        answer.notices.push(WireNotice::at(
            raised.severity.label(),
            raised.sqlstate,
            raised.message,
        ));
    }
    Ok(())
}

/// What one `RAISE` says: at which level, under which SQLSTATE, with which message.
struct Raised {
    severity: Severity,
    /// The five-character SQLSTATE the message carries: the raise default for the level, or
    /// the code the statement itself named ([`sqlstate_named`]).
    sqlstate: ::std::borrow::Cow<'static, str>,
    message: String,
}

/// Read a `RAISE` statement, evaluating the expressions its format is filled from.
fn read_raise(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
    block: &mut BlockBudget,
) -> Result<Raised, EngineError> {
    let mut at = 1usize;
    if tokens.len() == 1 {
        // PostgreSQL's own sentence for a bare `RAISE`, which re-raises the exception being
        // handled — and there is none outside a handler.
        return Err(EngineError::new(
            EngineErrorKind::Syntax,
            "RAISE without parameters cannot be used outside an exception handler",
        ));
    }
    let severity = match tokens.get(at) {
        Some(Token::Word(word)) => match raise_level(&word.value) {
            Some(level) => {
                at += 1;
                level
            }
            // No level named: "The level option specifies the error severity. ... If level
            // is omitted, EXCEPTION is used."
            None => Severity::Error,
        },
        _ => Severity::Error,
    };
    let mut sqlstate: ::std::borrow::Cow<'static, str> = match severity {
        Severity::Error => ::std::borrow::Cow::Borrowed(RAISE_EXCEPTION_SQLSTATE),
        _ => ::std::borrow::Cow::Borrowed(SUCCESSFUL_COMPLETION_SQLSTATE),
    };
    let mut message: Option<String> = None;
    match tokens.get(at) {
        // `RAISE <level> '<format>' [, expression …]`.
        Some(Token::SingleQuotedString(format)) => {
            at += 1;
            let arguments = format_arguments(database, facts, tokens, &mut at, block)?;
            message = Some(formatted(format, &arguments));
        }
        // `RAISE <level> SQLSTATE '<code>'`.
        Some(Token::Word(word)) if word.value.eq_ignore_ascii_case("SQLSTATE") => {
            at += 1;
            let Some(Token::SingleQuotedString(code)) = tokens.get(at) else {
                return Err(EngineError::new(
                    EngineErrorKind::Syntax,
                    "syntax error at end of input",
                ));
            };
            at += 1;
            sqlstate = sqlstate_named(code)?;
            // With no message of its own, the condition the statement named IS the message,
            // which for this form is the code it raises under.
            message = Some(code.clone());
        }
        // `RAISE <level> USING MESSAGE = …`, whose message comes from the options below.
        Some(Token::Word(word)) if word.value.eq_ignore_ascii_case("USING") => {}
        Some(Token::Word(word)) => return Err(condition_name_refusal(&word.value)),
        None => {}
        Some(other) => return Err(syntax_error_at(&other.to_string())),
    }
    // `USING option = expression [, …]`: the two options that decide what the client is
    // told, read whichever order they come in.
    if matches!(tokens.get(at), Some(Token::Word(word)) if word.value.eq_ignore_ascii_case("USING"))
    {
        at += 1;
        for (option, value) in using_options(database, facts, tokens, &mut at, block)? {
            match option.as_str() {
                "message" => message = Some(value),
                "errcode" => sqlstate = sqlstate_named(&value)?,
                // `DETAIL`, `HINT`, `COLUMN`, `TABLE` and the rest are further FIELDS of the
                // error; this endpoint carries the message and the SQLSTATE, so they are
                // read and left out rather than folded into the sentence.
                _ => {}
            }
        }
    }
    Ok(Raised {
        severity,
        sqlstate,
        message: message.unwrap_or_default(),
    })
}

/// The level `word` names, or `None` for a word that is not one.
///
/// The six levels `RAISE` takes, where `EXCEPTION` is the one that aborts the block and is
/// reported as an `ERROR`.
fn raise_level(word: &str) -> Option<Severity> {
    Some(match word.to_ascii_uppercase().as_str() {
        "DEBUG" => Severity::Debug,
        "LOG" => Severity::Log,
        "INFO" => Severity::Info,
        "NOTICE" => Severity::Notice,
        "WARNING" => Severity::Warning,
        "EXCEPTION" => Severity::Error,
        _ => return None,
    })
}

/// The SQLSTATE the statement NAMED, as the wire reports it.
///
/// `ERRCODE` is "either a SQLSTATE code or a condition name", and the SQLSTATE form is what
/// this endpoint raises under: a five-character code of digits and upper-case letters, which
/// is exactly what the protocol's `C` field carries and what a client branches on, so the
/// client's own code is carried through rather than replaced by one of this server's.
///
/// A value of any other shape is a CONDITION NAME, which is refused
/// ([`condition_name_refusal`]) for the reason that refusal gives.
fn sqlstate_named(value: &str) -> Result<::std::borrow::Cow<'static, str>, EngineError> {
    /// The width of a SQLSTATE: "a five-character error code".
    const SQLSTATE_CHARS: usize = 5;
    let code = value.trim().to_ascii_uppercase();
    if code.chars().count() == SQLSTATE_CHARS
        && code
            .chars()
            .all(|character| character.is_ascii_digit() || character.is_ascii_uppercase())
    {
        return Ok(::std::borrow::Cow::Owned(code));
    }
    Err(condition_name_refusal(value))
}

/// The refusal for a `RAISE` that names a CONDITION rather than a message or a SQLSTATE,
/// wherever it names it (after the level, or as its `ERRCODE`).
///
/// The condition names are entries of PostgreSQL's own error-code table, which this endpoint
/// does not carry: raising under a code derived from the name would be a code this server
/// invented, so the statement is refused saying which capability is missing instead.
fn condition_name_refusal(name: &str) -> EngineError {
    EngineError::new(
        EngineErrorKind::FeatureNotSupported,
        format!(
            "this endpoint raises a message or a SQLSTATE; the condition name \"{name}\" is \
             not served"
        ),
    )
}

/// The expressions a format's placeholders are filled from: everything after the format
/// string, comma separated, up to `USING` or the end of the statement.
///
/// Each one is EVALUATED BY THE ENGINE — handed to it as `SELECT <expression>` — so a
/// placeholder filled from a column, a function or an operator carries the engine's own
/// answer and nothing here computes a value.
fn format_arguments(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
    at: &mut usize,
    block: &mut BlockBudget,
) -> Result<Vec<String>, EngineError> {
    let mut arguments = Vec::new();
    while matches!(tokens.get(*at), Some(Token::Comma)) {
        *at += 1;
        let expression = expression_text(tokens, at);
        if expression.trim().is_empty() {
            return Err(EngineError::new(
                EngineErrorKind::Syntax,
                "syntax error at end of input",
            ));
        }
        arguments.push(evaluated(database, facts, &expression, block)?);
    }
    Ok(arguments)
}

/// The `USING option = expression` pairs, each option lowercased and each value evaluated by
/// the engine.
fn using_options(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
    at: &mut usize,
    block: &mut BlockBudget,
) -> Result<Vec<(String, String)>, EngineError> {
    let mut options = Vec::new();
    while let Some(Token::Word(option)) = tokens.get(*at) {
        *at += 1;
        if !matches!(tokens.get(*at), Some(Token::Eq)) {
            return Err(syntax_error_at(&option.value));
        }
        *at += 1;
        let expression = expression_text(tokens, at);
        if expression.trim().is_empty() {
            return Err(EngineError::new(
                EngineErrorKind::Syntax,
                "syntax error at end of input",
            ));
        }
        options.push((
            option.value.to_ascii_lowercase(),
            evaluated(database, facts, &expression, block)?,
        ));
        if !matches!(tokens.get(*at), Some(Token::Comma)) {
            break;
        }
        *at += 1;
    }
    Ok(options)
}

/// One expression of a `RAISE`, as the client wrote it: the tokens up to the next top-level
/// comma or the end of the statement.
fn expression_text(tokens: &[Token], at: &mut usize) -> String {
    let mut text = String::new();
    let mut depth = 0usize;
    while let Some(token) = tokens.get(*at) {
        match token {
            Token::LParen => depth += 1,
            Token::RParen => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            Token::Comma if depth == 0 => break,
            Token::SemiColon => break,
            _ => {}
        }
        text.push(' ');
        text.push_str(&token.to_string());
        *at += 1;
    }
    text.trim().to_string()
}

/// The VALUE of one expression, as the engine computes it: a one-row `SELECT` of it, whose
/// single cell is rendered the way the wire renders that cell.
fn evaluated(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    expression: &str,
    block: &mut BlockBudget,
) -> Result<String, EngineError> {
    let answer = wire_session::run_statement_at(
        database,
        facts,
        &format!("SELECT {expression}"),
        &[],
        block,
    )?;
    let cell = answer
        .result
        .rows
        .first()
        .and_then(|row| row.first())
        .cloned()
        .unwrap_or(Cell::Null);
    Ok(cell_text(&cell))
}

/// One evaluated cell as a message carries it: the value's own text, and `<NULL>` for a NULL
/// — which is how PostgreSQL renders a NULL argument of a `RAISE` format.
fn cell_text(cell: &Cell) -> String {
    match cell {
        Cell::Null => "<NULL>".to_string(),
        Cell::Integer(number) => number.to_string(),
        Cell::Real(number) => number.to_string(),
        Cell::Text(text) => text.clone(),
        Cell::Blob(bytes) => ::hex::encode(bytes),
    }
}

/// The message a format and its arguments come to: "The level option ... Inside the format
/// string, % is replaced by the next optional argument's string representation. Write %% to
/// emit a literal %."
fn formatted(format: &str, arguments: &[String]) -> String {
    let mut message = String::with_capacity(format.len());
    let mut filled = arguments.iter();
    let mut characters = format.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '%' {
            message.push(character);
            continue;
        }
        match characters.peek() {
            Some('%') => {
                characters.next();
                message.push('%');
            }
            // A placeholder with no argument left is PostgreSQL's "too few arguments for
            // format"; the message is still the one the client asked for, so the placeholder
            // is left as it was written rather than filled from nothing.
            _ => match filled.next() {
                Some(argument) => message.push_str(argument),
                None => message.push('%'),
            },
        }
    }
    message
}

/// PostgreSQL's parse error for a token its grammar cannot read here.
fn syntax_error_at(token: &str) -> EngineError {
    EngineError::new(
        EngineErrorKind::Syntax,
        format!("syntax error at or near \"{token}\""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the SQLSTATE a `RAISE` names is the code the message carries, carried through as
    /// the client wrote it; a value that is not a five-character code is a condition name,
    /// which this endpoint does not serve. The rows span the two cases either side of the
    /// code's own shape: its width, its character set, and the surrounding space a client may
    /// write.
    #[test]
    fn the_sqlstate_a_raise_names_is_the_one_it_is_reported_under() {
        let rows: [(&str, Option<&str>); 8] = [
            ("22012", Some("22012")),
            // The code every client writes for a user-raised exception, in either case.
            ("p0001", Some("P0001")),
            (" 23503 ", Some("23503")),
            ("0A000", Some("0A000")),
            // A condition NAME, which is the other form `ERRCODE` takes.
            ("division_by_zero", None),
            ("2201", None),
            ("22012X", None),
            ("", None),
        ];
        for (value, expected) in rows {
            let named = sqlstate_named(value);
            let carried = named.as_ref().ok().map(|code| code.as_ref());
            assert_eq!(
                carried, expected,
                "case {value:?}: expected {expected:?}, got {carried:?}"
            );
        }
    }

    /// RULE: the work one client statement may buy is bounded by ONE budget, whatever it
    /// nests: past the nesting depth, the statement count or the message bytes the statement
    /// is refused as over the limit.
    #[test]
    fn one_statements_budget_bounds_every_level_of_its_blocks() {
        let mut budget = BlockBudget::default();
        for level in 0..MAX_BLOCK_DEPTH {
            assert!(
                budget.enter().is_ok(),
                "case {:?}: a block at depth {level} is within the nesting bound",
                "the nesting bound"
            );
        }
        assert!(
            budget.enter().is_err(),
            "case {:?}: a block past the nesting bound is refused",
            "the nesting bound"
        );

        let mut budget = BlockBudget::default();
        for taken in 0..MAX_STATEMENTS_PER_REQUEST {
            assert!(
                budget.take_statement().is_ok(),
                "case {:?}: statement {taken} is within the budget",
                "the statement budget"
            );
        }
        assert!(
            budget.take_statement().is_err(),
            "case {:?}: the statement past the budget is refused",
            "the statement budget"
        );

        let mut budget = BlockBudget::default();
        // A message of the WHOLE budget is already past it: each one costs its own bytes plus
        // the record that carries it, so a flood of empty messages is bounded too.
        assert!(
            budget.take_message(MAX_MESSAGE_BYTES_PER_REQUEST).is_err(),
            "case {:?}: a message of the whole budget is past it",
            "the message budget"
        );
        let mut budget = BlockBudget::default();
        let one = MAX_MESSAGE_BYTES_PER_REQUEST / 4;
        for taken in 0..3 {
            assert!(
                budget.take_message(one).is_ok(),
                "case {:?}: message {taken} of a quarter of the budget is within it",
                "the message budget"
            );
        }
        assert!(
            budget.take_message(one).is_err(),
            "case {:?}: the message past the budget is refused",
            "the message budget"
        );
    }

    /// RULE: a format's `%` placeholders are filled from the arguments in order and `%%` is
    /// a literal `%`. The rows span no placeholder, one, several, an escaped one and a
    /// placeholder with no argument left.
    #[test]
    fn a_format_is_filled_from_its_arguments_in_order() {
        let rows: [(&str, &str, &[&str], &str); 5] = [
            ("no placeholder", "all is well", &[], "all is well"),
            ("one placeholder", "row % is gone", &["7"], "row 7 is gone"),
            (
                "several placeholders",
                "% of % failed",
                &["3", "9"],
                "3 of 9 failed",
            ),
            ("an escaped percent", "100%% done", &[], "100% done"),
            ("too few arguments", "row % of %", &["1"], "row 1 of %"),
        ];
        for (case, format, arguments, expected) in rows {
            let filled: Vec<String> = arguments.iter().map(|text| (*text).to_string()).collect();
            let rendered = formatted(format, &filled);
            assert_eq!(
                rendered, expected,
                "case {case:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }

    /// RULE: the level a `RAISE` names decides the severity, and a `RAISE` that names none
    /// is an `EXCEPTION`.
    #[test]
    fn each_raise_level_names_its_own_severity() {
        for (word, expected) in [
            ("debug", Some(Severity::Debug)),
            ("LOG", Some(Severity::Log)),
            ("info", Some(Severity::Info)),
            ("notice", Some(Severity::Notice)),
            ("Warning", Some(Severity::Warning)),
            ("exception", Some(Severity::Error)),
            ("sqlstate", None),
        ] {
            let level = raise_level(word);
            assert_eq!(
                level, expected,
                "case {word:?}: expected {expected:?}, got {level:?}"
            );
        }
    }

    /// RULE: the body's statements are the ones between its `BEGIN` and `END`, split at the
    /// top level only. The rows span a bare body, a framed one, several statements, and a
    /// semicolon inside a literal.
    #[test]
    fn a_blocks_body_carries_the_statements_between_begin_and_end() {
        let rows: [(&str, &str, &[&str]); 4] = [
            ("a bare statement", " RAISE LOG 'up' ", &["RAISE LOG 'up'"]),
            (
                "a framed body",
                " BEGIN RAISE LOG 'up'; END ",
                &["RAISE LOG 'up'"],
            ),
            (
                "two statements",
                "BEGIN RAISE LOG 'one'; RAISE WARNING 'two'; END",
                &["RAISE LOG 'one'", "RAISE WARNING 'two'"],
            ),
            (
                "a semicolon inside a literal",
                "BEGIN RAISE LOG 'a;b'; END",
                &["RAISE LOG 'a;b'"],
            ),
        ];
        for (case, body, expected) in rows {
            let statements = body_statements(body)
                .unwrap_or_else(|error| panic!("case {case:?}: expected a body, got {error:?}"));
            assert_eq!(
                statements, expected,
                "case {case:?}: expected {expected:?}, got {statements:?}"
            );
        }
    }

    /// RULE: PL/pgSQL's procedural statements are refused as a capability this endpoint does
    /// not have, and a SQL statement is not.
    #[test]
    fn the_procedural_statements_are_refused_and_sql_is_not() {
        for (statement, procedural) in [
            ("IF", true),
            ("LOOP", true),
            ("DECLARE", true),
            ("PERFORM", true),
            ("INSERT", false),
            ("SELECT", false),
            ("CREATE", false),
        ] {
            let refusal = procedural_refusal(statement, &[]);
            assert_eq!(
                refusal.is_some(),
                procedural,
                "case {statement:?}: expected procedural={procedural}, got {refusal:?}"
            );
        }
    }
}
