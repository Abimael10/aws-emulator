// SPDX-License-Identifier: CC0-1.0

//! What a launch-template VERSION record says about who created it, and what a read reports
//! once a version has been deleted — driven end to end over the wire the service speaks
//! (`ec2Query`) through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test:
//!
//! 1. "The principal that created the version" (`LaunchTemplateVersion$CreatedBy`): every
//!    version record names the ARN of the IAM principal whose credentials signed the call
//!    that created it — on the `CreateLaunchTemplateVersion` result and on every
//!    `DescribeLaunchTemplateVersions` projection of that same version — and never the
//!    account root, which signs no request here;
//! 2. a `DeleteLaunchTemplateVersions` that names live non-default versions beside numbers
//!    the template does not hold succeeds PER ITEM, and the `DescribeLaunchTemplateVersions`
//!    that follows reports exactly the versions that survived, each with its own launch
//!    parameters — so a client that deletes a version and re-reads the template can see that
//!    it is gone. The versions a read reports come NEWEST FIRST, which is the order the page
//!    cursor cuts a page of this operation in
//!    (`crate::ops::describe_launch_template_versions`'s `page_cursor`) and the order the
//!    recorded responses of this operation carry: a page smaller than the versions held
//!    starts at the newest version, so the survivors below are expected in that order rather
//!    than as a set.
//!
//! Each table row is a distinct input along what those rules generalise over — how a version
//! is derived (from no source version, from a numbered one, from either alias), which launch
//! parameter it carries, how many versions the template holds, which numbers a delete names
//! (live, unknown, and both mixed) and which key names the template — so no single row is the
//! specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, indexed};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for. A launch template carries no Region member, so
/// the Region only selects the ec2Query envelope.
const REGION: &str = "us-east-1";

/// The instance type the version at each number carries, so a surviving version is told
/// apart by its own launch parameters rather than by its number alone. Index 0 is version 1.
const INSTANCE_TYPES: [&str; 5] = ["t3.nano", "t3.micro", "t3.small", "t3.medium", "t3.large"];

/// A `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One `LaunchTemplateData.<member>` parameter.
fn data(member: &str, value: &str) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call that has to succeed, as the body it answered.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The principal every call of this suite speaks for: the identity the emulator resolves a
/// request's credentials to, which is what a version record has to name as its creator.
fn calling_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The account-root ARN — the identity no call here signs as, and so the one a version record
/// must never name.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", ::engine::emulator_account_id())
}

/// A reported `createdBy` names an IAM PRINCIPAL of this account: an `arn:aws:iam::<12-digit
/// account>:user/<name>` — the identity a long-term access key belongs to — rather than the
/// account root or any other resource of the account.
///
/// Asserted on the SHAPE as well as on the value, so a resolver that answered some other ARN
/// (the root, a service principal, an empty resource) fails the case even though the handler
/// and this suite would still agree on the string.
fn assert_names_an_iam_user(case: &str, created_by: &str) {
    let rest = created_by.strip_prefix("arn:aws:iam::").unwrap_or_else(|| {
        panic!("case {case:?}: expected an IAM ARN in createdBy, got {created_by:?}")
    });
    let (account, resource) = rest.split_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: expected an account segment in createdBy, got {created_by:?}")
    });
    assert!(
        account.len() == 12 && account.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected a 12-digit account id in createdBy, got {created_by:?}"
    );
    let user_name = resource.strip_prefix("user/").unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the ARN of the IAM principal that created the version, \
             not {resource:?} (the account root creates nothing here): {created_by:?}"
        )
    });
    assert!(
        !user_name.is_empty(),
        "case {case:?}: expected a named IAM principal in createdBy, got {created_by:?}"
    );
}

/// One version as a read reports it: the members the rows below assert over.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedVersion {
    /// `LaunchTemplateVersion$VersionNumber`.
    number: i64,
    /// `LaunchTemplateVersion$DefaultVersion`.
    default_version: String,
    /// `LaunchTemplateVersion$CreatedBy`.
    created_by: String,
    /// The version's own `LaunchTemplateData$InstanceType`.
    instance_type: Option<String>,
}

/// Every version a `DescribeLaunchTemplateVersions` body carries, in the order the body
/// carries them. A reported version is the element with a `versionNumber` child of its own,
/// so nothing is read out of a shape that merely shares a member name.
fn reported_versions(case: &str, body: &str) -> Vec<ReportedVersion> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "versionNumber").is_some())
        .map(|node| ReportedVersion {
            number: child_text(node, "versionNumber")
                .and_then(|number| number.parse::<i64>().ok())
                .unwrap_or_default(),
            default_version: child_text(node, "defaultVersion").unwrap_or_default(),
            created_by: child_text(node, "createdBy").unwrap_or_default(),
            instance_type: child(node, "launchTemplateData")
                .and_then(|launch_data| child_text(launch_data, "instanceType")),
        })
        .collect()
}

/// `DescribeLaunchTemplateVersions` for one template, with `selection` as the extra
/// parameters that narrow it.
async fn describe_versions(
    case: &str,
    router: &MultiServiceRouter,
    launch_template_id: &str,
    selection: &[(String, String)],
) -> Vec<ReportedVersion> {
    let mut params = vec![p("LaunchTemplateId", launch_template_id)];
    params.extend(selection.iter().cloned());
    let body = ok(case, router, "DescribeLaunchTemplateVersions", &params).await;
    reported_versions(case, &body)
}

/// Create one launch template whose first version carries the instance type of version 1, and
/// answer its id.
async fn create_template(case: &str, router: &MultiServiceRouter, name: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            data("InstanceType", INSTANCE_TYPES[0]),
        ],
    )
    .await;
    element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must report the id it minted: {body}"))
}

/// One template holding `held` versions, version `n` carrying the instance type of its own
/// number, and its id.
async fn template_with_versions(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    held: i64,
) -> String {
    let launch_template_id = create_template(case, router, name).await;
    for number in 2..=held {
        let index = usize::try_from(number - 1).unwrap_or_default();
        ok(
            case,
            router,
            "CreateLaunchTemplateVersion",
            &[
                p("LaunchTemplateId", &launch_template_id),
                data("InstanceType", INSTANCE_TYPES[index]),
            ],
        )
        .await;
    }
    launch_template_id
}

/// RULE 1: every version record — the one the create answers with and the one every read
/// projects — names the calling principal as its creator.
///
/// The rows differ along how the new version is derived (no source version, a numbered one,
/// each alias) and which launch parameter it carries; each row's versions are then read back
/// through three different selections, because the projection is what a client reads.
#[tokio::test]
async fn every_version_names_the_calling_principal_that_created_it() {
    let (router, _engine) = super::ec2_query::emulator();
    let caller = calling_principal();
    let root = account_root();
    // The rule is only a rule because the two differ: the identity a request resolves to is
    // not the account root.
    assert_ne!(
        caller, root,
        "the calling principal must not be the account root, got {caller:?}"
    );
    assert_names_an_iam_user("the calling principal", &caller);

    // Each row: how the version names its source, and the one launch parameter it sets.
    let rows: [(&str, Option<&str>, (&str, &str)); 4] = [
        ("no source version", None, ("InstanceType", "t3.micro")),
        (
            "a numbered source version",
            Some("1"),
            ("InstanceType", "t3.small"),
        ),
        (
            "the $Latest alias as the source",
            Some("$Latest"),
            ("ImageId", "ami-00000000000000001"),
        ),
        (
            "the $Default alias as the source",
            Some("$Default"),
            ("KeyName", "analytics-key"),
        ),
    ];

    for (index, (case, source_version, (member, value))) in rows.iter().enumerate() {
        let launch_template_id =
            create_template(case, &router, &format!("attribution.row-{index}")).await;

        let mut params = vec![
            p("LaunchTemplateId", &launch_template_id),
            data(member, value),
        ];
        if let Some(source_version) = source_version {
            params.push(p("SourceVersion", source_version));
        }
        let created = ok(case, &router, "CreateLaunchTemplateVersion", &params).await;
        let created_by = element(&created, "createdBy");
        assert_eq!(
            created_by.as_deref(),
            Some(caller.as_str()),
            "case {case:?}: expected the create to attribute the version to the calling \
             principal {caller:?}, got {created_by:?} in {created}"
        );
        assert_names_an_iam_user(case, created_by.as_deref().unwrap_or_default());

        // The same version, read back through the projection every client reads — whichever
        // way the request selects the versions it asks for.
        let selections: [(&str, Vec<(String, String)>); 3] = [
            ("no version named", Vec::new()),
            (
                "both versions by number",
                indexed("LaunchTemplateVersion", &["1", "2"]),
            ),
            (
                "the $Latest alias",
                indexed("LaunchTemplateVersion", &["$Latest"]),
            ),
        ];
        for (selection_case, selection) in selections {
            let case = format!("{case}, read with {selection_case}");
            let reported =
                describe_versions(&case, &router, &launch_template_id, &selection).await;
            assert!(
                !reported.is_empty(),
                "case {case:?}: expected the read to report a version, got {reported:?}"
            );
            for version in &reported {
                assert_names_an_iam_user(&case, &version.created_by);
                assert_eq!(
                    version.created_by, caller,
                    "case {case:?}: expected version {} to name the calling principal \
                     {caller:?}, got {:?}",
                    version.number, version.created_by
                );
            }
        }
    }
}

/// One row of the delete table.
struct DeleteRow {
    /// What this row covers.
    case: &'static str,
    /// How many versions the template holds before the delete.
    held: i64,
    /// The version values the delete names, in the order it names them.
    named: &'static [&'static str],
    /// The numbers of the versions the delete removes.
    deleted: &'static [i64],
    /// The numbers the template does not hold, which the delete reports as unsuccessful.
    unknown: &'static [i64],
    /// Whether the delete and the read name the template by name rather than by id.
    by_name: bool,
}

/// RULE 2: a version the delete removed is gone from the read that follows, and the versions
/// it did not name are all still there with their own launch parameters.
///
/// The rows differ along how many versions the template holds, which numbers the delete names
/// (a live one beside one that never existed, several live ones, only unknown ones, and the
/// two mixed), and which key names the template.
#[tokio::test]
async fn the_read_after_a_partial_failure_delete_reports_only_the_survivors() {
    let (router, _engine) = super::ec2_query::emulator();
    let caller = calling_principal();

    let rows: [DeleteRow; 4] = [
        DeleteRow {
            case: "one live version beside a number that never existed",
            held: 2,
            named: &["2", "99"],
            deleted: &[2],
            unknown: &[99],
            by_name: false,
        },
        DeleteRow {
            case: "two live versions named at once",
            held: 3,
            named: &["3", "2"],
            deleted: &[3, 2],
            unknown: &[],
            by_name: true,
        },
        DeleteRow {
            case: "only numbers the template never held",
            held: 2,
            named: &["7", "8"],
            deleted: &[],
            unknown: &[7, 8],
            by_name: false,
        },
        DeleteRow {
            case: "every non-default version, with an unknown number among them",
            held: 4,
            named: &["2", "50", "4"],
            deleted: &[2, 4],
            unknown: &[50],
            by_name: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("survivors.row-{index}");
        let launch_template_id = template_with_versions(case, &router, &name, row.held).await;
        // Either key names the template, the delete and the read being about the template
        // rather than about the key the request used for it.
        let template_key = if row.by_name {
            p("LaunchTemplateName", &name)
        } else {
            p("LaunchTemplateId", &launch_template_id)
        };

        let mut params = vec![template_key];
        params.extend(indexed("LaunchTemplateVersion", row.named));
        let deleted_body = ok(case, &router, "DeleteLaunchTemplateVersions", &params).await;

        // Each named version falls to exactly one of the two reported sets.
        let parsed = document(case, &deleted_body);
        let numbers = |set: &str| -> Vec<i64> {
            set_items(parsed.root_element(), set)
                .into_iter()
                .filter_map(|item| child_text(item, "versionNumber"))
                .filter_map(|number| number.parse::<i64>().ok())
                .collect()
        };
        let succeeded = numbers("successfullyDeletedLaunchTemplateVersionSet");
        assert_eq!(
            succeeded,
            row.deleted.to_vec(),
            "case {case:?}: expected the versions {:?} to be deleted, got {succeeded:?} in \
             {deleted_body}",
            row.deleted
        );
        let failed = numbers("unsuccessfullyDeletedLaunchTemplateVersionSet");
        assert_eq!(
            failed,
            row.unknown.to_vec(),
            "case {case:?}: expected the numbers {:?} to be reported unsuccessful, got \
             {failed:?} in {deleted_body}",
            row.unknown
        );
        for item in set_items(
            parsed.root_element(),
            "unsuccessfullyDeletedLaunchTemplateVersionSet",
        ) {
            let code = child(item, "responseError").and_then(|error| child_text(error, "code"));
            assert_eq!(
                code.as_deref(),
                Some("launchTemplateVersionDoesNotExist"),
                "case {case:?}: expected a version the template does not hold to be reported \
                 under launchTemplateVersionDoesNotExist, got {code:?} in {deleted_body}"
            );
        }

        // The read that follows reports exactly the versions that survived — each with its
        // own instance type, the first version still the default one, and all of them still
        // attributed to the calling principal.
        let expected: Vec<ReportedVersion> = (1..=row.held)
            .filter(|number| !row.deleted.contains(number))
            .rev()
            .map(|number| ReportedVersion {
                number,
                default_version: if number == 1 { "true" } else { "false" }.to_string(),
                created_by: caller.clone(),
                instance_type: INSTANCE_TYPES
                    .get(usize::try_from(number - 1).unwrap_or_default())
                    .map(|instance_type| (*instance_type).to_string()),
            })
            .collect();
        let reported = describe_versions(
            &format!("{case}, the read after the delete"),
            &router,
            &launch_template_id,
            &[],
        )
        .await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the read to report the surviving versions {expected:?}, \
             got {reported:?}"
        );

        // A read changes nothing: the same read answers with the same versions.
        let again = describe_versions(
            &format!("{case}, the same read repeated"),
            &router,
            &launch_template_id,
            &[],
        )
        .await;
        assert_eq!(
            again, reported,
            "case {case:?}: two reads of one template must report the same versions, got \
             {again:?} after {reported:?}"
        );
    }
}
