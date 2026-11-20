// SPDX-License-Identifier: CC0-1.0

//! Every parameter a submitted template DECLARES needs a value in force, whatever spelling
//! the body is written in and however the value went missing.
//!
//! `Parameter$ParameterKey` is documented "if you don't specify a key and value for a
//! particular parameter, CloudFormation uses the default value that's specified in your
//! template" (`smithy_query(query="shape", shape="Parameter")`), so a declaration that states
//! no `Default` has no value of its own to fall back on: a stack operation that puts no value
//! in force for it is refused with the awsQuery common error `ValidationError` at HTTP 400 —
//! neither `CreateStack` nor `UpdateStack` models an error for this case
//! (`smithy_query(query="operation", operation="CreateStack,UpdateStack")`) — reading
//! `Parameters: [<names>] must have values`, where the fixed text is `Parameters: [` …
//! `] must have values` and the variable part is every such declaration, and nothing is
//! created or changed.
//!
//! `parameter_set_contract.rs` holds that rule, and the undeclared-key rule beside it, over
//! the SETS a JSON body and a request can state. This suite spans the two dimensions that
//! table holds fixed, because neither is what the rule is about:
//!
//! * how the body is WRITTEN — "you can write CloudFormation templates in JSON or YAML", and
//!   the YAML spelling is read by a second reader, whose short-form tags (`!Ref`) are the
//!   template's own — so each row is submitted in both spellings and must answer the same;
//! * how the value goes MISSING — a request that states no entry for the key at all, and one
//!   that states the key with no `ParameterValue` (no `UsePreviousValue` either, so it asks
//!   for nothing) — and, on the other side of the rule, that a value in force may be the
//!   EMPTY string, since what the rule wants is a value, not a non-empty one.
//!
//! A row also carries the declarations whose own constraints the request SATISFIES beside the
//! unsatisfied one, so the set rule is shown to bite on a request every per-value constraint
//! admits rather than only on one that trips a constraint first.
//!
//! The sentences are spelled out here rather than imported from the crate, because the
//! sentence IS the requirement: a reworded message must fail this suite.

use crate::harness::{
    call, create_stack, member, router, wait_for_the_transition, xml_text, xml_texts,
};

/// A declaration member's value, rendered the way a template writes a scalar.
///
/// A double-quoted scalar is text in both spellings (YAML 1.2 is a superset of JSON) and a
/// plain one is the number or boolean it spells, so one rendering serves both bodies.
#[derive(Debug, Clone, Copy)]
enum Scalar {
    Text(&'static str),
    Plain(&'static str),
}

impl Scalar {
    fn rendered(self) -> String {
        match self {
            Scalar::Text(text) => format!("\"{text}\""),
            Scalar::Plain(literal) => literal.to_string(),
        }
    }
}

/// One entry of a template's `Parameters` section: its name and the members it states.
///
/// A declaration is REQUIRED exactly when its members carry no `Default`, which is what the
/// rule under test is about; the other members are the ones the template reference documents
/// for the section (`Type`, `NoEcho`, `AllowedPattern`, `MinLength`/`MaxLength`,
/// `MinValue`/`MaxValue`), carried so a row can state a declaration the request satisfies.
#[derive(Debug, Clone, Copy)]
struct Declaration {
    name: &'static str,
    members: &'static [(&'static str, Scalar)],
}

/// A `String` declaration constrained by a pattern and a length window, which the rows that
/// state a value for it satisfy.
const SLUG: Declaration = Declaration {
    name: "Slug",
    members: &[
        ("Type", Scalar::Text("String")),
        ("AllowedPattern", Scalar::Text("^[a-z][a-z0-9-]*$")),
        ("MinLength", Scalar::Plain("3")),
        ("MaxLength", Scalar::Plain("32")),
    ],
};

/// A `Number` declaration constrained by a numeric window.
const RETENTION: Declaration = Declaration {
    name: "Retention",
    members: &[
        ("Type", Scalar::Text("Number")),
        ("MinValue", Scalar::Plain("1")),
        ("MaxValue", Scalar::Plain("30")),
    ],
};

/// A masked declaration with no `Default`: required, and carrying the one member that keeps
/// its value out of a stack's reads.
const SECRET: Declaration = Declaration {
    name: "Secret",
    members: &[
        ("Type", Scalar::Text("String")),
        ("NoEcho", Scalar::Plain("true")),
    ],
};

/// A required `String` declaration that constrains nothing.
const ALPHA: Declaration = Declaration {
    name: "Alpha",
    members: &[("Type", Scalar::Text("String"))],
};

/// A required `Number` declaration that constrains nothing.
const BETA: Declaration = Declaration {
    name: "Beta",
    members: &[("Type", Scalar::Text("Number"))],
};

/// The same declaration as [`ALPHA`] with a `Default`, which answers for it.
const ALPHA_DEFAULTED: Declaration = Declaration {
    name: "Alpha",
    members: &[
        ("Type", Scalar::Text("String")),
        ("Default", Scalar::Text("from-the-template")),
    ],
};

/// The resource every body declares: a type no owning service has to be asked about, so a
/// case about the REQUEST does not depend on a deployment.
const RESOURCE_TYPE: &str = "AWS::CloudFormation::WaitConditionHandle";

/// The declarations as the `Parameters` section of a JSON body.
fn json_body(declarations: &[Declaration]) -> String {
    let section = declarations
        .iter()
        .map(|declaration| {
            let members = declaration
                .members
                .iter()
                .map(|(name, value)| format!("\"{name}\":{}", value.rendered()))
                .collect::<Vec<_>>()
                .join(",");
            format!("\"{}\":{{{members}}}", declaration.name)
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"Parameters":{{{section}}},
            "Resources":{{"Handle":{{"Type":"{RESOURCE_TYPE}"}}}}}}"#
    )
}

/// The same declarations as the `Parameters` section of a YAML body, whose handle carries the
/// SHORT FORM of `Ref` — the spelling only the YAML reader has to expand — against the first
/// declaration, so the rule is exercised on a body that spells an intrinsic that way.
fn yaml_body(declarations: &[Declaration]) -> String {
    let mut body = String::from("Parameters:\n");
    for declaration in declarations {
        body.push_str(&format!("  {}:\n", declaration.name));
        for (name, value) in declaration.members {
            body.push_str(&format!("    {name}: {}\n", value.rendered()));
        }
    }
    let referenced = declarations
        .first()
        .expect("every row declares a parameter")
        .name;
    body.push_str(&format!(
        "Resources:\n  \
         Handle:\n    \
         Type: {RESOURCE_TYPE}\n    \
         Metadata:\n      \
         Echoed: !Ref {referenced}\n"
    ));
    body
}

/// How the body of a row is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Json,
    Yaml,
}

/// Both spellings, since the rule is about the declarations and not about the spelling.
const FORMS: &[Form] = &[Form::Json, Form::Yaml];

impl Form {
    fn body(self, declarations: &[Declaration]) -> String {
        match self {
            Form::Json => json_body(declarations),
            Form::Yaml => yaml_body(declarations),
        }
    }

    /// The part of a stack name that keeps one form's stacks apart from the other's.
    fn tag(self) -> &'static str {
        match self {
            Form::Json => "json",
            Form::Yaml => "yaml",
        }
    }
}

/// One `Parameters` entry a request states: the key, and the value it states — `None` for an
/// entry that states the key and no `ParameterValue` at all.
type Entry = (&'static str, Option<&'static str>);

/// One row: what the template declares, what the request states for it, and the refusal that
/// pair is answered with (`None` where every declaration has a value in force).
struct Row {
    case: &'static str,
    declarations: &'static [Declaration],
    supplied: &'static [Entry],
    refused: Option<&'static str>,
}

/// The class the rule covers: how many declarations are left without a value, whether the
/// others are constrained and satisfied, and each way a value can be missing or present.
const ROWS: &[Row] = &[
    Row {
        case: "the one declaration with no Default, nothing supplied",
        declarations: &[SECRET],
        supplied: &[],
        refused: Some("Parameters: [Secret] must have values"),
    },
    Row {
        case: "a declaration with no Default beside constrained ones the request satisfies",
        declarations: &[SLUG, RETENTION, SECRET],
        supplied: &[("Slug", Some("valid-slug")), ("Retention", Some("7"))],
        refused: Some("Parameters: [Secret] must have values"),
    },
    Row {
        case: "two declarations with no Default, nothing supplied",
        declarations: &[ALPHA, BETA],
        supplied: &[],
        refused: Some("Parameters: [Alpha, Beta] must have values"),
    },
    Row {
        case: "one of two declarations with no Default supplied",
        declarations: &[ALPHA, BETA],
        supplied: &[("Alpha", Some("a"))],
        refused: Some("Parameters: [Beta] must have values"),
    },
    Row {
        case: "an entry stating the key and no ParameterValue",
        declarations: &[ALPHA],
        supplied: &[("Alpha", None)],
        refused: Some("Parameters: [Alpha] must have values"),
    },
    Row {
        case: "an entry stating the key and no ParameterValue beside a satisfied declaration",
        declarations: &[SLUG, ALPHA],
        supplied: &[("Slug", Some("valid-slug")), ("Alpha", None)],
        refused: Some("Parameters: [Alpha] must have values"),
    },
    Row {
        case: "an entry whose stated value is the empty string, which IS a value in force",
        declarations: &[ALPHA],
        supplied: &[("Alpha", Some(""))],
        refused: None,
    },
    Row {
        case: "every declaration with no Default given a value",
        declarations: &[SLUG, RETENTION, SECRET],
        supplied: &[
            ("Slug", Some("valid-slug")),
            ("Retention", Some("7")),
            ("Secret", Some("s3cret")),
        ],
        refused: None,
    },
    Row {
        case: "a declaration whose own Default answers for it",
        declarations: &[ALPHA_DEFAULTED],
        supplied: &[],
        refused: None,
    },
];

/// The `Parameters.member.N.*` members one list of entries is sent as.
fn parameter_members(supplied: &[Entry]) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (index, (key, value)) in supplied.iter().enumerate() {
        let at = format!("Parameters.member.{}", index + 1);
        members.push(member(&format!("{at}.ParameterKey"), key));
        if let Some(value) = value {
            members.push(member(&format!("{at}.ParameterValue"), value));
        }
    }
    members
}

/// The `Code` and `Message` a refusal carries, as the pair a row compares.
fn refusal(body: &str) -> (Option<String>, Option<String>) {
    (xml_text(body, "Code"), xml_text(body, "Message"))
}

/// The refusal a row expects: the awsQuery common error, with the sentence the row states.
fn expected_refusal(message: &str) -> (Option<String>, Option<String>) {
    (
        Some("ValidationError".to_string()),
        Some(message.to_string()),
    )
}

#[tokio::test]
async fn a_create_refuses_a_declaration_left_without_a_value_in_either_body_spelling() {
    let router = router();
    for form in FORMS {
        for (index, row) in ROWS.iter().enumerate() {
            let case = format!("{:?} body, {}", form, row.case);
            let name = format!("reqparam-{}-{index}", form.tag());
            let (code, body) = create_stack(
                &router,
                &name,
                &form.body(row.declarations),
                &parameter_members(row.supplied),
            )
            .await;
            match row.refused {
                Some(message) => {
                    assert_eq!(
                        code, 400,
                        "case {case:?}: the create must be refused, got {code} {body}"
                    );
                    let answered = refusal(&body);
                    let expected = expected_refusal(message);
                    assert_eq!(
                        answered, expected,
                        "case {case:?}: expected {expected:?}, got {answered:?}"
                    );
                    // The refusal is of the REQUEST, so no stack is left behind under that
                    // name: the read of one is refused as a stack that does not exist.
                    let (read, read_body) =
                        call(&router, "DescribeStacks", &[member("StackName", &name)]).await;
                    assert_eq!(
                        read, 400,
                        "case {case:?}: a refused create leaves no stack, got {read} {read_body}"
                    );
                    let answered = refusal(&read_body);
                    let expected =
                        expected_refusal(&format!("Stack with id {name} does not exist"));
                    assert_eq!(
                        answered, expected,
                        "case {case:?}: expected {expected:?} from the read, got {answered:?}"
                    );
                }
                None => {
                    assert_eq!(
                        code, 200,
                        "case {case:?}: every declaration has a value in force, got {code} {body}"
                    );
                    assert!(
                        xml_text(&body, "StackId").is_some(),
                        "case {case:?}: an accepted create answers a stack id, got {body}"
                    );
                    // The values really went in force: the stack reports one parameter per
                    // declaration, whether the request stated it or the template defaulted it.
                    let (read, read_body) =
                        call(&router, "DescribeStacks", &[member("StackName", &name)]).await;
                    assert_eq!(
                        read, 200,
                        "case {case:?}: the created stack must be readable, got {read} {read_body}"
                    );
                    let mut reported = xml_texts(&read_body, "ParameterKey");
                    reported.sort();
                    let mut declared: Vec<String> = row
                        .declarations
                        .iter()
                        .map(|declaration| declaration.name.to_string())
                        .collect();
                    declared.sort();
                    assert_eq!(
                        reported, declared,
                        "case {case:?}: expected a value in force per declaration {declared:?}, \
                         got {reported:?}"
                    );
                }
            }
        }
    }
}

/// One row of the update table: the declarations the update's own body carries, the entries it
/// states, and the refusal that pair is answered with.
struct UpdateRow {
    case: &'static str,
    declarations: &'static [Declaration],
    supplied: &'static [Entry],
    refused: Option<&'static str>,
}

/// An update is held to the same rule: the body it SUBMITS is the template whose declarations
/// have to have values, and the stack's own values answer only for the keys it carries.
const UPDATE_ROWS: &[UpdateRow] = &[
    UpdateRow {
        case: "a submitted body declaring a parameter the update states no entry for",
        declarations: &[ALPHA, BETA],
        supplied: &[("Alpha", Some("a2"))],
        refused: Some("Parameters: [Beta] must have values"),
    },
    UpdateRow {
        case: "an entry stating the key and no ParameterValue, which asks for nothing",
        declarations: &[ALPHA],
        supplied: &[("Alpha", None)],
        refused: Some("Parameters: [Alpha] must have values"),
    },
    UpdateRow {
        case: "every declaration of the submitted body given a value",
        declarations: &[ALPHA, BETA],
        supplied: &[("Alpha", Some("a2")), ("Beta", Some("7"))],
        refused: None,
    },
];

#[tokio::test]
async fn an_update_refuses_a_declaration_left_without_a_value_and_changes_nothing() {
    let router = router();
    let name = "reqparam-update";
    let (code, body) = create_stack(
        &router,
        name,
        &yaml_body(&[ALPHA]),
        &parameter_members(&[("Alpha", Some("deployed"))]),
    )
    .await;
    assert_eq!(
        code, 200,
        "the stack the updates are applied to must be created, got {code} {body}"
    );
    wait_for_the_transition().await;

    for row in UPDATE_ROWS {
        let case = row.case;
        let mut members = vec![
            member("StackName", name),
            member("TemplateBody", &yaml_body(row.declarations)),
        ];
        members.extend(parameter_members(row.supplied));
        let (code, body) = call(&router, "UpdateStack", &members).await;
        match row.refused {
            Some(message) => {
                assert_eq!(
                    code, 400,
                    "case {case:?}: the update must be refused, got {code} {body}"
                );
                let answered = refusal(&body);
                let expected = expected_refusal(message);
                assert_eq!(
                    answered, expected,
                    "case {case:?}: expected {expected:?}, got {answered:?}"
                );
                // Nothing changed: the stack still reports the one parameter it deployed with.
                let (_, described) =
                    call(&router, "DescribeStacks", &[member("StackName", name)]).await;
                let reported = (
                    xml_text(&described, "ParameterKey"),
                    xml_text(&described, "ParameterValue"),
                );
                let expected = (Some("Alpha".to_string()), Some("deployed".to_string()));
                assert_eq!(
                    reported, expected,
                    "case {case:?}: the stack keeps what it had, expected {expected:?}, got \
                     {reported:?}"
                );
            }
            None => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: every declaration has a value in force, got {code} {body}"
                );
                wait_for_the_transition().await;
                let (_, described) =
                    call(&router, "DescribeStacks", &[member("StackName", name)]).await;
                let mut reported = xml_texts(&described, "ParameterValue");
                reported.sort();
                assert_eq!(
                    reported,
                    vec!["7".to_string(), "a2".to_string()],
                    "case {case:?}: the accepted update puts its values in force, got \
                     {described}"
                );
            }
        }
    }
}
