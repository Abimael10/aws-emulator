// SPDX-License-Identifier: CC0-1.0

//! The WORDING `GetRoleTemplateVersion` refuses a malformed `TemplateArn` with, end to end
//! over the awsQuery wire.
//!
//! RULE: a value supplied for a member documented as the ARN of a role template is refused as
//! a value the service cannot accept (HTTP 400, awsQuery code `ValidationError`) with the
//! sentence belonging to the CLASS of defect the value carries, and that sentence is FIXED
//! text: it is the same for every value of its class and quotes nothing of the request. Three
//! classes: a value nothing can be read as an ARN from, an ARN that does name a role template
//! of IAM's but under an account rather than the `aws` publisher, and any other departure from
//! the form.
//!
//! `tests/all/role_template_version_reads.rs` already enumerates one value per way the form
//! can be wrong; what this suite adds is the INVARIANT above that enumeration: for a FAMILY of
//! values generated inside one class — varying the partition, the account field, the publishing
//! service, the template name, the major version, the Region field and the length, on each
//! published bound of `arnType` (20..2048) the class can reach — the refusal sentence does not
//! vary with the value and contains none of the value's variable parts. That is what lets a
//! client branch on the text to tell "this is not an ARN" from "this ARN is in the wrong
//! namespace", which a sentence that echoed the value or collapsed the two classes would not.
//!
//! The sentences are written out here rather than read from the implementation's constants, so
//! a reworded constant fails here instead of agreeing with itself — the reason that sibling
//! suite states for keeping its own copy too.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns. Every row drives the
//! same router over one shared engine, as the production router does.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{account_id, call, element, error_code, router};

/// The published bounds of `arnType`, the type `TemplateArn` is carried through: "len
/// 20..2048" (`smithy_query(query="operation", operation="GetRoleTemplateVersion")`), so a row
/// may sit on either bound but never outside them.
const MIN_ARN_CHARACTERS: usize = 20;
const MAX_ARN_CHARACTERS: usize = 2048;

/// What a value nothing can be read as an ARN from is refused with: IAM's own unparseable-ARN
/// sentence, whose empty value slot renders as the two spaces after the colon.
const ARN_NOT_PARSEABLE: &str = "Invalid ARN:  Could not be parsed!";

/// What an ARN naming a role template under an account rather than the `aws` publisher is
/// refused with — fixed text, the example ARN included, quoting neither the account field nor
/// the value.
const TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS: &str =
    "The specified role template ARN is not valid. The ARN must use 'aws' as the account \
     field (e.g., arn:aws:iam::aws:role-template/...).";

/// What any other departure from the form is refused with: the sentence stating the whole
/// form, whose braced tokens are the sentence's own placeholder text.
const TEMPLATE_ARN_NOT_VALID: &str =
    "The specified role template ARN is not valid. Expected format is: \
     arn:{partition}:iam::aws:role-template/{servicePrincipal}/{templateName}:{majorVersion} \
     (majorVersion must be a positive integer with no leading zeros).";

/// A published template every account can read, used as the positive control each test ends
/// on: the refusals must come from reading the VALUE, not from a reader that refuses
/// everything.
///
/// SOURCE: `state::PUBLISHED_ROLE_TEMPLATES`, as `PublishedRoleTemplate::arn` spells an entry.
const PUBLISHED_TEMPLATE_ARN: &str =
    "arn:aws:iam::aws:role-template/iam.amazonaws.com/PowerUserRoleTemplate:1";
const PUBLISHED_TEMPLATE_NAME: &str = "PowerUserRoleTemplate";

/// The shortest part of a value the echo check below reads as quoted back. A part shorter
/// than this says nothing: a single character or two can occur inside a sentence of ordinary
/// English for reasons that have nothing to do with the request, so only the parts long enough
/// to identify the value are checked.
const SHORTEST_IDENTIFYING_PART: usize = 4;

/// One generated value, the sentence its class earns, and the parts of the value that the
/// refusal must NOT quote back.
struct Row {
    case: String,
    template_arn: String,
    expected: &'static str,
    /// The variable parts of this value: a refusal that is fixed text carries none of them.
    /// Parts shorter than [`SHORTEST_IDENTIFYING_PART`] are skipped, so the check never rests
    /// on a character a sentence may legitimately contain.
    not_echoed: Vec<String>,
}

/// One `GetRoleTemplateVersion` call.
async fn read(router: &MultiServiceRouter, template_arn: &str) -> (u16, String) {
    call(
        router,
        "GetRoleTemplateVersion",
        &[("TemplateArn", template_arn)],
    )
    .await
}

/// Assert `rows` are each refused 400 `ValidationError` with the one sentence their class
/// earns, quoting nothing of the value.
async fn assert_refused(rows: Vec<Row>) {
    let router = router();
    for row in rows {
        let Row {
            case,
            template_arn,
            expected,
            not_echoed,
        } = row;
        let length = template_arn.chars().count();
        assert!(
            (MIN_ARN_CHARACTERS..=MAX_ARN_CHARACTERS).contains(&length),
            "case {case:?}: the row must sit inside the published bounds of arnType \
             ({MIN_ARN_CHARACTERS}..={MAX_ARN_CHARACTERS}), got {length}"
        );

        let (status, body) = read(&router, &template_arn).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the value to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationError",
            "case {case:?}: expected ValidationError, got {:?}: {body}",
            error_code(&body)
        );
        let message = element(&body, "Message");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal to read {expected:?}, got {message:?}"
        );
        for part in not_echoed {
            if part.chars().count() < SHORTEST_IDENTIFYING_PART {
                continue;
            }
            assert!(
                !message.contains(&part),
                "case {case:?}: the refusal must be fixed text, but it quotes {part:?}: \
                 {message:?}"
            );
        }
    }

    // The positive control: the same reader still serves a template it resolves.
    let (status, body) = read(&router, PUBLISHED_TEMPLATE_ARN).await;
    assert_eq!(
        status, 200,
        "the positive control must still be served, got {status}: {body}"
    );
    assert_eq!(
        element(&body, "TemplateName"),
        PUBLISHED_TEMPLATE_NAME,
        "the positive control must report {PUBLISHED_TEMPLATE_NAME:?}, got {:?}",
        element(&body, "TemplateName")
    );
}

/// `before`, then `after`, with the single character `filler` repeated between them until the
/// whole value is exactly `MAX_ARN_CHARACTERS` long — so a row can sit on the published
/// maximum of `arnType` without the length being written out.
fn at_maximum_length(before: &str, filler: char, after: &str) -> String {
    let fixed = before.chars().count() + after.chars().count();
    assert!(
        fixed < MAX_ARN_CHARACTERS,
        "{before:?} and {after:?} leave no room for the filler inside {MAX_ARN_CHARACTERS} \
         characters"
    );
    let padding = String::from(filler).repeat(MAX_ARN_CHARACTERS - fixed);
    format!("{before}{padding}{after}")
}

/// RULE (class 1): a value nothing can be read as an ARN from is refused with the one
/// unparseable-ARN sentence, whatever the value is — the sentence has no slot for it.
///
/// Rows vary along what the class generalises over: how the value departs from an ARN (plain
/// text, a wrong leading field, the right fields in the wrong case, too few fields, separators
/// alone, a value that merely contains an ARN), and its length, at both published bounds of
/// `arnType`.
#[tokio::test]
async fn an_unreadable_template_arn_is_refused_with_the_unparseable_arn_sentence() {
    let shortest = "x".repeat(MIN_ARN_CHARACTERS);
    let longest = at_maximum_length("", 'y', "");

    let values = [
        ("plain text that is no ARN", "not-a-valid-arn-shape".to_string()),
        (
            "a value whose leading field is not arn",
            format!("urn:aws:iam::aws:role-template/iam.amazonaws.com/{PUBLISHED_TEMPLATE_NAME}:1"),
        ),
        (
            "an otherwise well-formed ARN whose leading field is upper case",
            format!("ARN:aws:iam::aws:role-template/iam.amazonaws.com/{PUBLISHED_TEMPLATE_NAME}:1"),
        ),
        (
            "a value carrying fewer fields than an ARN has",
            "arn:aws:iam:ap-southeast-2".to_string(),
        ),
        (
            "a value carrying every field of an ARN but no resource",
            format!("arn:aws:iam::{}:", account_id()),
        ),
        (
            "a value of separators alone",
            "/////////////////////".to_string(),
        ),
        (
            "a value that only contains an ARN",
            format!("TemplateArn={PUBLISHED_TEMPLATE_ARN}"),
        ),
        ("a value at the published minimum length", shortest),
        ("a value at the published maximum length", longest),
    ];

    let rows = values
        .into_iter()
        .map(|(case, template_arn)| Row {
            case: case.to_string(),
            not_echoed: vec![template_arn.clone()],
            template_arn,
            expected: ARN_NOT_PARSEABLE,
        })
        .collect();

    assert_refused(rows).await;
}

/// RULE (class 2): an ARN that IS a role template's, but whose account field carries an account
/// rather than the `aws` publisher every role template lives under, is in the wrong namespace
/// rather than of the wrong shape, and is refused with the namespace sentence — fixed text,
/// quoting neither the account nor the ARN.
///
/// Rows vary along what the class generalises over: which account the field names (the caller's
/// own, another account's, an account with no number at all, another publisher's name), the
/// partition, whether the ARN names a Region, the publishing service and template name the
/// identity spells, the major version, and the length, up to the published MAXIMUM of `arnType`
/// — the minimum is out of this class's reach, because the shortest value that reads as a role
/// template's ARN under an account is already longer than 20 characters.
#[tokio::test]
async fn a_template_arn_in_an_accounts_namespace_is_refused_with_the_namespace_sentence() {
    // (case, partition, region, account, publishing service, template name, major version)
    let values = [
        (
            "the caller's own account",
            "aws",
            "",
            account_id().to_string(),
            "deploy.amazonaws.com",
            "ReadFixture",
            "1",
        ),
        (
            "another account's number",
            "aws",
            "",
            "000000000000".to_string(),
            "iam.amazonaws.com",
            PUBLISHED_TEMPLATE_NAME,
            "1",
        ),
        (
            "an account field of a single digit in another partition",
            "aws-cn",
            "",
            "1".to_string(),
            "backup.amazonaws.com",
            "AWSBackupDefaultServiceRoleTemplate",
            "7",
        ),
        (
            "another publisher's name in the account field, with a Region named too",
            "aws-us-gov",
            "us-gov-west-1",
            "amazonaws".to_string(),
            "lambda.amazonaws.com",
            "Template-With.Punctuation_1",
            "42",
        ),
    ];

    let mut rows: Vec<Row> = values
        .into_iter()
        .map(
            |(case, partition, region, account, publisher, name, major_version)| Row {
                case: format!("a role-template ARN naming {case}"),
                template_arn: format!(
                    "arn:{partition}:iam:{region}:{account}:role-template/{publisher}/{name}:{major_version}"
                ),
                expected: TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS,
                // Everything the caller chose, the whole value included: a fixed sentence
                // quotes none of it.
                not_echoed: vec![
                    format!(
                        "arn:{partition}:iam:{region}:{account}:role-template/{publisher}/{name}:{major_version}"
                    ),
                    account,
                    publisher.to_string(),
                    name.to_string(),
                ],
            },
        )
        .collect();

    // The same class at the published maximum length of `arnType`, so a long value is told
    // about its namespace rather than about its shape.
    let longest = at_maximum_length(
        &format!(
            "arn:aws:iam::{}:role-template/deploy.amazonaws.com/",
            account_id()
        ),
        'N',
        ":1",
    );
    rows.push(Row {
        case: "a role-template ARN of this account at the published maximum length".to_string(),
        expected: TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS,
        not_echoed: vec![longest.clone(), account_id().to_string()],
        template_arn: longest,
    });

    assert_refused(rows).await;
}

/// RULE (class 3): the two classes above are DISTINCT from the departure-from-the-form class,
/// so a value that is readable as an ARN and is not a role template's under an account answers
/// the sentence stating the whole form — which is what keeps the three sentences
/// distinguishable to a caller.
///
/// Rows vary along what the class generalises over: another resource type of IAM's own, a
/// Region on a publisher's own ARN, and a major version that is not a positive integer without
/// leading zeros — each of them under the `aws` publisher, so what the row isolates is the
/// form. The sibling table (`role_template_version_reads.rs`) enumerates the class in full;
/// these rows are here only to hold the sentence DISTINCT from the two above.
#[tokio::test]
async fn a_template_arn_of_another_form_is_refused_with_the_form_sentence() {
    let values = [
        (
            "another resource type of IAM's own",
            "arn:aws:iam::aws:policy/PowerUserAccess".to_string(),
        ),
        (
            "a publisher's own ARN naming a Region",
            format!(
                "arn:aws:iam:ap-southeast-2:aws:role-template/iam.amazonaws.com/{PUBLISHED_TEMPLATE_NAME}:1"
            ),
        ),
        (
            "a major version written with leading zeros",
            format!(
                "arn:aws:iam::aws:role-template/iam.amazonaws.com/{PUBLISHED_TEMPLATE_NAME}:007"
            ),
        ),
    ];

    let rows = values
        .into_iter()
        .map(|(case, template_arn)| Row {
            case: format!("a template ARN naming {case}"),
            not_echoed: vec![template_arn.clone()],
            template_arn,
            expected: TEMPLATE_ARN_NOT_VALID,
        })
        .collect();

    assert_refused(rows).await;
}
