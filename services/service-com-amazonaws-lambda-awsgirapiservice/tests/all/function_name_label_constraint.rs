// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the modelled constraints on the `FunctionName` the function
//! operations bind to their URI PATH.
//!
//! RULE: a value a modelled member's own `@length` or `@pattern` does not admit is refused at
//! the service boundary — before the request reaches a handler and before anything is looked
//! up — with HTTP 400 `ValidationException`, whose message is AWS's constraint envelope built
//! from the request alone: `N validation error(s) detected: Value '<the submitted value>' at
//! '<the member>' failed to satisfy constraint: <the requirement>`, one clause per constraint
//! the value broke. That holds for a member bound to the request's PATH exactly as it holds
//! for one carried in its body: `FunctionName` targets `NamespacedFunctionName`, which
//! declares `length 1..256` and a `@pattern`
//! (`smithy_query(query="shape",shape="NamespacedFunctionName",model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`),
//! and every operation binding it as an `@httpLabel` is held to both.
//!
//! Why it matters beyond the wording: a refusal of the REQUEST is a terminal 400 a caller can
//! act on, while a failure that escapes the boundary and surfaces as a server-class answer
//! tells the caller — and every retry policy and dead-letter consumer reading the code — that
//! the service is at fault. EventBridge Scheduler's universal target reports the code the
//! invoked operation answered on the dead-letter event it writes, which is pinned over the
//! assembled router by `core/router/tests/all/scheduler_universal_target_invokes_its_lambda.rs`;
//! what this suite pins is Lambda's own half of it.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the FORM of the inadmissible value — whitespace alone (at two widths), the other ASCII
//!   whitespace characters, whitespace inside an otherwise admissible name, punctuation
//!   outside the member's character class, a non-ASCII letter, an inadmissible qualifier
//!   after an admissible name, and the ARN form with a space in it;
//! * the CONSTRAINT it breaks — the pattern, the length maximum (at one past it), and both at
//!   once, which is the envelope's plural form with a clause each;
//! * the OPERATION that binds the member, so the refusal is a property of the member and not
//!   of one route: `Invoke` (`POST /2015-03-31/functions/{FunctionName}/invocations`),
//!   `InvokeAsync` (`POST /2014-11-13/functions/{FunctionName}/invoke-async`) and
//!   `GetFunction` (`GET /2015-03-31/functions/{FunctionName}`), which also spans the two
//!   methods and a read as well as two writes;
//! * and, for the admissible side, the documented FORM the name addresses the function by.
//!
//! The one value the member's `length` minimum refuses — the empty string — is not a row: a
//! path label stands for one whole segment, so an empty one addresses a different route
//! altogether and there is no request carrying it on this member.

use std::sync::Arc;

use crate::function_lifecycle::{
    create_active_function, package_bytes, router_on, send, Answer, ACCOUNT, REGION,
};

/// The `NamespacedFunctionName` pattern as a refusal states it: the model's own expression
/// with the `^`/`$` anchors a whole-value Smithy pattern wraps it in dropped, and nothing
/// else changed.
const NAME_EXPRESSION: &str = r"(arn:(aws[a-zA-Z-]*)?:lambda:)?((eusc-)?[a-z]{2}((-gov)|(-iso([a-z]?)))?-[a-z]+-\d{1}:)?(\d{12}:)?(function:)?([a-zA-Z0-9-_\.]+)(:(\$LATEST(\.PUBLISHED)?|[a-zA-Z0-9-_]+))?";

/// The modelled maximum of that member (`length 1..256`).
const MAX_NAME_LENGTH: usize = 256;

/// The member path a clause of this refusal names the member by: the lowerCamel spelling the
/// validating walk reports a violation at.
const MEMBER_PATH: &str = "functionName";

/// The error code the JSON protocols answer a constraint refusal with.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The code Lambda answers a well-formed name it holds no function under with.
const NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// The function the admissible rows address, which exists for the whole suite.
const FUNCTION: &str = "label-pattern-fn";

/// The characters a path segment carries AS THEY STAND: RFC 3986's unreserved set plus the two
/// sub-delimiters a Lambda identifier is written with (`:` in an ARN or before a qualifier,
/// `$` in the unpublished version's label). Everything else is percent-escaped, which is what
/// a client's SDK does with a value bound to an `@httpLabel` — so a value the path cannot
/// carry still reaches the member it was sent on.
const LABEL_ESCAPED: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b':')
    .remove(b'$');

/// `value` as one path segment carries it.
fn label(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, LABEL_ESCAPED).to_string()
}

/// One requirement a clause of the refusal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Requirement {
    /// The member's `@pattern`.
    Pattern,
    /// The member's `@length` maximum.
    Length,
}

impl Requirement {
    /// The requirement as a clause states it.
    fn text(self) -> String {
        match self {
            Self::Pattern => {
                format!("Member must satisfy regular expression pattern: {NAME_EXPRESSION}")
            }
            Self::Length => {
                format!("Member must have length less than or equal to {MAX_NAME_LENGTH}")
            }
        }
    }

    /// The whole clause this requirement contributes for `value`.
    fn clause(self, value: &str) -> String {
        format!(
            "Value '{value}' at '{MEMBER_PATH}' failed to satisfy constraint: {}",
            self.text()
        )
    }
}

/// The envelope AWS states `count` violations under: singular for one, plural for more.
fn envelope(count: usize) -> String {
    let noun = if count == 1 { "error" } else { "errors" };
    format!("{count} validation {noun} detected: ")
}

/// Which operation a row's request is made as.
#[derive(Debug, Clone, Copy)]
enum Called {
    /// `Invoke`, asynchronously so nothing waits on a run: `POST
    /// /2015-03-31/functions/{FunctionName}/invocations`, accepted at 202.
    Invoke,
    /// The deprecated asynchronous entry point `InvokeAsync`: `POST
    /// /2014-11-13/functions/{FunctionName}/invoke-async`, accepted at 202.
    InvokeAsync,
    /// `GetFunction`, a read: `GET /2015-03-31/functions/{FunctionName}`, answered at 200.
    GetFunction,
}

impl Called {
    /// The method and the URI this call addresses `name` by.
    fn request(self, name: &str) -> (&'static str, String) {
        let label = label(name);
        match self {
            Self::Invoke => ("POST", format!("/2015-03-31/functions/{label}/invocations")),
            Self::InvokeAsync => (
                "POST",
                format!("/2014-11-13/functions/{label}/invoke-async"),
            ),
            Self::GetFunction => ("GET", format!("/2015-03-31/functions/{label}")),
        }
    }

    /// The status this call answers a request it SERVES with, as the model pins it
    /// (`Invoke`'s accepted asynchronous invocation and `InvokeAsync` both 202, `GetFunction`
    /// 200).
    fn served_status(self) -> u16 {
        match self {
            Self::Invoke | Self::InvokeAsync => 202,
            Self::GetFunction => 200,
        }
    }

    /// Make the call against the function `name`.
    async fn send(self, router: &smithy_server_runtime::MultiServiceRouter, name: &str) -> Answer {
        let (method, uri) = self.request(name);
        let (content_type, body, headers) = match self {
            // An asynchronous invocation, so a served row is answered the queueing receipt
            // rather than whatever a run comes to.
            Self::Invoke => (
                "application/octet-stream",
                br#"{"event":1}"#.to_vec(),
                vec![("x-amz-invocation-type", "Event".to_string())],
            ),
            Self::InvokeAsync => (
                "application/octet-stream",
                br#"{"event":1}"#.to_vec(),
                Vec::new(),
            ),
            Self::GetFunction => ("application/json", Vec::new(), Vec::new()),
        };
        send(router, method, &uri, content_type, body, &headers).await
    }

    /// What a failing assertion calls this operation.
    fn named(self) -> &'static str {
        match self {
            Self::Invoke => "Invoke",
            Self::InvokeAsync => "InvokeAsync",
            Self::GetFunction => "GetFunction",
        }
    }
}

/// Every operation the rows are driven through.
const CALLED: &[Called] = &[Called::Invoke, Called::InvokeAsync, Called::GetFunction];

/// One inadmissible value: what a failing assertion calls it, the value as submitted, and
/// every requirement it breaks.
struct Refused {
    case: &'static str,
    value: String,
    broken: Vec<Requirement>,
}

/// The values outside the member's own constraints, spanning the forms and the constraints
/// described in this module's header.
fn refused_values() -> Vec<Refused> {
    vec![
        Refused {
            case: "whitespace alone",
            value: "     ".to_string(),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "one space",
            value: " ".to_string(),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "a tab",
            value: "\t".to_string(),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "a newline",
            value: "\n".to_string(),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "a space inside an otherwise admissible name",
            value: "my function".to_string(),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "punctuation outside the member's character class",
            value: "bad name!".to_string(),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "a non-ASCII letter",
            value: "préfix".to_string(),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "an admissible name with an inadmissible qualifier",
            value: format!("{FUNCTION}:$NEWEST"),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "the ARN form with a space in the function's name",
            value: format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:bad name"),
            broken: vec![Requirement::Pattern],
        },
        Refused {
            case: "one character past the member's modelled maximum",
            value: "a".repeat(MAX_NAME_LENGTH + 1),
            broken: vec![Requirement::Length],
        },
        Refused {
            case: "past the maximum AND outside the pattern",
            value: format!("{}!", "a".repeat(MAX_NAME_LENGTH)),
            broken: vec![Requirement::Length, Requirement::Pattern],
        },
    ]
}

/// RULE: every value outside the member's own modelled constraints is refused 400
/// `ValidationException` with AWS's envelope over one clause per constraint it broke, quoting
/// the value as submitted and naming the member — whichever of the operations binding that
/// member the request was made as.
#[tokio::test]
async fn a_function_name_outside_its_modelled_constraints_is_refused_with_aws_clauses() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_active_function(&router, &engine, FUNCTION, &package_bytes(48)).await;

    for refused in refused_values() {
        for called in CALLED {
            let answer = called.send(&router, &refused.value).await;
            let body = String::from_utf8_lossy(&answer.body).to_string();
            assert_eq!(
                (answer.status, answer.error_code()),
                (400, VALIDATION_EXCEPTION.to_string()),
                "case {:?} ({}): expected 400 {VALIDATION_EXCEPTION}, got {} {body}",
                refused.case,
                called.named(),
                answer.status
            );
            let reported = answer.string("message").unwrap_or_default();
            let expected_envelope = envelope(refused.broken.len());
            assert!(
                reported.starts_with(&expected_envelope),
                "case {:?} ({}): expected the message to open with {expected_envelope:?}, got \
                 {reported:?}",
                refused.case,
                called.named()
            );
            for requirement in &refused.broken {
                let clause = requirement.clause(&refused.value);
                assert!(
                    reported.contains(&clause),
                    "case {:?} ({}): expected the clause {clause:?}, got {reported:?}",
                    refused.case,
                    called.named()
                );
            }
            // Exactly the clauses the value broke and no others: a single-violation refusal is
            // the whole message, so it is compared in full.
            if refused.broken.len() == 1 {
                assert_eq!(
                    reported,
                    format!(
                        "{expected_envelope}{}",
                        refused.broken[0].clause(&refused.value)
                    ),
                    "case {:?} ({}): expected the envelope and that one clause alone, got \
                     {reported:?}",
                    refused.case,
                    called.named()
                );
            }
        }
    }
}

/// RULE: the refusal is the boundary's, so it is answered BEFORE anything is looked up — an
/// inadmissible value is refused 400 `ValidationException` even on a store holding no function
/// at all, while a value the constraints DO admit gets the operation's own not-found answer.
#[tokio::test]
async fn an_inadmissible_function_name_is_refused_before_any_function_is_looked_up() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for refused in refused_values() {
        for called in CALLED {
            let answer = called.send(&router, &refused.value).await;
            assert_eq!(
                (answer.status, answer.error_code()),
                (400, VALIDATION_EXCEPTION.to_string()),
                "case {:?} ({}): expected the constraint refusal on an empty store, got {} {}",
                refused.case,
                called.named(),
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
        }
    }

    // The other side of the ordering: a value the constraints admit is looked up, and this
    // store holds nothing under it.
    for called in CALLED {
        let answer = called.send(&router, "no-such-function").await;
        assert_eq!(
            (answer.status, answer.error_code()),
            (404, NOT_FOUND_EXCEPTION.to_string()),
            "case {:?}: expected 404 {NOT_FOUND_EXCEPTION} for an admissible name no function \
             is held under, got {} {}",
            called.named(),
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
    }
}

/// RULE: the constraints decide which values are refused and nothing more — every documented
/// form of the member reaches the operation and is served, so the refusal above is about the
/// value and not about the member being read at all.
#[tokio::test]
async fn every_documented_function_name_form_still_reaches_the_operation() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_active_function(&router, &engine, FUNCTION, &package_bytes(48)).await;

    // The forms `NamespacedFunctionName` documents: the bare name, the name with the
    // unpublished version's qualifier, the `function:` prefix form, the partial
    // `<account>:function:<name>` form and the full ARN, qualified and not.
    let admitted = [
        ("the bare name", FUNCTION.to_string()),
        (
            "the name with the unpublished version's qualifier",
            format!("{FUNCTION}:$LATEST"),
        ),
        ("the function: prefix form", format!("function:{FUNCTION}")),
        (
            "the partial <account>:function:<name> form",
            format!("{ACCOUNT}:function:{FUNCTION}"),
        ),
        (
            "the full ARN",
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{FUNCTION}"),
        ),
        (
            "the full ARN with a qualifier",
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{FUNCTION}:$LATEST"),
        ),
    ];

    for (case, value) in admitted {
        for called in CALLED {
            let answer = called.send(&router, &value).await;
            assert_eq!(
                answer.status,
                called.served_status(),
                "case {case:?} ({}): expected HTTP {} for a documented form, got {} {}",
                called.named(),
                called.served_status(),
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
        }
    }
}
