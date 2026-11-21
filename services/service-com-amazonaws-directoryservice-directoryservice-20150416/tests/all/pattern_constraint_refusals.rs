// SPDX-License-Identifier: CC0-1.0

//! A modelled `@pattern` the boundary validator's own engine cannot compile, driven end to
//! end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router.
//!
//! The rule under test: a value a member's `@pattern` does not admit is refused BEFORE the
//! handler runs, with the protocol-level constraint refusal — HTTP 400 `ValidationException`
//! whose body counts the clauses, names the member by the path a validation message spells
//! it with and quotes the expression the model declares — and that holds for an expression
//! written with look-around, which the `regex` crate behind `@pattern` cannot compile and so
//! leaves admitted, exactly as it holds for one that crate does compile. A value the
//! expression DOES admit is not refused by it at all and reaches the handler.
//!
//! It is asserted over a TABLE rather than one recorded call, along the two things the rule
//! generalises over:
//!
//! * the VALUE: for the administrator `Password` of a create, each way a value can fall
//!   outside the expression (shorter than the minimum length, longer than the maximum, and
//!   each two-class combination of Active Directory's four character classes) and each way
//!   it can satisfy it (three classes without a symbol, all four, and both published length
//!   bounds);
//! * the MEMBER and the OPERATION: the same `Password` shape through BOTH creates that
//!   declare it, whose refusal bodies must be identical, and a second member with a
//!   look-around expression of its own on a third operation — the account `UserName` a
//!   password reset names, whose expression lists the characters a name may not carry.
//!
//! A clause about a `@sensitive` member states NO value: `Password` is `@sensitive`, so the
//! refusal names the member and the expression and never the password — which the table
//! asserts on every refusal it reads. The expression a clause quotes is the one the whole
//! value has to satisfy: the model's own text where the model wraps it in a matched `^`/`$`
//! pair (the account `UserName` below), and with an end the model leaves open spelled out
//! where it does not (the administrator `Password`, whose model text anchors its start and
//! closes on an inner anchor).
//!
//! The last case is about the CLASS rather than the clause: a constraint the request breaks
//! is answered by the request layer in the protocol-level `ValidationException` envelope,
//! while a refusal a HANDLER returns is one of that operation's own modelled Directory
//! Service errors — two answers a client branches on differently, so neither may be sent in
//! the other's envelope.

use serde_json::{json, Value};

use crate::directory_lifecycle::minimal_create;
use crate::harness::{call, ok, service};
use crate::microsoft_ad::minimal_microsoft_ad;

/// The `@pattern` the model declares on the `Password` shape both creates' administrator
/// password targets (`smithy_query(query="shape", shape="Password")`): a length assertion
/// and an alternation of the four three-class combinations of Active Directory's character
/// classes, all written as look-ahead.
const PASSWORD_PATTERN: &str = r"^(?=^.{8,64}$)((?=.*\d)(?=.*[A-Z])(?=.*[a-z])|(?=.*\d)(?=.*[^A-Za-z0-9\s])(?=.*[a-z])|(?=.*[^A-Za-z0-9\s])(?=.*[A-Z])(?=.*[a-z])|(?=.*\d)(?=.*[A-Z])(?=.*[^A-Za-z0-9\s]))^";

/// That expression as a clause QUOTES it: the one the whole value has to satisfy. The model
/// anchors it at the START only — its text closes on an inner `^` rather than on `$` — so
/// the END it leaves open is spelled out with the unconstrained run that end admits, and the
/// start anchor goes with the rendering. Derived from [`PASSWORD_PATTERN`], so the quoted
/// text cannot drift from the expression the model declares.
fn stated_password_pattern() -> String {
    format!(
        "{}.*",
        PASSWORD_PATTERN
            .strip_prefix('^')
            .expect("the modelled password pattern anchors its start")
    )
}

/// The `@pattern` the model declares on `ResetUserPasswordRequest$UserName`
/// (`smithy_query(query="operation", operation="ResetUserPassword")`): a look-ahead listing
/// the characters an account name may not carry.
const RESET_USER_NAME_PATTERN: &str =
    r#"^(?!.*\\|.*"|.*\/|.*\[|.*\]|.*:|.*;|.*\||.*=|.*,|.*\+|.*\*|.*\?|.*<|.*>|.*@).*$"#;

/// A `NewPassword` the directory's policy accepts, so a reset case turns on its `UserName`
/// and on nothing else.
const GOOD_NEW_PASSWORD: &str = "Sim2Real-Reset-Pw1!";

/// A well-formed `DirectoryId` no directory is created under, so a reset whose `UserName`
/// clears the expression reaches the handler and is refused there instead.
const ABSENT_DIRECTORY_ID: &str = "d-0123456789";

/// The published MINIMUM length of the administrator password, from the length clause of
/// [`PASSWORD_PATTERN`] (`(?=^.{8,64}$)`) — the same bound
/// `CreateDirectoryRequest$Password`'s own documentation states in prose ("Must be between 8
/// and 64 characters").
const MIN_PASSWORD_CHARS: usize = 8;

/// The published MAXIMUM length of the administrator password, from the other half of that
/// same clause.
const MAX_PASSWORD_CHARS: usize = 64;

/// A password of `characters` characters drawn from three character classes, so its LENGTH
/// is the clause a case turns on. Built from the bound rather than written out.
fn sized_password(characters: usize) -> String {
    let fixed = "Sim2";
    format!("{fixed}{}", "a".repeat(characters.saturating_sub(fixed.len())))
}

/// The envelope a refusal of the request-constraint class opens with, counting the clauses
/// it carries — one, for every row of this suite. THE one place this suite spells it, so a
/// clause it builds in full and a clause it only recognises by its opening cannot disagree.
const ONE_CLAUSE_COUNTED: &str = "1 validation error detected: ";

/// The whole clause the boundary states for a `@sensitive` member outside its `@pattern`:
/// the member, the requirement, and no value at all.
fn withheld_clause(member: &str, pattern: &str) -> String {
    format!(
        "{ONE_CLAUSE_COUNTED}Value at '{member}' failed to satisfy constraint: \
         Member must satisfy regular expression pattern: {pattern}"
    )
}

/// The whole clause the boundary states for a member whose value it may quote.
fn quoted_clause(value: &str, member: &str, pattern: &str) -> String {
    format!(
        "{ONE_CLAUSE_COUNTED}Value '{value}' at '{member}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {pattern}"
    )
}

/// The `message` of a refusal body, whichever casing the envelope carries it in: the
/// boundary's `ValidationException` reports `message`, a modelled error of this service
/// reports `Message`.
fn message(parsed: &Value, case: &str) -> String {
    parsed["message"]
        .as_str()
        .or_else(|| parsed["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected a message, got {parsed}"))
        .to_string()
}

/// Assert `operation` with `body` is refused with the boundary's constraint envelope and
/// `expected` as its whole message, and return the body so a caller can compare two.
async fn refused_by_the_boundary(
    router: &smithy_server_runtime::MultiServiceRouter,
    operation: &str,
    body: Value,
    expected: &str,
    case: &str,
) -> Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to refuse the value, got {status} with {parsed}"
    );
    assert_eq!(
        parsed["__type"],
        json!("ValidationException"),
        "case {case:?}: expected the boundary's validation envelope, got {parsed}"
    );
    let stated = message(&parsed, case);
    assert_eq!(
        stated, expected,
        "case {case:?}: expected {expected:?}, got {stated:?}"
    );
    parsed
}

/// RULE: a `Password` outside the modelled expression is refused by the boundary, in the
/// same envelope and with the same clause whichever create declared the member — and the
/// clause never states the `@sensitive` value.
#[tokio::test]
async fn a_password_outside_its_pattern_is_refused_by_the_boundary() {
    let router = service();

    let too_short = sized_password(MIN_PASSWORD_CHARS - 1);
    let too_long = sized_password(MAX_PASSWORD_CHARS + 1);
    // One row per way a value falls outside the expression: each end of the length clause,
    // and every two-class combination of the four character classes the alternation counts
    // three of (lower, upper, digit, symbol).
    let rows: [(&str, &str); 8] = [
        ("one character below the minimum length", too_short.as_str()),
        ("one character above the maximum length", too_long.as_str()),
        ("lower case and digits only", "sim2real99pass"),
        ("lower case and upper case only", "SimRealPassword"),
        ("lower case and symbols only", "sim-real!pass"),
        ("upper case and digits only", "SIM2REAL99PASS"),
        ("upper case and symbols only", "SIM-REAL!PASS"),
        ("digits and symbols only", "2024-99!!2024"),
    ];

    let expected = withheld_clause("password", &stated_password_pattern());
    for (index, (class, password)) in rows.iter().enumerate() {
        // The constraint travels with the Password SHAPE, not with the entry point, so the
        // two creates that declare it must answer the same body for the same value.
        let mut simple = minimal_create(&format!("pw{index}.simple.example.com"));
        simple["Password"] = json!(password);
        let mut managed = minimal_microsoft_ad(&format!("pw{index}.managed.example.com"));
        managed["Password"] = json!(password);

        let from_create = refused_by_the_boundary(
            &router,
            "CreateDirectory",
            simple,
            &expected,
            &format!("CreateDirectory: {class}"),
        )
        .await;
        let from_managed = refused_by_the_boundary(
            &router,
            "CreateMicrosoftAD",
            managed,
            &expected,
            &format!("CreateMicrosoftAD: {class}"),
        )
        .await;
        assert_eq!(
            from_create["message"], from_managed["message"],
            "case {class:?}: expected both creates to state one clause, got {from_create} and \
             {from_managed}"
        );

        // The value is `@sensitive`: no part of the refusal reports it back.
        for (operation, parsed) in [
            ("CreateDirectory", &from_create),
            ("CreateMicrosoftAD", &from_managed),
        ] {
            assert!(
                !parsed.to_string().contains(password),
                "case {class:?}: expected {operation} not to echo the password, got {parsed}"
            );
        }
    }
}

/// RULE: a `Password` the expression ADMITS clears validation and reaches the handler, which
/// creates the directory — so the check admits exactly the values the model's expression
/// matches.
#[tokio::test]
async fn a_password_its_pattern_admits_is_not_refused_by_it() {
    let router = service();

    let at_the_minimum = sized_password(MIN_PASSWORD_CHARS);
    let at_the_maximum = sized_password(MAX_PASSWORD_CHARS);
    // Three of the four classes with no symbol at all (so demanding one would be
    // over-strict), all four classes, and each published length bound.
    let rows: [(&str, &str); 4] = [
        ("three classes, no symbol", "Sim2RealPassword"),
        ("all four classes", "Sim2Real#Pass99"),
        ("at the minimum length", at_the_minimum.as_str()),
        ("at the maximum length", at_the_maximum.as_str()),
    ];

    for (index, (class, password)) in rows.iter().enumerate() {
        for (operation, body_of) in [
            (
                "CreateDirectory",
                (|name: &str| minimal_create(name)) as fn(&str) -> Value,
            ),
            (
                "CreateMicrosoftAD",
                (|name: &str| minimal_microsoft_ad(name)) as fn(&str) -> Value,
            ),
        ] {
            let case = format!("{operation}: {class}");
            let mut body = body_of(&format!(
                "ok{index}.{}.example.com",
                operation.to_lowercase()
            ));
            body["Password"] = json!(password);
            let answer = ok(&router, operation, body, &case).await;
            assert!(
                answer["DirectoryId"].is_string(),
                "case {case:?}: expected the create to be served, got {answer}"
            );
        }
    }
}

/// RULE: the same refusal holds for ANOTHER member whose expression uses look-around — the
/// account `UserName` a reset names — quoting that member's own expression, and echoing the
/// value because that member is not `@sensitive`.
#[tokio::test]
async fn a_user_name_outside_its_pattern_is_refused_by_the_boundary() {
    let router = service();

    // One row per character the expression's look-ahead forbids, which is what the clause
    // is about; the names are otherwise well formed.
    let forbidden = [
        ("a backslash", r"corp\admin"),
        ("a double quote", "corp\"admin"),
        ("a forward slash", "corp/admin"),
        ("an opening bracket", "corp[admin"),
        ("a colon", "corp:admin"),
        ("a semicolon", "corp;admin"),
        ("a pipe", "corp|admin"),
        ("an equals sign", "corp=admin"),
        ("a comma", "corp,admin"),
        ("a plus sign", "corp+admin"),
        ("an asterisk", "corp*admin"),
        ("a question mark", "corp?admin"),
        ("a less-than sign", "corp<admin"),
        ("an at sign", "admin@corp.example.com"),
    ];

    for (class, user_name) in forbidden {
        refused_by_the_boundary(
            &router,
            "ResetUserPassword",
            json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "UserName": user_name,
                "NewPassword": GOOD_NEW_PASSWORD,
            }),
            &quoted_clause(user_name, "userName", RESET_USER_NAME_PATTERN),
            &format!("ResetUserPassword: {class}"),
        )
        .await;
    }

    // The positive control: a name the expression admits is not refused by it at all — it
    // reaches the handler, which resolves the directory the request named and refuses THAT
    // instead.
    for user_name in ["Administrator", "Admin", "corp.admin_1-x", "a"] {
        let case = format!("ResetUserPassword: the name {user_name:?}");
        let (status, parsed) = call(
            &router,
            "ResetUserPassword",
            json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "UserName": user_name,
                "NewPassword": GOOD_NEW_PASSWORD,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the absent directory to be refused, got {status} with \
             {parsed}"
        );
        let wire_type = parsed["__type"].as_str().unwrap_or_default();
        assert!(
            wire_type.contains("EntityDoesNotExistException"),
            "case {case:?}: expected the resolution failure rather than a constraint refusal, \
             got {parsed}"
        );
    }
}

/// The error class the request-constraint layer refuses a request with, which is not one of
/// any operation's modelled errors: the protocol-level one an SDK's generic input-validation
/// code handles.
const CONSTRAINT_CLASS: &str = "ValidationException";

/// The member a refusal of the constraint class states its diagnostic in: the lower-case
/// `message` of the framework's constraint envelope.
const CONSTRAINT_MESSAGE_MEMBER: &str = "message";

/// The members a refusal of one of this service's MODELLED errors states instead: every
/// error shape Directory Service declares carries them, and the constraint envelope carries
/// neither.
const MODELLED_MEMBERS: [&str; 2] = ["Message", "RequestId"];

/// A `Password` the modelled expression does not admit: two of Active Directory's four
/// character classes, where the alternation counts three.
const PASSWORD_OUTSIDE_THE_PATTERN: &str = "sim2real99pass";

/// A `Password` the modelled expression ADMITS — three classes, inside the length clause —
/// which the directory's own administrator-password policy still refuses, because it carries
/// the name of the account it would be the password of. That clause is about a WORD rather
/// than a character grammar, so no `@pattern` can state it and only the handler can refuse
/// it: the other side of the class split below.
const PASSWORD_NAMING_THE_ADMINISTRATOR: &str = "Sim2Admin-Pw1";

/// A `UserName` outside its member's expression, whose look-ahead forbids a pipe.
const USER_NAME_OUTSIDE_THE_PATTERN: &str = "corp|admin";

/// A `DirectoryId` outside the `@pattern` the model declares for it
/// (`^d-[0-9a-f]{10}$`) — an expression the boundary validator's own engine DOES compile, so
/// the row it drives is refused by the walk that applies an expression rather than by the
/// layer that stands in for the ones it cannot.
const DIRECTORY_ID_OUTSIDE_THE_PATTERN: &str = "d-not-a-hex";

/// RULE: the two refusals a request can get are two different ANSWERS, each in its own
/// envelope. A constraint the REQUEST breaks is answered by the request layer, before any
/// handler runs, as the protocol-level `ValidationException`: HTTP 400 whose diagnostic is
/// the counted clause in the constraint envelope's lower-case `message`, carrying none of the
/// members a modelled Directory Service fault carries. A refusal a HANDLER returns — here the
/// half of the administrator-password policy no expression can state, and a directory that
/// does not exist — is one of that operation's own MODELLED errors instead, with `Message` and
/// `RequestId` and no constraint clause at all. So a client branching on the class and one
/// reading the diagnostic each get what they branch on: a broken `@pattern` is never reported
/// as a service fault, and a service fault is never reported as a constraint violation.
#[tokio::test]
async fn a_broken_constraint_and_a_handler_refusal_carry_different_envelopes() {
    let router = service();

    let mut weak_simple = minimal_create("envelope0.simple.example.com");
    weak_simple["Password"] = json!(PASSWORD_OUTSIDE_THE_PATTERN);
    let mut weak_managed = minimal_microsoft_ad("envelope0.managed.example.com");
    weak_managed["Password"] = json!(PASSWORD_OUTSIDE_THE_PATTERN);

    // One row per way a request can break a declared expression: the `@sensitive`
    // administrator password of BOTH creates that declare it (an expression written with
    // look-around), a member whose value a clause may quote, and a member whose expression
    // the boundary's own engine compiles — one class, so one envelope, whichever layer
    // applied the expression.
    let broken_constraints: Vec<(&str, &str, Value, &str)> = vec![
        (
            "CreateDirectory: the administrator password",
            "CreateDirectory",
            weak_simple,
            "password",
        ),
        (
            "CreateMicrosoftAD: the administrator password",
            "CreateMicrosoftAD",
            weak_managed,
            "password",
        ),
        (
            "ResetUserPassword: the account name",
            "ResetUserPassword",
            json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "UserName": USER_NAME_OUTSIDE_THE_PATTERN,
                "NewPassword": GOOD_NEW_PASSWORD,
            }),
            "userName",
        ),
        (
            "DeleteDirectory: the directory id",
            "DeleteDirectory",
            json!({ "DirectoryId": DIRECTORY_ID_OUTSIDE_THE_PATTERN }),
            "directoryId",
        ),
    ];

    for (case, operation, body, member) in broken_constraints {
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the request to be refused, got {status} with {parsed}"
        );
        assert_eq!(
            parsed["__type"],
            json!(CONSTRAINT_CLASS),
            "case {case:?}: expected the request layer's class rather than a modelled fault, \
             got {parsed}"
        );
        let stated = parsed[CONSTRAINT_MESSAGE_MEMBER].as_str().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the constraint envelope's {CONSTRAINT_MESSAGE_MEMBER} \
                 member, got {parsed}"
            )
        });
        let clause = format!("at '{member}' failed to satisfy constraint: ");
        assert!(
            stated.starts_with(ONE_CLAUSE_COUNTED) && stated.contains(&clause),
            "case {case:?}: expected a counted clause naming {member:?}, got {stated:?}"
        );
        for absent in MODELLED_MEMBERS {
            assert!(
                parsed[absent].is_null(),
                "case {case:?}: expected no modelled {absent} member on a constraint refusal, \
                 got {parsed}"
            );
        }
        // The administrator password is `@sensitive`: no part of the answer reports it back.
        assert!(
            !parsed.to_string().contains(PASSWORD_OUTSIDE_THE_PATTERN),
            "case {case:?}: expected the password not to be echoed, got {parsed}"
        );
    }

    let mut named_simple = minimal_create("envelope1.simple.example.com");
    named_simple["Password"] = json!(PASSWORD_NAMING_THE_ADMINISTRATOR);
    let mut named_managed = minimal_microsoft_ad("envelope1.managed.example.com");
    named_managed["Password"] = json!(PASSWORD_NAMING_THE_ADMINISTRATOR);

    // One row per handler refusal of a request whose every declared expression is satisfied:
    // the policy clause no expression states, on BOTH creates that apply it, and a directory
    // the account does not hold — two different modelled error shapes, one envelope.
    let handler_refusals: Vec<(&str, &str, Value, &str)> = vec![
        (
            "CreateDirectory: a password naming the administrator account",
            "CreateDirectory",
            named_simple,
            "InvalidParameterException",
        ),
        (
            "CreateMicrosoftAD: a password naming the administrator account",
            "CreateMicrosoftAD",
            named_managed,
            "InvalidParameterException",
        ),
        (
            "ResetUserPassword: a directory the account does not hold",
            "ResetUserPassword",
            json!({
                "DirectoryId": ABSENT_DIRECTORY_ID,
                "UserName": "Administrator",
                "NewPassword": GOOD_NEW_PASSWORD,
            }),
            "EntityDoesNotExistException",
        ),
    ];

    for (case, operation, body, modelled) in handler_refusals {
        let (status, parsed) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the request to be refused, got {status} with {parsed}"
        );
        let wire_type = parsed["__type"].as_str().unwrap_or_default();
        assert!(
            wire_type.contains(modelled),
            "case {case:?}: expected the modelled {modelled}, got {parsed}"
        );
        for stated_member in MODELLED_MEMBERS {
            assert!(
                parsed[stated_member].is_string(),
                "case {case:?}: expected the modelled {stated_member} member, got {parsed}"
            );
        }
        assert!(
            parsed[CONSTRAINT_MESSAGE_MEMBER].is_null(),
            "case {case:?}: expected no {CONSTRAINT_MESSAGE_MEMBER} member of the constraint \
             envelope on a modelled refusal, got {parsed}"
        );
        let stated = parsed["Message"].as_str().unwrap_or_default();
        assert!(
            !stated.contains(ONE_CLAUSE_COUNTED.trim_end_matches(' ')),
            "case {case:?}: expected the service's own sentence rather than a counted \
             constraint clause, got {stated:?}"
        );
        // The password is `@sensitive` whichever layer refuses the request.
        assert!(
            !parsed.to_string().contains(PASSWORD_NAMING_THE_ADMINISTRATOR),
            "case {case:?}: expected the password not to be echoed, got {parsed}"
        );
    }
}
