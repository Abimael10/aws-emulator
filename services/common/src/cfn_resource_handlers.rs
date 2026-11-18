// SPDX-License-Identifier: CC0-1.0

//! What the RESOURCE PROVIDER that owns a CloudFormation resource type reports for a resource
//! it cannot find.
//!
//! ## The rule this module implements
//!
//! A resource type in the CloudFormation registry is acted on through its resource provider:
//! the handler package whose `create`, `read`, `update` and `delete` handlers are called to
//! make, report and remove a resource of that type. So when an API reports that a resource of
//! some type is NOT THERE, the diagnostic inside that report is the HANDLER's, not the calling
//! API's: the handler of one type says a table does not exist, another's says an alarm does
//! not exist, and a type whose handler reports through the generic resource-provider base
//! class states nothing but the type and the identifier, which is the uniform sentence
//! ([`uniform_not_found`]).
//!
//! Two APIs act through those same handlers and therefore report the same diagnostic, so it
//! lives here once: Cloud Control API is a facade over a type's handlers and refuses an action
//! against a resource the handler could not read, and CloudFormation calls the same handler
//! while planning an IMPORT of an existing resource, failing the change set when the resource
//! the import names is not there. Held apart, one API could report a handler's own wording for
//! a type while the other reported the uniform sentence for it.
//!
//! The catalog holds only the types whose handler states a sentence of its OWN
//! ([`NOT_FOUND_DIAGNOSTICS`]); every other type answers `None` and is reported in the uniform
//! sentence by whoever asked.

/// One resource type's own not-found diagnostic — what the handler that owns the type states
/// for a resource it cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotFoundDiagnostic {
    /// The sentence that handler states, with the resource it is about named wherever the
    /// wording names it ([`IDENTIFIER_PLACEHOLDER`] filled in from the request).
    pub sentence: String,
    /// The `Service` the owning handler's client is for, where that handler carries its
    /// client's rendering of the service's exception rather than the bare sentence — the
    /// envelope an API that reports a whole handler invocation puts around it.
    pub sdk_service: Option<&'static str>,
}

/// One catalogued diagnostic, as it is written down: the sentence with its placeholder still
/// in it.
struct CataloguedDiagnostic {
    sentence: &'static str,
    sdk_service: Option<&'static str>,
}

/// Where a catalogued sentence names the resource the diagnostic is about.
const IDENTIFIER_PLACEHOLDER: &str = "{Identifier}";

/// What the handler that owns a resource type states for a resource it cannot find, keyed by
/// `TypeName` — the registry's own key.
///
/// Each sentence is the one RECORDED for that type's handler, which is not always how the
/// owning service words the same absence on its own wire: the `AWS::S3::Bucket` handler states
/// "Bucket not found" where `s3:HeadBucket` answers `NoSuchBucket` "The specified bucket does
/// not exist", and the `AWS::DynamoDB::Table` handler states "Table: <name> does not exist."
/// where `dynamodb:DescribeTable` answers "Requested resource not found". The handler's own
/// sentence is what a client of an API acting through that handler reads, so it is what this
/// catalog holds; only the resource's name inside it comes from the request
/// ([`IDENTIFIER_PLACEHOLDER`]).
///
/// A type absent from here is reported in the uniform sentence ([`uniform_not_found`]), which
/// is what a handler built on the generic resource-provider base class states.
const NOT_FOUND_DIAGNOSTICS: &[(&str, CataloguedDiagnostic)] = &[
    (
        "AWS::CloudWatch::Alarm",
        CataloguedDiagnostic {
            sentence: "Alarm does not exist",
            sdk_service: None,
        },
    ),
    (
        "AWS::DynamoDB::Table",
        CataloguedDiagnostic {
            sentence: "Table: {Identifier} does not exist.",
            sdk_service: None,
        },
    ),
    (
        "AWS::S3::Bucket",
        CataloguedDiagnostic {
            sentence: "Bucket not found",
            sdk_service: None,
        },
    ),
    (
        "AWS::SNS::Topic",
        CataloguedDiagnostic {
            sentence: "Topic does not exist",
            sdk_service: Some("Sns"),
        },
    ),
];

/// The diagnostic the handler that owns `type_name` states for the resource `identifier` names,
/// or `None` for a type whose handler states no sentence of its own — one reporting through the
/// generic resource-provider base class, whose not-found names nothing but the type and the
/// identifier ([`uniform_not_found`]).
pub fn not_found_diagnostic(type_name: &str, identifier: &str) -> Option<NotFoundDiagnostic> {
    let (_, catalogued) = NOT_FOUND_DIAGNOSTICS
        .iter()
        .find(|(name, _)| *name == type_name)?;
    Some(NotFoundDiagnostic {
        // The sentence names the resource wherever the owning handler's wording does, with
        // the identifier the caller named.
        sentence: catalogued
            .sentence
            .replace(IDENTIFIER_PLACEHOLDER, identifier),
        sdk_service: catalogued.sdk_service,
    })
}

/// The SENTENCE that handler states, for a caller that reports the diagnostic on its own — with
/// no handler-invocation envelope of its own to put a client-rendered exception in.
pub fn not_found_sentence(type_name: &str, identifier: &str) -> Option<String> {
    not_found_diagnostic(type_name, identifier).map(|diagnostic| diagnostic.sentence)
}

/// The UNIFORM not-found sentence — what a handler built on the generic resource-provider base
/// class states, naming nothing but the type and the identifier the caller addressed the
/// resource by.
///
/// The fallback for every type the catalog above does not cover, and the sentence both APIs
/// report such a type's absence in.
pub fn uniform_not_found(type_name: &str, identifier: &str) -> String {
    format!("Resource of type '{type_name}' with identifier '{identifier}' was not found.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a catalogued type answers its handler's own sentence, with the identifier the
    /// caller named filled in wherever that wording names the resource; every other type
    /// answers `None`, so its absence is reported in the uniform sentence.
    #[test]
    fn a_catalogued_type_answers_its_own_handlers_sentence() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "AWS::DynamoDB::Table",
                "orders",
                Some("Table: orders does not exist."),
            ),
            // A type whose sentence names the resource nowhere: the identifier changes
            // nothing about it.
            (
                "AWS::CloudWatch::Alarm",
                "cpu-high",
                Some("Alarm does not exist"),
            ),
            ("AWS::S3::Bucket", "assets", Some("Bucket not found")),
            ("AWS::SNS::Topic", "events", Some("Topic does not exist")),
            // A type reported through the generic base class, a type of no catalogued
            // service, and a value that is no type name at all.
            ("AWS::SSM::Parameter", "/held", None),
            ("AWS::SQS::Queue", "jobs", None),
            ("My::Sample::Resource", "sample", None),
            ("", "", None),
        ];
        for (type_name, identifier, expected) in rows {
            let sentence = not_found_sentence(type_name, identifier);
            assert_eq!(
                sentence.as_deref(),
                *expected,
                "case {type_name:?}/{identifier:?}: expected {expected:?}, got {sentence:?}"
            );
        }
    }

    /// RULE: the client-exception envelope belongs to the handler, not to the caller — the
    /// `Service` it names travels with the diagnostic, and only for the types whose handler
    /// carries its client's rendering.
    #[test]
    fn the_client_rendering_travels_with_the_diagnostic() {
        let rows: &[(&str, Option<&str>)] = &[
            ("AWS::SNS::Topic", Some("Sns")),
            ("AWS::DynamoDB::Table", None),
            ("AWS::CloudWatch::Alarm", None),
            ("AWS::S3::Bucket", None),
        ];
        for (type_name, expected) in rows {
            let diagnostic = not_found_diagnostic(type_name, "named");
            let sdk_service = diagnostic.as_ref().and_then(|found| found.sdk_service);
            assert_eq!(
                sdk_service, *expected,
                "case {type_name:?}: expected {expected:?}, got {sdk_service:?}"
            );
        }
    }

    /// RULE: the uniform sentence names the type and the identifier, and nothing else.
    #[test]
    fn the_uniform_sentence_names_the_type_and_the_identifier() {
        let rows: &[(&str, &str, &str)] = &[
            (
                "AWS::SSM::Parameter",
                "/import/absent",
                "Resource of type 'AWS::SSM::Parameter' with identifier '/import/absent' was \
                 not found.",
            ),
            (
                "AWS::SQS::Queue",
                "jobs",
                "Resource of type 'AWS::SQS::Queue' with identifier 'jobs' was not found.",
            ),
        ];
        for (type_name, identifier, expected) in rows {
            let sentence = uniform_not_found(type_name, identifier);
            assert_eq!(
                sentence, *expected,
                "case {type_name:?}/{identifier:?}: expected {expected:?}, got {sentence:?}"
            );
        }
    }

    /// The catalog carries each type once, so no lookup depends on which entry comes first.
    #[test]
    fn the_catalogue_names_each_type_once() {
        let mut seen: Vec<&str> = Vec::new();
        for (type_name, _) in NOT_FOUND_DIAGNOSTICS {
            assert!(
                !seen.contains(type_name),
                "case {type_name:?}: the catalogue carries it twice"
            );
            seen.push(type_name);
        }
    }
}
