// SPDX-License-Identifier: CC0-1.0

//! The FORM a CodeBuild resource policy is reported in, driven end to end over the wire the
//! router serves: `POST /` with `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1.
//!
//! THE RULE: `PutResourcePolicy` takes the policy as a string
//! (`smithy_query(query="operation",operation="PutResourcePolicy,GetResourcePolicy")`:
//! `policy` targets `NonEmptyString` on both sides), and what a later `GetResourcePolicy`
//! reports is the service's own rendering of the IAM (Aspen) policy the caller submitted, not
//! the bytes the request spelled. Three things change, and nothing else:
//!
//! 1. the whitespace BETWEEN the document's JSON tokens is gone — no space after a `:` or a
//!    `,`, no indentation, no newlines, whatever spacing the request used;
//! 2. an element whose value the policy language types as "String or Array of strings" — a
//!    statement's `Action`/`NotAction` and `Resource`/`NotResource`, and the principal lists
//!    inside `Principal`/`NotPrincipal` (`AWS`, `Service`, `Federated`, `CanonicalUser`) — is
//!    written as the STRING when the caller submitted a one-element array holding one string;
//!    and
//! 3. an ACCOUNT principal named by the account's id alone is written as that account's ROOT
//!    ARN (`arn:aws:iam::<account id>:root`), the other spelling of the very same principal.
//!
//! Everything else is the caller's own data and survives untouched: member order, statement
//! count, the `Statement` array itself (a one-statement policy keeps its one-element array),
//! the type and spelling of every other value, and every byte inside a string literal —
//! whitespace, escapes, non-ASCII characters and text that merely LOOKS like a policy element
//! included.
//!
//! The rendering is also a FIXED POINT: submitting what the service reported stores the
//! identical document, so a client that re-applies the policy it read sees no drift.
//!
//! The table below is the rule's input class; the recorded case (a document with a space
//! after every `:` and `,`, a one-element `Action` and a one-element `AWS` principal naming
//! an account by its id) is one of its rows. The rule holds for both resource kinds a policy
//! attaches to, which the second test drives through a project.
//!
//! The harness (`call`, `create_project`, `emulator`, `REGION`) is the one
//! `project_build_lifecycle` already drives this service with, and the report-group request
//! builder is `report_group_lifecycle`'s.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{call, create_project, emulator, REGION};
use super::report_group_lifecycle::create_report_group_body;

/// One submitted document and the document the service must report for it.
struct RenderingCase {
    label: &'static str,
    submitted: &'static str,
    /// Written out by hand, so the expectation is the rule and not the emulator's own
    /// rendering of the row.
    stored: &'static str,
}

/// The rendering table, over a REPORT GROUP's policy. Rows differ along what the rule
/// generalises over: the spacing of the submitted JSON (compact, spaced, indented across
/// lines, padded around every token, trailing newline), the element carrying a one-element
/// list (`Action`, `NotAction`, `Resource`, `NotResource`, each principal list), the element
/// count (one collapses, two do not, an empty list stays a list), the spelling of an account
/// principal (bare id, root ARN, a role ARN, the wildcard, a service, a federated provider),
/// the member order, and the kinds of value a document can carry (strings with significant
/// whitespace, escapes, non-ASCII text, numbers, booleans, null, nested objects).
///
/// Every `Action` a row names is one of the read-only consumer actions a report group may be
/// shared with — the CodeBuild user guide's "Sharing a report group" lists them, and
/// `resource_policy` is the suite that pins that refusal — so no row is refused for what it
/// grants rather than for how it is written.
fn rendering_cases() -> Vec<RenderingCase> {
    vec![
        RenderingCase {
            label: "already rendered: compact, no one-element list",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Read","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Action":["codebuild:BatchGetReportGroups","codebuild:BatchGetReports"],"Resource":"*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Read","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Action":["codebuild:BatchGetReportGroups","codebuild:BatchGetReports"],"Resource":"*"}]}"#,
        },
        RenderingCase {
            // The form a client that serialises a dict with default separators sends, which
            // is the form the recorded request uses.
            label: "a space after every ':' and ','",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Sid": "Read", "Effect": "Allow", "Principal": {"AWS": "arn:aws:iam::222222222222:root"}, "Action": ["codebuild:BatchGetReportGroups", "codebuild:BatchGetReports"], "Resource": "*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Read","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Action":["codebuild:BatchGetReportGroups","codebuild:BatchGetReports"],"Resource":"*"}]}"#,
        },
        RenderingCase {
            label: "indented across lines, with tabs and a trailing newline",
            submitted: "{\n\t\"Version\": \"2012-10-17\",\n\t\"Statement\": [\n\t\t{\n\t\t\t\"Sid\": \"Indented\",\n\t\t\t\"Effect\": \"Allow\",\n\t\t\t\"Principal\": \"*\",\n\t\t\t\"Action\": [\n\t\t\t\t\"codebuild:ListReportsForReportGroup\",\n\t\t\t\t\"codebuild:DescribeTestCases\"\n\t\t\t],\n\t\t\t\"Resource\": \"*\"\n\t\t}\n\t]\n}\n",
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Indented","Effect":"Allow","Principal":"*","Action":["codebuild:ListReportsForReportGroup","codebuild:DescribeTestCases"],"Resource":"*"}]}"#,
        },
        RenderingCase {
            // The recorded grant: one action, submitted as a one-element list.
            label: "a one-element Action list becomes the string",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Sid":"One","Effect":"Allow","Principal":"*","Action":["codebuild:BatchGetReportGroups"],"Resource":"*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"One","Effect":"Allow","Principal":"*","Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
        },
        RenderingCase {
            label: "one-element NotAction and Resource lists, spaced",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Sid": "NotOne", "Effect": "Deny", "Principal": "*", "NotAction": ["codebuild:BatchGetReports"], "Resource": ["arn:aws:codebuild:us-west-2:000000000000:report-group/rendered"]}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"NotOne","Effect":"Deny","Principal":"*","NotAction":"codebuild:BatchGetReports","Resource":"arn:aws:codebuild:us-west-2:000000000000:report-group/rendered"}]}"#,
        },
        RenderingCase {
            label: "one-element principal and NotResource lists",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Principals","Effect":"Allow","Principal":{"AWS":["arn:aws:iam::222222222222:root"],"Service":["codebuild.amazonaws.com"],"Federated":["accounts.google.com"]},"Action":"codebuild:BatchGetReportGroups","NotResource":["*"]}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Principals","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root","Service":"codebuild.amazonaws.com","Federated":"accounts.google.com"},"Action":"codebuild:BatchGetReportGroups","NotResource":"*"}]}"#,
        },
        RenderingCase {
            label: "two-element and empty lists keep their list form",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Sid": "Many", "Effect": "Allow", "Principal": {"AWS": ["arn:aws:iam::222222222222:root", "*"]}, "Action": [], "Resource": ["*", "arn:aws:codebuild:us-west-2:000000000000:report-group/rendered"]}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Many","Effect":"Allow","Principal":{"AWS":["arn:aws:iam::222222222222:root","*"]},"Action":[],"Resource":["*","arn:aws:codebuild:us-west-2:000000000000:report-group/rendered"]}]}"#,
        },
        RenderingCase {
            // `Statement` is NOT one of the collapsing elements: a one-statement policy is
            // reported as a one-element array, which is what the recorded document shows.
            label: "a one-statement Statement array stays an array",
            submitted: "{ \"Statement\" : [ { \"Effect\" : \"Allow\" , \"Principal\" : \"*\" , \"Action\" : \"codebuild:BatchGetReportGroups\" , \"Resource\" : \"*\" } ] }",
            stored: r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
        },
        RenderingCase {
            // The member order is the caller's: a document whose members are in no
            // canonical order keeps the order it was written in.
            label: "member order that is not alphabetical",
            submitted: r#"{"Statement":[{"Resource":"*","Action":"codebuild:BatchGetReports","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Effect":"Allow","Sid":"Ordered"}],"Version":"2012-10-17"}"#,
            stored: r#"{"Statement":[{"Resource":"*","Action":"codebuild:BatchGetReports","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Effect":"Allow","Sid":"Ordered"}],"Version":"2012-10-17"}"#,
        },
        RenderingCase {
            // Rule 3 alone: an account named by its id, with nothing else to rewrite.
            label: "an account named by its id becomes that account's root ARN",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Account","Effect":"Allow","Principal":{"AWS":"222222222222"},"Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Account","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
        },
        RenderingCase {
            // Rules 2 and 3 together, which is what the recorded grant needs: a
            // one-element list holding one account id.
            label: "a one-element list holding an account id becomes that root ARN",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Principal": {"AWS": ["222222222222"]}, "Action": ["codebuild:BatchGetReportGroups"], "Resource": "*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
        },
        RenderingCase {
            // Every value of the element is written in that spelling, wherever the element
            // carries it: a list of account ids stays a list of two, each an account's root.
            label: "every account id of a list becomes its root ARN, and the list stays one",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["222222222222","333333333333"]},"Action":"codebuild:BatchGetReportGroups","Resource":"*"},{"Effect":"Deny","NotPrincipal":{"AWS":"444444444444"},"Action":"codebuild:BatchGetReports","Resource":"*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["arn:aws:iam::222222222222:root","arn:aws:iam::333333333333:root"]},"Action":"codebuild:BatchGetReportGroups","Resource":"*"},{"Effect":"Deny","NotPrincipal":{"AWS":"arn:aws:iam::444444444444:root"},"Action":"codebuild:BatchGetReports","Resource":"*"}]}"#,
        },
        RenderingCase {
            // Every other value of the `AWS` type is the caller's own: the wildcard, an
            // identity's ARN, a root ARN already, and text that is no account id (eleven
            // digits, thirteen digits, digits with a letter) are reported as submitted.
            label: "a value that is not an account id is left as it was submitted",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["*","arn:aws:iam::222222222222:role/deploy","arn:aws:iam::222222222222:root","22222222222","2222222222222","22222222222a"]},"Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["*","arn:aws:iam::222222222222:role/deploy","arn:aws:iam::222222222222:root","22222222222","2222222222222","22222222222a"]},"Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
        },
        RenderingCase {
            // An account id under another principal TYPE names no account, so it is the
            // caller's text and stays as written.
            label: "digits under Service, Federated and CanonicalUser are not an account",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"222222222222","Federated":["222222222222"],"CanonicalUser":"222222222222"},"Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"222222222222","Federated":"222222222222","CanonicalUser":"222222222222"},"Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
        },
        RenderingCase {
            // Text inside a string literal is never read as a policy element, however much
            // it looks like one.
            label: "a string literal that looks like a collapsing element",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Sid": "\"Action\": [\"codebuild:DeleteReportGroup\"], \"AWS\": [\"222222222222\"]", "Effect": "Allow", "Principal": "*", "Action": ["codebuild:BatchGetReportGroups"], "Resource": "*"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"\"Action\": [\"codebuild:DeleteReportGroup\"], \"AWS\": [\"222222222222\"]","Effect":"Allow","Principal":"*","Action":"codebuild:BatchGetReportGroups","Resource":"*"}]}"#,
        },
        RenderingCase {
            label: "whitespace, escapes and non-ASCII text inside string literals survive",
            submitted: "{\n  \"Version\": \"2012-10-17\",\n  \"Statement\": [{\n    \"Sid\": \"a  b, c: d\",\n    \"Effect\": \"Allow\",\n    \"Principal\": \"*\",\n    \"Action\": [\"codebuild:BatchGetReportGroups\"],\n    \"Resource\": \"*\",\n    \"Condition\": {\"StringEquals\": {\"aws:PrincipalTag/équipe\": \"café — ☕ \\u0041\\\\ \\\"quoted\\\"\"}}\n  }]\n}",
            stored: "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"a  b, c: d\",\"Effect\":\"Allow\",\"Principal\":\"*\",\"Action\":\"codebuild:BatchGetReportGroups\",\"Resource\":\"*\",\"Condition\":{\"StringEquals\":{\"aws:PrincipalTag/équipe\":\"café — ☕ \\u0041\\\\ \\\"quoted\\\"\"}}}]}",
        },
        RenderingCase {
            // Numbers, booleans and null keep the spelling the caller chose: the document is
            // rendered from the caller's own bytes, not from a re-serialised value.
            label: "numbers, booleans and null keep their spelling",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Sid": "Values", "Effect": "Allow", "Principal": "*", "Action": "codebuild:BatchGetReportGroups", "Resource": "*", "Condition": {"NumericEquals": {"n": 1.50, "e": 1e2, "big": 10000000000000000000000}, "Bool": {"t": true, "f": false, "nothing": null}}}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Sid":"Values","Effect":"Allow","Principal":"*","Action":"codebuild:BatchGetReportGroups","Resource":"*","Condition":{"NumericEquals":{"n":1.50,"e":1e2,"big":10000000000000000000000},"Bool":{"t":true,"f":false,"nothing":null}}}]}"#,
        },
        RenderingCase {
            label: "the smallest policy object, padded with whitespace",
            submitted: "  {  }  ",
            stored: "{}",
        },
    ]
}

/// Store `policy` against `arn`, asserting the call succeeded and answered that ARN.
async fn put(router: &MultiServiceRouter, case: &str, arn: &str, policy: &str) {
    let (status, body) = call(
        router,
        "PutResourcePolicy",
        json!({"resourceArn": arn, "policy": policy}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutResourcePolicy must succeed, got {status} with {body}"
    );
    assert_eq!(
        body["resourceArn"].as_str(),
        Some(arn),
        "case {case:?}: the answer states the ARN the policy was stored against, got {body}"
    );
}

/// Read the policy stored against `arn`, asserting the call succeeded, and hand back the
/// document it reported.
async fn get(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let (status, body) = call(router, "GetResourcePolicy", json!({"resourceArn": arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResourcePolicy must succeed, got {status} with {body}"
    );
    body["policy"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the answer states the policy document, got {body}"))
        .to_string()
}

/// Create the report group `name` and hand back the ARN the service stored it with, read
/// from the service's own answer rather than assembled here.
async fn create_report_group(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "CreateReportGroup",
        create_report_group_body(name, "TEST", json!({"exportConfigType": "NO_EXPORT"}), None),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateReportGroup {name:?}: expected 200, got {status} with {body}"
    );
    body["reportGroup"]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateReportGroup {name:?} must report an ARN, got {body}"))
        .to_string()
}

/// RULES 1, 2 and 3, over the whole table: every submitted document is reported in the one
/// rendered form, a second read answers the same text, and re-submitting what was reported
/// stores it unchanged.
#[tokio::test]
async fn a_policy_is_reported_in_the_form_the_service_renders_it() {
    let router = emulator();
    let arn = create_report_group(&router, "rendered").await;

    for case in rendering_cases() {
        let label = case.label;
        put(&router, label, &arn, case.submitted).await;
        let read = get(&router, label, &arn).await;
        assert_eq!(
            read, case.stored,
            "case {label:?}: expected the service's rendering of the submitted document, \
             got {read:?}"
        );

        // Reading a policy changes nothing, so the next caller sees the same text.
        let again = get(&router, label, &arn).await;
        assert_eq!(
            again, read,
            "case {label:?}: a second read must answer the same document, got {again:?}"
        );

        // The rendering is a FIXED POINT: putting back what was read stores the same bytes,
        // so a client that re-applies the policy it read converges.
        put(&router, label, &arn, case.stored).await;
        let reread = get(&router, label, &arn).await;
        assert_eq!(
            reread, case.stored,
            "case {label:?}: re-putting the reported document must store it unchanged, got \
             {reread:?}"
        );
    }
}

/// RULE: the rendering is the SERVICE's, so it is the same for either kind of resource a
/// policy attaches to — a project's grant is rendered exactly as a report group's is.
///
/// The rows are the three rewrites, written with the read-only consumer actions a PROJECT may
/// be shared with (the CodeBuild user guide's "Sharing a project").
#[tokio::test]
async fn a_projects_policy_is_rendered_the_same_way() {
    let router = emulator();
    let project = create_project(&router, "rendered-project", json!({})).await;
    let arn = project["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateProject must report an ARN, got {project}"))
        .to_string();

    // (the case, the submitted document, the document the service must report)
    let rows: [(&str, &str, &str); 3] = [
        (
            "a space after every ':' and ','",
            r#"{"Version": "2012-10-17", "Statement": [{"Sid": "Read", "Effect": "Allow", "Principal": {"AWS": "arn:aws:iam::222222222222:root"}, "Action": ["codebuild:BatchGetProjects", "codebuild:BatchGetBuilds"], "Resource": "*"}]}"#,
            r#"{"Version":"2012-10-17","Statement":[{"Sid":"Read","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Action":["codebuild:BatchGetProjects","codebuild:BatchGetBuilds"],"Resource":"*"}]}"#,
        ),
        (
            "a one-element Action list becomes the string",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":["codebuild:ListBuildsForProject"],"Resource":"*"}]}"#,
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"codebuild:ListBuildsForProject","Resource":"*"}]}"#,
        ),
        (
            "an account named by its id becomes that account's root ARN",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["222222222222"]},"Action":"codebuild:BatchGetProjects","Resource":"*"}]}"#,
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::222222222222:root"},"Action":"codebuild:BatchGetProjects","Resource":"*"}]}"#,
        ),
    ];

    for (label, submitted, stored) in rows {
        put(&router, label, &arn, submitted).await;
        let read = get(&router, label, &arn).await;
        assert_eq!(
            read, stored,
            "case {label:?}: a project's policy is rendered as a report group's is, got \
             {read:?}"
        );
    }

    // The ARN the rows were stored against is this account's project in the signed Region,
    // so nothing above was stored against a value the service minted differently.
    assert_eq!(
        arn,
        format!(
            "arn:aws:codebuild:{REGION}:{}:project/rendered-project",
            engine::EMULATOR_ACCOUNT_ID
        ),
        "the rows must be stored against the project's own ARN"
    );
}

/// The document a refused write never stores: a sanity control for the suite above, so a row
/// cannot pass because the service accepts anything at all.
#[tokio::test]
async fn a_document_that_is_not_json_is_refused_and_nothing_is_rendered() {
    let router = emulator();
    let arn = create_report_group(&router, "rendered-refusal").await;

    let (status, body) = call(
        &router,
        "PutResourcePolicy",
        json!({"resourceArn": arn, "policy": "{\"Version\": \"2012-10-17\""}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case an unterminated object: the write must be refused, got {status} with {body}"
    );

    let (status, body) = call(&router, "GetResourcePolicy", json!({"resourceArn": arn})).await;
    assert_eq!(
        status, 400,
        "case an unterminated object: the refused write must store nothing, got {status} with \
         {body}"
    );
}
