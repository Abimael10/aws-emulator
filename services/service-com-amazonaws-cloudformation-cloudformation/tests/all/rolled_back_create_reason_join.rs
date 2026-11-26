// SPDX-License-Identifier: CC0-1.0

//! How the stack-level reason of a create CloudFormation ROLLED BACK joins the failure's own
//! sentence to the clause saying the rollback was asked for, end to end over the awsQuery wire.
//!
//! RULE under test: **the `ResourceStatusReason` of a rolled-back create's own stack-level
//! event (`LogicalResourceId` = the stack) is the failure's own sentence reproduced verbatim,
//! then ONE space, then the fixed clause `Rollback requested by user.` — CloudFormation adds
//! no punctuation of its own, so a sentence that already ends with its period is followed by
//! the clause directly and never by a second period, and a sentence that leaves its separating
//! space keeps exactly that one space.** For the export-name uniqueness refusal the sentence
//! is `Export with name <ExportName> is already exported by stack <ExportingStackName>.`, whose
//! only variable parts are the name the submitted template declares and the name of the stack
//! already publishing it; the stack's own `StackStatusReason` carries that sentence ALONE,
//! since the clause belongs to the event that reports the rollback.
//!
//! Why it matters: deployment tooling (and an operator reading the console) classifies a
//! rollback by matching AWS's exact sentence and pulls the owning stack's name out of it, so a
//! reason carrying a doubled period is reported as an unknown rollback cause and the stack
//! holding the name never reaches the user.
//!
//! The rows differ along what the rule generalises over: the CLASS of failure whose sentence
//! the clause follows — one CloudFormation words itself and terminates with a period (the
//! export-name collision, an `Fn::ImportValue` naming an export nothing publishes), one it
//! leaves open with its separating space (the summary of the resources an owning service
//! refused) — and, for the collision, its two variable parts: the form of the export name and
//! the name of the stack that holds it. Every sentence is spelled out HERE rather than read off
//! the emulator's own formatter, so what the assertion pins is the wording and not the
//! implementation's agreement with itself.
//!
//! What this suite OWNS is that whole joined reason, read off the wire: the two period-terminated
//! classes are the ones no suite pinned without going through the emulator's own formatter, and
//! the space-terminated row is kept as the one contrast case — `stack_failure_reason.rs` owns the
//! resource-failure summary itself and its rolled-back form, and `unresolvable_imports.rs` the
//! unresolvable import's own sentence.

use crate::event_detailed_status::REFUSED_TOPIC_TEMPLATE;
use crate::harness::{create_stack, router, settled_stack, stack_own_events, xml_text, REGION};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The clause CloudFormation closes a rolled-back create's stack-level reason with, as AWS
/// words it.
const ROLLBACK_CLAUSE: &str = "Rollback requested by user.";

/// The logical id [`REFUSED_TOPIC_TEMPLATE`] declares its refused topic under, which the
/// resource-failure summary of that row names.
const REFUSED_LOGICAL_ID: &str = "Notifications";

/// A template publishing one exported output value under `export_name`, over a resource no
/// owning service has to provision.
fn exporting(export_name: &str) -> String {
    format!(
        r#"{{"Resources":{{"Handle":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
            "Outputs":{{"Published":{{"Value":"a published value",
              "Export":{{"Name":"{export_name}"}}}}}}}}"#
    )
}

/// A template whose output imports `export_name`.
fn importing(export_name: &str) -> String {
    format!(
        r#"{{"Resources":{{"Handle":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
            "Outputs":{{"Imported":{{"Value":{{"Fn::ImportValue":"{export_name}"}}}}}}}}"#
    )
}

/// The sentence CloudFormation words for a deployment that would publish an export name
/// another live stack of the Region already publishes: fixed text around the two variable
/// parts.
fn already_exported(export_name: &str, exporting_stack: &str) -> String {
    format!("Export with name {export_name} is already exported by stack {exporting_stack}.")
}

/// One row: the stack that must already publish an export name (for the collision rows), the
/// stack whose create fails, its template and the sentence that failure words.
struct Rolled {
    case: &'static str,
    /// `(stack name, export name it publishes)`, for a row whose failure is the collision.
    holder: Option<(&'static str, &'static str)>,
    stack: &'static str,
    template: String,
    /// The failure's own sentence as AWS words it, INCLUDING the separating space where that
    /// sentence leaves one.
    sentence: String,
}

/// The rows: the three failure classes, and the collision over three forms of its two
/// variable parts.
fn rows() -> Vec<Rolled> {
    vec![
        Rolled {
            case: "an export named after the stack that holds it",
            holder: Some(("join-kb-stack", "join-kb-stack-MetadataKnowledgeBaseId")),
            stack: "join-kb-second",
            template: exporting("join-kb-stack-MetadataKnowledgeBaseId"),
            sentence: already_exported("join-kb-stack-MetadataKnowledgeBaseId", "join-kb-stack"),
        },
        Rolled {
            case: "a one-character export name, held by a stack whose name ends in a digit",
            holder: Some(("join-holder-2", "e")),
            stack: "join-second-2",
            template: exporting("e"),
            sentence: already_exported("e", "join-holder-2"),
        },
        Rolled {
            case: "a mixed-case export name carrying digits and consecutive hyphens",
            holder: Some(("join-holder-three", "Join--Export--09-NAME")),
            stack: "join-second-three",
            template: exporting("Join--Export--09-NAME"),
            sentence: already_exported("Join--Export--09-NAME", "join-holder-three"),
        },
        Rolled {
            case: "another sentence of CloudFormation's own: an import nothing publishes",
            holder: None,
            stack: "join-missing-import",
            template: importing("join-never-published"),
            sentence: "No export named join-never-published found.".to_string(),
        },
        Rolled {
            case: "a sentence that leaves its separating space: a refused resource's summary",
            holder: None,
            stack: "join-refused-topic",
            template: REFUSED_TOPIC_TEMPLATE.to_string(),
            sentence: format!(
                "The following resource(s) failed to create: [{REFUSED_LOGICAL_ID}]. "
            ),
        },
    ]
}

#[tokio::test]
async fn a_rolled_back_create_joins_the_failure_and_the_clause_with_one_space() {
    for row in rows() {
        let case = row.case;
        let router = router();

        // The stack that HOLDS the export name, for the rows whose failure is the collision:
        // the name it publishes is what the create below cannot have.
        if let Some((holder, export_name)) = row.holder {
            let (code, body) = create_stack(&router, holder, &exporting(export_name), &[]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: CreateStack({holder}) must be accepted, got {code} {body}"
            );
            let described = settled_stack(case, &router, REGION, holder).await;
            assert_eq!(
                xml_text(&described, "StackStatus").as_deref(),
                Some(status::CREATE_COMPLETE),
                "case {case:?}: the holding stack must complete, got {described}"
            );
        }

        let (code, body) = create_stack(&router, row.stack, &row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({}) must be accepted — the request is valid and the \
             deployment is what fails — got {code} {body}",
            row.stack
        );
        // Polled for: the failure and the rollback it owes are each due at an instant this
        // service records.
        let described = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the failed create to roll back, got {described}"
        );
        // The stack's own field carries the failure ALONE: the clause is the rollback event's.
        let status_reason = xml_text(&described, "StackStatusReason").unwrap_or_default();
        assert_eq!(
            status_reason, row.sentence,
            "case {case:?}: expected the stack to report {:?}, got {status_reason:?}",
            row.sentence
        );

        let reasons: Vec<String> = stack_own_events(&router, case, row.stack)
            .await
            .into_iter()
            .map(|event| event.reason)
            .filter(|reason| !reason.is_empty())
            .collect();
        // The whole reason a client classifying the rollback matches: the sentence, one
        // space, the clause.
        let expected = format!("{} {ROLLBACK_CLAUSE}", row.sentence.trim_end());
        assert!(
            reasons.contains(&expected),
            "case {case:?}: expected a stack-level reason {expected:?}, got {reasons:?}"
        );
        let carrying: Vec<&String> = reasons
            .iter()
            .filter(|reason| reason.contains(ROLLBACK_CLAUSE))
            .collect();
        assert_eq!(
            carrying.len(),
            1,
            "case {case:?}: expected exactly one stack-level reason to carry the clause, got \
             {reasons:?}"
        );
        let reason = carrying[0];
        // ...and nothing of CloudFormation's own went in beside that one space: what stands
        // ahead of the clause is the failure's sentence, character for character.
        let joined = format!(" {ROLLBACK_CLAUSE}");
        let ahead = reason.strip_suffix(joined.as_str()).unwrap_or_else(|| {
            panic!("case {case:?}: expected the clause after a single space, got {reason:?}")
        });
        assert_eq!(
            ahead,
            row.sentence.trim_end(),
            "case {case:?}: expected the failure's own sentence ahead of the clause, got \
             {ahead:?}"
        );
        assert!(
            !reason.contains(".."),
            "case {case:?}: expected no doubled period in the reason, got {reason:?}"
        );
    }
}
