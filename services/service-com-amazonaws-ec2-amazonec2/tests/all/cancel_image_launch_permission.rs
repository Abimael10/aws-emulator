// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `CancelImageLaunchPermission`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=CancelImageLaunchPermission`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: the call "removes your Amazon Web Services account from the launch
//! permissions for the specified AMI" — exactly the grant recorded for the CALLING account
//! comes off the image named, every other grantee's stands, and the request succeeds whether
//! or not that account held one. So the rows below differ along what the image's launch
//! permissions hold when the call is made (the calling account alone, it beside another
//! account, another account alone, nothing at all, a group, an organization and an
//! organizational unit) and along how many times the call is made, which is what the rule
//! generalises over; no row is the specification.
//!
//! Three suites:
//!
//! 1. the write table — what the attribute reads as after the cancel, read back through
//!    `DescribeImageAttribute` and, for the group, through `DescribeImages`;
//! 2. the refusal table — the ids it refuses, with the code and the sentence each carries;
//! 3. the `DryRun` probe — the verdict, and that a dry run leaves every grant where it was.
//!
//! The images are seeded through the store's own writer
//! (`ComAmazonawsEc2Amazonec2State::insert_image`, what a register operation calls), because
//! `RegisterImage` and `CreateImage` are still scaffolds with their own stub issues; every
//! launch permission a row starts from is granted over the wire through
//! `ModifyImageAttribute`, so the permissions this operation takes away are the ones that
//! operation really recorded.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, EXECUTABLE_BY_ALL, ImageLaunchPermissionGrantees,
    ImageSpec,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};

/// The Region every call here is signed for.
const REGION: &str = "us-east-2";

/// An account that is not the one this emulator serves, so an image of its own is another
/// account's.
const PARTNER_ACCOUNT: &str = "210987654321";

/// An account a launch permission is granted to beside the calling one, so a row can check
/// that another grantee's permission is left alone.
const OTHER_GRANTEE_ACCOUNT: &str = "444455556666";

/// "The name of the group" a public image's launch permission names
/// (`LaunchPermission$Group`): the `PermissionGroup` enum's only member, which is the same
/// term `DescribeImages` selects public AMIs with (`ec2::state::EXECUTABLE_BY_ALL`).
const LAUNCH_PERMISSION_GROUP_ALL: &str = EXECUTABLE_BY_ALL;

/// "The Amazon Resource Name (ARN) of an organization" (`LaunchPermission$OrganizationArn`),
/// in the format AWS Organizations publishes:
/// `arn:<partition>:organizations::<management-account>:organization/o-<id>`.
const ORGANIZATION_ARN: &str = "arn:aws:organizations::123456789012:organization/o-a1b2c3d4e5";

/// "The Amazon Resource Name (ARN) of an organizational unit (OU)"
/// (`LaunchPermission$OrganizationalUnitArn`):
/// `arn:<partition>:organizations::<management-account>:ou/o-<org>/ou-<org>-<id>`.
const ORGANIZATIONAL_UNIT_ARN: &str =
    "arn:aws:organizations::123456789012:ou/o-a1b2c3d4e5/ou-a1b2-f6g7h8i9";

/// EC2's answer to an authorized dry run, and the status and sentence it carries it on.
const DRY_RUN_CODE: &str = "DryRunOperation";
const DRY_RUN_STATUS: u16 = 412;
const DRY_RUN_MESSAGE: &str = "Request would have succeeded, but DryRun flag is set.";

/// The code EC2 answers for something it cannot read as an AMI id.
const INVALID_AMI_ID_MALFORMED: &str = "InvalidAMIID.Malformed";

/// The code EC2 answers for a readable AMI id the account can see no image for.
const INVALID_AMI_ID_NOT_FOUND: &str = "InvalidAMIID.NotFound";

/// The account this emulator serves — the one whose launch permission the cancel takes off.
fn own_account() -> String {
    ::engine::emulator_account_id().to_string()
}

/// One `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One `CancelImageLaunchPermission` call for `image_id`, with `extra` beside its `ImageId`.
async fn cancel(
    router: &MultiServiceRouter,
    image_id: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("ImageId", image_id)];
    params.extend_from_slice(extra);
    call(router, REGION, "CancelImageLaunchPermission", &params).await
}

/// Grant the launch permissions `params` names on `image_id`, through the operation that
/// records them — the writer whose grants this suite then cancels.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    params: &[(String, String)],
) {
    let mut body = vec![p("ImageId", image_id)];
    body.extend_from_slice(params);
    let (status, response) = call(router, REGION, "ModifyImageAttribute", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the grant {params:?} must succeed, got {status} with body {response}"
    );
}

/// Store one AMI and answer its id: the calling account's own unless `owner` names another
/// account, and private unless `public` says it has public launch permissions.
fn insert_image(
    engine: &engine::InMemoryEngine,
    name: &str,
    owner: Option<&str>,
    public: bool,
) -> String {
    let mut spec = ImageSpec {
        name: name.to_string(),
        description: "An image whose launch permissions this suite cancels".to_string(),
        public,
        ..ImageSpec::default()
    };
    if let Some(owner) = owner {
        spec.owner_id = owner.to_string();
    }
    engine.state::<Ec2State>().lock().insert_image(spec)
}

/// Every grantee the `launchPermission` attribute of `body` reports, as (element name, its
/// text) in document order.
///
/// Read with the XML parser rather than by substring, because the rule under test is about
/// WHICH grantee each permission names: `<group>all</group>` and `<userId>…</userId>` are
/// different permissions of the same image.
fn reported_permissions(case: &str, body: &str) -> Vec<(String, String)> {
    let document = roxmltree::Document::parse(body).unwrap_or_else(|error| {
        panic!("case {case:?}: the response body must be XML ({error}): {body}")
    });
    document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "launchPermission")
        .flat_map(|node| {
            // `descendants` yields the container itself first, and each `item` wrapper carries
            // the one leaf that names the grantee.
            node.descendants()
                .skip(1)
                .filter(|inner| {
                    inner.is_element() && !inner.children().any(|child| child.is_element())
                })
                .filter_map(|leaf| {
                    let text = leaf.text().unwrap_or_default();
                    (!text.is_empty())
                        .then(|| (leaf.tag_name().name().to_string(), text.to_string()))
                })
                .collect::<Vec<(String, String)>>()
        })
        .collect()
}

/// The launch permissions `DescribeImageAttribute` reports for `image_id`.
async fn read_permissions(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeImageAttribute",
        &[p("ImageId", image_id), p("Attribute", "launchPermission")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the attribute read of {image_id} must succeed, got {status} with \
         body {body}"
    );
    reported_permissions(case, &body)
}

/// What `DescribeImages` reports for `Image$Public` of `image_id`.
async fn reported_public(router: &MultiServiceRouter, case: &str, image_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeImages",
        &[p("ImageId.1", image_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeImages for {image_id} must succeed, got {status} with \
         body {body}"
    );
    element(&body, "isPublic").unwrap_or_default()
}

/// One row of the write table.
struct Row {
    /// What this row is a case of, named in every assertion of it.
    case: &'static str,
    /// The launch permissions granted before the cancel, each through `ModifyImageAttribute`.
    granted: Vec<(String, String)>,
    /// How many times the cancel is called — more than once is the idempotence case.
    cancels: usize,
    /// The (element, text) pairs the attribute must report afterwards, in order.
    expected: Vec<(String, String)>,
    /// What `Image$Public` must report afterwards.
    public: &'static str,
}

/// RULE: the call takes the CALLING account's launch permission off the AMI and leaves every
/// other grantee's where it was, and the attribute reads as that afterwards.
#[tokio::test]
async fn it_removes_the_calling_accounts_launch_permission_and_no_other() {
    let (router, engine) = super::ec2_query::emulator();
    let own = own_account();

    let rows: Vec<Row> = vec![
        Row {
            case: "an AMI shared with the calling account alone",
            granted: vec![p("LaunchPermission.Add.1.UserId", &own)],
            cancels: 1,
            expected: Vec::new(),
            public: "false",
        },
        Row {
            case: "an AMI shared with the calling account and with another account",
            granted: vec![
                p("LaunchPermission.Add.1.UserId", &own),
                p("LaunchPermission.Add.2.UserId", OTHER_GRANTEE_ACCOUNT),
            ],
            cancels: 1,
            expected: vec![p("userId", OTHER_GRANTEE_ACCOUNT)],
            public: "false",
        },
        Row {
            case: "an AMI shared with another account only",
            granted: vec![p("LaunchPermission.Add.1.UserId", OTHER_GRANTEE_ACCOUNT)],
            cancels: 1,
            expected: vec![p("userId", OTHER_GRANTEE_ACCOUNT)],
            public: "false",
        },
        Row {
            case: "an AMI shared with nobody at all",
            granted: Vec::new(),
            cancels: 1,
            expected: Vec::new(),
            public: "false",
        },
        Row {
            case: "an AMI whose permission for the calling account is cancelled twice",
            granted: vec![
                p("LaunchPermission.Add.1.UserId", &own),
                p("LaunchPermission.Add.2.UserId", OTHER_GRANTEE_ACCOUNT),
            ],
            cancels: 2,
            expected: vec![p("userId", OTHER_GRANTEE_ACCOUNT)],
            public: "false",
        },
        Row {
            case: "a PUBLIC AMI also shared with the calling account by name",
            granted: vec![
                p("LaunchPermission.Add.1.Group", LAUNCH_PERMISSION_GROUP_ALL),
                p("LaunchPermission.Add.2.UserId", &own),
            ],
            cancels: 1,
            expected: vec![p("group", LAUNCH_PERMISSION_GROUP_ALL)],
            public: "true",
        },
        Row {
            case: "an AMI shared with the calling account, an organization and an OU",
            granted: vec![
                p("LaunchPermission.Add.1.UserId", &own),
                p("LaunchPermission.Add.2.OrganizationArn", ORGANIZATION_ARN),
                p(
                    "LaunchPermission.Add.3.OrganizationalUnitArn",
                    ORGANIZATIONAL_UNIT_ARN,
                ),
            ],
            cancels: 1,
            expected: vec![
                p("organizationArn", ORGANIZATION_ARN),
                p("organizationalUnitArn", ORGANIZATIONAL_UNIT_ARN),
            ],
            public: "false",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let image_id = insert_image(
            &engine,
            &format!("cancel-launch-permission-{index}"),
            None,
            false,
        );
        if !row.granted.is_empty() {
            grant(&router, case, &image_id, &row.granted).await;
        }

        for attempt in 1..=row.cancels {
            let (status, body) = cancel(&router, &image_id, &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?} call {attempt}: expected HTTP 200, got {status} with body {body}"
            );
            assert_eq!(
                element(&body, "return").as_deref(),
                Some("true"),
                "case {case:?} call {attempt}: expected <return>true</return>, got body {body}"
            );
        }

        let reported = read_permissions(&router, case, &image_id).await;
        let expected: Vec<(String, String)> = row.expected.clone();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the launch permissions {expected:?}, got {reported:?}"
        );
        let public = reported_public(&router, case, &image_id).await;
        assert_eq!(
            public, row.public,
            "case {case:?}: expected isPublic {:?}, got {public:?}",
            row.public
        );
    }
}

/// RULE: a permission the cancel took off is no longer one the inventory narrows on — the
/// account's own grant and the one it left alone are read by `DescribeImages`' executable-by
/// narrowing exactly as the attribute reports them.
#[tokio::test]
async fn the_image_inventory_agrees_about_the_permission_that_came_off() {
    let (router, engine) = super::ec2_query::emulator();
    let own = own_account();
    let case = "a partner's public AMI shared with the calling account by name";

    // Another account's PUBLIC image, so the calling account can see it without owning it,
    // with an explicit grant to the calling account recorded beside the group.
    let image_id = insert_image(
        &engine,
        "cancel-launch-permission-partner",
        Some(PARTNER_ACCOUNT),
        true,
    );
    {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        // Granted through the store's own writer rather than over the wire: only the AMI's
        // OWNER may modify its attributes, and this image is another account's.
        guard.grant_image_launch_permissions(
            &image_id,
            &ImageLaunchPermissionGrantees {
                user_ids: vec![own.clone(), OTHER_GRANTEE_ACCOUNT.to_string()],
                ..ImageLaunchPermissionGrantees::default()
            },
        );
    }

    let (status, body) = cancel(&router, &image_id, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );

    let reported = read_permissions(&router, case, &image_id).await;
    assert_eq!(
        reported,
        vec![
            p("group", LAUNCH_PERMISSION_GROUP_ALL),
            p("userId", OTHER_GRANTEE_ACCOUNT),
        ],
        "case {case:?}: the calling account's permission must be the only one taken off, got \
         {reported:?}"
    );

    // The narrowing the inventory makes on the same fact: the account the grant was left for
    // may still launch the image, and the image is still public.
    let (status, body) = call(
        &router,
        REGION,
        "DescribeImages",
        &[
            p("ImageId.1", &image_id),
            p("ExecutableBy.1", OTHER_GRANTEE_ACCOUNT),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the inventory read must succeed, got {status} with body {body}"
    );
    assert!(
        body.contains(&image_id),
        "case {case:?}: {OTHER_GRANTEE_ACCOUNT} must still be able to launch {image_id}, got \
         body {body}"
    );
}

/// One row of the refusal table: the case, the id, and the code and sentence it carries.
struct Refusal {
    case: &'static str,
    image_id: String,
    code: &'static str,
    message: String,
}

/// RULE: the id is resolved before anything is written — a value this service cannot read as
/// an AMI id is malformed, and a readable id the account can see no image for does not exist.
#[tokio::test]
async fn it_refuses_an_image_id_it_cannot_resolve() {
    let (router, engine) = super::ec2_query::emulator();

    // An image of ANOTHER account that is private: an id this service really issued, naming
    // an image the calling account cannot see at all.
    let invisible = insert_image(
        &engine,
        "cancel-launch-permission-invisible",
        Some(PARTNER_ACCOUNT),
        false,
    );

    let malformed = |case: &'static str, image_id: &str| Refusal {
        case,
        image_id: image_id.to_string(),
        code: INVALID_AMI_ID_MALFORMED,
        message: format!("The image ID '{image_id}' is malformed"),
    };
    let not_found = |case: &'static str, image_id: &str| Refusal {
        case,
        image_id: image_id.to_string(),
        code: INVALID_AMI_ID_NOT_FOUND,
        message: format!("The image id '[{image_id}]' does not exist"),
    };
    let rows: Vec<Refusal> = vec![
        malformed("the AMI id prefix alone", "ami-"),
        malformed("a hexadecimal suffix of neither published width", "ami-0000000000000000"),
        malformed("a suffix carrying a character that is no hex digit", "ami-0123456789abcdefg"),
        malformed("a long-form suffix in upper case", "ami-0123456789ABCDEF0"),
        malformed("another resource kind's id", "i-0123456789abcdef0"),
        malformed("a value that is no EC2 id at all", "not-an-image-id"),
        // A long-form identifier above everything this service's mint has issued names no
        // image it could ever have held, which is a different answer from one that is absent.
        malformed("a fabricated long-form id", "ami-0fedcba987654321f"),
        // The retired SHORT form carries no identifier to decode, so one nothing holds is
        // reported absent rather than unreadable.
        not_found("a short-form id nothing holds", "ami-1a2b3c4d"),
        not_found("another account's private image", &invisible),
    ];

    for row in &rows {
        let case = row.case;
        let (status, body) = cancel(&router, &row.image_id, &[]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: expected {}, got body {body}",
            row.code
        );
        assert_eq!(
            error_message(&body),
            row.message,
            "case {case:?}: expected the sentence {:?}, got body {body}",
            row.message
        );
    }
}

/// RULE: `DryRun` "checks whether you have the required permissions for the action, WITHOUT
/// actually making the request" — so a request that would have gone on is answered
/// `DryRunOperation` and nothing comes off the image, while one that would have been refused
/// is refused with that reason instead.
#[tokio::test]
async fn a_dry_run_is_answered_without_cancelling_anything() {
    let (router, engine) = super::ec2_query::emulator();
    let own = own_account();
    let case = "a dry run of a cancel that would have succeeded";

    let image_id = insert_image(&engine, "cancel-launch-permission-dry-run", None, false);
    grant(
        &router,
        case,
        &image_id,
        &[
            p("LaunchPermission.Add.1.UserId", &own),
            p("LaunchPermission.Add.2.UserId", OTHER_GRANTEE_ACCOUNT),
        ],
    )
    .await;

    let (status, body) = cancel(&router, &image_id, &[p("DryRun", "true")]).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_CODE,
        "case {case:?}: expected {DRY_RUN_CODE}, got body {body}"
    );
    assert_eq!(
        error_message(&body),
        DRY_RUN_MESSAGE,
        "case {case:?}: expected the sentence {DRY_RUN_MESSAGE:?}, got body {body}"
    );

    let reported = read_permissions(&router, case, &image_id).await;
    assert_eq!(
        reported,
        vec![p("userId", &own), p("userId", OTHER_GRANTEE_ACCOUNT)],
        "case {case:?}: a dry run must leave every launch permission where it was, got \
         {reported:?}"
    );

    // A request the id screen refuses is refused with THAT reason under the flag too: there
    // is nothing to look up, whatever the caller may do.
    for (case, image_id, code) in [
        (
            "a dry run naming a value that is no AMI id",
            "ami-0000000000000000".to_string(),
            INVALID_AMI_ID_MALFORMED,
        ),
        (
            "a dry run naming a readable id nothing holds",
            "ami-1a2b3c4d".to_string(),
            INVALID_AMI_ID_NOT_FOUND,
        ),
    ] {
        let (status, body) = cancel(&router, &image_id, &[p("DryRun", "true")]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected {code}, got body {body}"
        );
    }

    // `DryRun=false` is not a dry run: it is an ordinary call, and it performs the cancel.
    let case = "DryRun=false";
    let (status, body) = cancel(&router, &image_id, &[p("DryRun", "false")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    let reported = read_permissions(&router, case, &image_id).await;
    assert_eq!(
        reported,
        vec![p("userId", OTHER_GRANTEE_ACCOUNT)],
        "case {case:?}: the calling account's permission must have come off, got {reported:?}"
    );
}
