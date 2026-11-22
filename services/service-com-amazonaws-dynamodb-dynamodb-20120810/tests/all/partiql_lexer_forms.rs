// SPDX-License-Identifier: CC0-1.0

//! What the statement LEXER reads, before any clause of the grammar is parsed: the forms a
//! numeric literal is written in, the comments a statement may carry, how a quoted run
//! spells the quote character itself, and which text is no lexeme at all.
//!
//! RULES under test, each one level above the case that found it:
//!
//! 1. A numeric literal is digits with an OPTIONAL fraction and an OPTIONAL decimal
//!    exponent, and the digits before the point may be left out altogether — so `1e5`,
//!    `1E-3` and `.5` each denote the number they spell, stored and reported in the one
//!    canonical text every reading of that number carries.
//! 2. A comment is TEXT the parse never sees: `--` runs to the end of its line and
//!    `/* … */` to the delimiter that closes it, so a statement carrying either runs as the
//!    statement around it — whatever verb that statement states — while the same characters
//!    inside a quoted run are part of the string, not a comment.
//! 3. A quoted run spells its own delimiter by DOUBLING it, for both the `'string'` literal
//!    and the `"name"` identifier.
//! 4. Text the scanner cannot read at all — a run it never closes, a numeric lexeme it
//!    cannot finish, a character the language spells nothing with — never became tokens, so
//!    there is no grammar position to name and the refusal states the fixed sentence with no
//!    phrase behind it. A `.` that opens no number is a TOKEN of the language (the step
//!    separator), so text that writes one where a value belongs stops at that token and is
//!    refused as the term it is.
//!
//! The rows differ along what each rule generalises over: where the digits, the point and
//! the exponent stand in a literal and how the exponent is signed; WHICH comment, where in
//! the statement it stands, how many lines it spans and which verb the statement around it
//! states; how many doubled quotes a literal carries and where in it they stand; and which
//! run or character the scanner stopped on.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row below addresses.
const TABLE: &str = "PartiqlLexerForms";

/// The whole sentence a parse defect is stated with: the fixed text DynamoDB states one
/// with, and the parser's phrase for the position the parse stopped at.
fn parse_failure(phrase: &str) -> String {
    format!("Statement wasn't well formed, can't be processed: {phrase}")
}

/// The phrase a defect the parser reports NONE for leaves behind: the fixed text, its colon
/// and the space after it are the whole of such a refusal.
const NO_PHRASE: &str = "";

/// The parser's phrase for a token the grammar has no place for where it stands.
const UNEXPECTED_TERM: &str = "Unexpected term";

/// One emulator holding one active table, keyed by a string partition key.
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    router
}

/// Run one statement, answering its HTTP status and body.
async fn execute(router: &MultiServiceRouter, statement: &str) -> (u16, Value) {
    call(router, "ExecuteStatement", json!({"Statement": statement})).await
}

/// The item stored under `key`, or `Value::Null` when the store holds none.
async fn stored(router: &MultiServiceRouter, key: &str) -> Value {
    let read = ok(
        router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": key}}}),
    )
    .await;
    read.get("Item").cloned().unwrap_or(Value::Null)
}

/// One numeric literal: how the statement writes it, and the canonical `N` text the number
/// it denotes is stored and reported as.
struct Literal {
    case: &'static str,
    written: &'static str,
    stored: &'static str,
}

/// The forms a numeric literal comes in, spanning where the digits, the point and the
/// exponent stand and how each is signed.
fn literals() -> Vec<Literal> {
    vec![
        Literal {
            case: "plain digits",
            written: "5",
            stored: "5",
        },
        Literal {
            case: "a fraction written with both parts",
            written: "0.5",
            stored: "0.5",
        },
        Literal {
            case: "a positive exponent",
            written: "1e5",
            stored: "100000",
        },
        Literal {
            case: "an uppercase exponent marker",
            written: "1E5",
            stored: "100000",
        },
        Literal {
            case: "a negative exponent",
            written: "1E-3",
            stored: "0.001",
        },
        Literal {
            case: "an explicitly signed exponent",
            written: "1e+2",
            stored: "100",
        },
        Literal {
            case: "an exponent of zero",
            written: "7e0",
            stored: "7",
        },
        Literal {
            case: "a leading decimal point",
            written: ".5",
            stored: "0.5",
        },
        Literal {
            case: "a leading decimal point under a sign",
            written: "-.5",
            stored: "-0.5",
        },
        Literal {
            case: "a leading decimal point under an exponent",
            written: ".5e2",
            stored: "50",
        },
        Literal {
            case: "a leading decimal point and a long fraction",
            written: ".125",
            stored: "0.125",
        },
        Literal {
            case: "trailing zeros in the fraction",
            written: "12.50",
            stored: "12.5",
        },
        Literal {
            case: "a fractional mantissa under an exponent",
            written: "1.25e3",
            stored: "1250",
        },
    ]
}

/// RULE 1: every form of a numeric literal denotes the number it spells — the stored value
/// is that number in its canonical text, and the same literal names it again in a predicate.
#[tokio::test]
async fn a_numeric_literal_denotes_the_number_it_spells_in_every_form() {
    let router = seeded().await;

    for (row, literal) in literals().into_iter().enumerate() {
        let key = format!("number-{row}");
        let statement = format!(
            "INSERT INTO \"{TABLE}\" VALUE {{'pk': '{key}', 'n': {}}}",
            literal.written
        );
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            status,
            200,
            "case {:?}: {statement:?} expected HTTP 200, got {status} with {answered}",
            literal.case
        );

        let item = stored(&router, &key).await;
        assert_eq!(
            item["n"],
            json!({"N": literal.stored}),
            "case {:?}: {:?} is stored as {:?}, got {item}",
            literal.case,
            literal.written,
            literal.stored
        );

        let selected = ok(
            &router,
            "ExecuteStatement",
            json!({"Statement": format!(
                "SELECT pk FROM \"{TABLE}\" WHERE pk = '{key}' AND n = {}",
                literal.written
            )}),
        )
        .await;
        assert_eq!(
            selected["Items"],
            json!([{"pk": {"S": &key}}]),
            "case {:?}: the same literal names the stored number in a predicate, got {selected}",
            literal.case
        );
    }
}

/// RULE 2: a statement carrying a comment runs as the statement around it, whichever verb
/// that statement states, and the same characters inside a quoted run are part of the string.
#[tokio::test]
async fn a_comment_is_text_the_parse_never_sees() {
    let router = seeded().await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "read"}, "n": {"N": "1"}}}),
    )
    .await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "markers"}, "s": {"S": "a--b"}, "block": {"S": "a/*b*/c"}
        }}),
    )
    .await;

    // Each row reads the one item it names, so a comment that was not passed over would
    // either refuse the statement or change what it selects.
    let reads = vec![
        (
            "a line comment before the statement",
            format!("-- read one item\nSELECT pk FROM \"{TABLE}\" WHERE pk = 'read'"),
        ),
        (
            "a line comment after the statement",
            format!("SELECT pk FROM \"{TABLE}\" WHERE pk = 'read' -- and nothing else"),
        ),
        (
            "a line comment the text never ends with a newline",
            format!("SELECT pk FROM \"{TABLE}\" WHERE pk = 'read'--end"),
        ),
        (
            "a line comment between two clauses",
            format!("SELECT pk FROM \"{TABLE}\" -- the source\nWHERE pk = 'read'"),
        ),
        (
            "a block comment between two clauses",
            format!("SELECT pk /* the projection */ FROM \"{TABLE}\" WHERE pk = 'read'"),
        ),
        (
            "a block comment spanning two lines",
            format!("SELECT pk\n/* the source\n   of the read */ FROM \"{TABLE}\" WHERE pk = 'read'"),
        ),
        (
            "a block comment with no space around it",
            format!("SELECT pk/*tight*/FROM \"{TABLE}\" WHERE pk = 'read'"),
        ),
        (
            "comments of both kinds, before an ORDER BY",
            format!(
                "-- both kinds\nSELECT pk FROM \"{TABLE}\" /* the source */ WHERE pk = 'read' \
                 ORDER BY pk ASC"
            ),
        ),
    ];
    for (case, statement) in reads {
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (status, answered["Items"].clone()),
            (200, json!([{"pk": {"S": "read"}}])),
            "case {case:?}: {statement:?} reads the item it names, got {status} with {answered}"
        );
    }

    // The same characters inside a quoted run are part of the string the run spells.
    for (case, attribute, text) in [
        ("a line comment marker inside a literal", "s", "a--b"),
        ("a block comment marker inside a literal", "block", "a/*b*/c"),
    ] {
        let statement = format!(
            "SELECT pk FROM \"{TABLE}\" WHERE pk = 'markers' AND {attribute} = '{text}'"
        );
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (status, answered["Items"].clone()),
            (200, json!([{"pk": {"S": "markers"}}])),
            "case {case:?}: {statement:?} compares the text {text:?}, got {status} with {answered}"
        );
    }

    // The rule is the LEXER's, so it holds for the statements that write as well: each of
    // these carries a comment and has to take effect.
    let written = format!(
        "INSERT /* the destination */ INTO \"{TABLE}\" VALUE {{'pk': 'written', 'n': 1}}"
    );
    let (status, answered) = execute(&router, &written).await;
    assert_eq!(
        status, 200,
        "case a commented INSERT: {written:?} expected HTTP 200, got {status} with {answered}"
    );
    assert_eq!(
        stored(&router, "written").await,
        json!({"pk": {"S": "written"}, "n": {"N": "1"}}),
        "case a commented INSERT: the item it carries is stored"
    );

    let updated =
        format!("UPDATE \"{TABLE}\" SET n = 2 -- bump it\nWHERE pk = 'written'");
    let (status, answered) = execute(&router, &updated).await;
    assert_eq!(
        status, 200,
        "case a commented UPDATE: {updated:?} expected HTTP 200, got {status} with {answered}"
    );
    assert_eq!(
        stored(&router, "written").await["n"],
        json!({"N": "2"}),
        "case a commented UPDATE: the write it states is applied"
    );

    let deleted = format!("DELETE FROM \"{TABLE}\" WHERE pk = 'written' /* done with it */");
    let (status, answered) = execute(&router, &deleted).await;
    assert_eq!(
        status, 200,
        "case a commented DELETE: {deleted:?} expected HTTP 200, got {status} with {answered}"
    );
    assert_eq!(
        stored(&router, "written").await,
        Value::Null,
        "case a commented DELETE: the item it names is removed"
    );
}

/// RULE 3: a quoted run spells its own delimiter by doubling it — the `'string'` literal and
/// the `"name"` identifier alike.
#[tokio::test]
async fn a_quoted_run_spells_its_delimiter_by_doubling_it() {
    let router = seeded().await;

    // One row per form a doubled quote stands in: in the middle of a literal, at either
    // end, twice over, and as the whole of one.
    let literals = vec![
        ("a doubled quote in the middle", "'it''s'", "it's"),
        ("a literal that is only a doubled quote", "''''", "'"),
        ("the empty literal", "''", ""),
        ("two doubled quotes", "'a''b''c'", "a'b'c"),
        ("a doubled quote at each end", "'''a'''", "'a'"),
        ("a doubled quote beside the closing one", "'a'''", "a'"),
    ];
    for (row, (case, written, text)) in literals.into_iter().enumerate() {
        let key = format!("quoted-{row}");
        let statement =
            format!("INSERT INTO \"{TABLE}\" VALUE {{'pk': '{key}', 's': {written}}}");
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {statement:?} expected HTTP 200, got {status} with {answered}"
        );
        let item = stored(&router, &key).await;
        assert_eq!(
            item["s"],
            json!({"S": text}),
            "case {case:?}: {written:?} spells the text {text:?}, got {item}"
        );
        let selected = ok(
            &router,
            "ExecuteStatement",
            json!({"Statement": format!(
                "SELECT pk FROM \"{TABLE}\" WHERE pk = '{key}' AND s = {written}"
            )}),
        )
        .await;
        assert_eq!(
            selected["Items"],
            json!([{"pk": {"S": &key}}]),
            "case {case:?}: the same literal names the stored text in a predicate, got {selected}"
        );
    }

    // The identifier run doubles its own delimiter the same way: this attribute's name
    // carries a double quote, so only the doubled spelling can name it.
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"pk": {"S": "named"}, "a\"b": {"N": "1"}}}),
    )
    .await;
    let selected = ok(
        &router,
        "ExecuteStatement",
        json!({"Statement": format!(
            "SELECT \"a\"\"b\" FROM \"{TABLE}\" WHERE pk = 'named'"
        )}),
    )
    .await;
    assert_eq!(
        selected["Items"],
        json!([{"a\"b": {"N": "1"}}]),
        "case a doubled quote in an identifier: the projection names the attribute, got \
         {selected}"
    );
}

/// RULE 4: text the scanner cannot read states the fixed sentence with no phrase behind it,
/// while a `.` that opens no number is a token of the language and is refused as the term it
/// is.
#[tokio::test]
async fn text_the_scanner_cannot_read_carries_no_phrase() {
    let router = seeded().await;

    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a block comment the text never closes",
            format!("/* never closed\nSELECT pk FROM \"{TABLE}\""),
            NO_PHRASE,
        ),
        (
            "a block comment closed by a star alone",
            format!("SELECT pk FROM \"{TABLE}\" /* nearly closed *"),
            NO_PHRASE,
        ),
        (
            "a string literal the text never closes",
            format!("SELECT pk FROM \"{TABLE}\" WHERE s = 'abc"),
            NO_PHRASE,
        ),
        (
            "an odd number of quotes, so the last run never closes",
            format!("SELECT pk FROM \"{TABLE}\" WHERE s = 'it's'"),
            NO_PHRASE,
        ),
        (
            "a quoted name the text never closes",
            format!("SELECT pk FROM \"{TABLE}"),
            NO_PHRASE,
        ),
        (
            "an exponent marker with nothing behind it",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = 1e"),
            NO_PHRASE,
        ),
        (
            "an exponent marker with only a sign behind it",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = 1e+"),
            NO_PHRASE,
        ),
        (
            "a second decimal point in one lexeme",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = 1.2.3"),
            NO_PHRASE,
        ),
        (
            "a number run together with a word",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = 1abc"),
            NO_PHRASE,
        ),
        (
            "a number run together with an underscore",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = 1_0"),
            NO_PHRASE,
        ),
        (
            "a hash, which the language spells nothing with",
            format!("# a comment in another dialect\nSELECT pk FROM \"{TABLE}\""),
            NO_PHRASE,
        ),
        (
            "the concatenation operator of another dialect",
            format!("SELECT pk FROM \"{TABLE}\" WHERE s = 'a' || 'b'"),
            NO_PHRASE,
        ),
        (
            "a backslash, which escapes nothing here",
            format!("SELECT pk FROM \"{TABLE}\" WHERE s = \\'a\\'"),
            NO_PHRASE,
        ),
        (
            "a parameter marker of another dialect",
            format!("SELECT pk FROM \"{TABLE}\" WHERE pk = @name"),
            NO_PHRASE,
        ),
        (
            "a positional marker of another dialect",
            format!("SELECT pk FROM \"{TABLE}\" WHERE pk = $1"),
            NO_PHRASE,
        ),
        (
            "an arithmetic operator the language does not spell",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n % 2 = 0"),
            NO_PHRASE,
        ),
        (
            "a negation of another dialect",
            format!("SELECT pk FROM \"{TABLE}\" WHERE !pk = 'read'"),
            NO_PHRASE,
        ),
        (
            "a tilde",
            format!("SELECT pk FROM \"{TABLE}\" WHERE s ~ 'a'"),
            NO_PHRASE,
        ),
        // A `.` with no digit behind it is the step separator of a document path, which IS
        // a token of the language: the text became tokens and the parse stopped at one.
        (
            "a decimal point with no digit behind it",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = ."),
            UNEXPECTED_TERM,
        ),
        (
            "a decimal point with an exponent marker behind it",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = .e1"),
            UNEXPECTED_TERM,
        ),
        (
            "a decimal point with a word behind it",
            format!("SELECT pk FROM \"{TABLE}\" WHERE n = .x"),
            UNEXPECTED_TERM,
        ),
    ];

    for (case, statement, phrase) in rows {
        let (status, answered) = execute(&router, &statement).await;
        assert_eq!(
            (
                status,
                error_code(&answered).as_str(),
                message(&answered).as_str()
            ),
            (400, "ValidationException", parse_failure(phrase).as_str()),
            "case {case:?}: {statement:?} expected the phrase {phrase:?}, got {status} with \
             {answered}"
        );
    }
}
