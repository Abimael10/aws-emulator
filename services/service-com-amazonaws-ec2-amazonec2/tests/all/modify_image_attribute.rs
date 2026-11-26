// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyImageAttribute`, driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=ModifyImageAttribute`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test is that the operation WRITES THE ONE ATTRIBUTE THE REQUEST NAMES onto the
//! stored AMI of the calling account — the attribute resolved from `Attribute` when the request
//! names it and otherwise from the single value member it carries — so a later read of that
//! attribute reports exactly what was written, and a request that names none of them, two of
//! them, or an attribute this action cannot modify is refused and writes nothing.
//!
//! So the tables below span the attributes the operation serves and the FORMS a caller may name
//! them in, rather than one attribute in one form: each row differs along the attribute, the form
//! (`Attribute` + `Value`, the structured value member, the structured `Add`/`Remove` lists, the
//! flat `OperationType` lists), the kind of grantee (an account, the `all` group, an organization,
//! an organizational unit), the direction and the count. The recorded case — granting launch
//! permissions and reading them back — is one row.
//!
//! Every write is proved by reading it back through `DescribeImageAttribute`, and the public
//! launch permission also through the `Image$Public` member `DescribeImages` reports and the
//! `ExecutableUsers` narrowing it serves, so the writer and the readers of one fact are checked
//! against each other.
//!
//! The images are seeded through the store's own writer
//! (`ComAmazonawsEc2Amazonec2State::insert_image`, what a register operation calls), because
//! `RegisterImage` and `CreateImage` are still scaffolds with their own stub issues.

use ec2::ops::types::ProductCode;
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, IMAGE_BLOCK_PUBLIC_ACCESS_BLOCK_NEW_SHARING,
    IMDS_SUPPORT_V2, ImageSpec, MAX_STORED_BYTES,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};

/// The Region every call here is signed for.
const REGION: &str = "eu-west-1";

/// An account that is not the one this emulator serves, so an image of its own is another
/// account's.
const PARTNER_ACCOUNT: &str = "210987654321";

/// An account a launch permission is granted to.
const GRANTEE_ACCOUNT: &str = "111122223333";

/// A second one, so a table row can grant two at once and revoke one of them.
const OTHER_GRANTEE_ACCOUNT: &str = "444455556666";

/// A third one, never granted anything, so the `ExecutableUsers` narrowing has an account that
/// may NOT launch the image.
const UNGRANTED_ACCOUNT: &str = "777788889999";

/// "The Amazon Resource Name (ARN) of an organization" (`LaunchPermission$OrganizationArn`), in
/// the format AWS Organizations publishes:
/// `arn:<partition>:organizations::<management-account>:organization/o-<id>`.
const ORGANIZATION_ARN: &str = "arn:aws:organizations::123456789012:organization/o-a1b2c3d4e5";

/// "The Amazon Resource Name (ARN) of an organizational unit (OU)"
/// (`LaunchPermission$OrganizationalUnitArn`):
/// `arn:<partition>:organizations::<management-account>:ou/o-<org>/ou-<org>-<id>`.
const ORGANIZATIONAL_UNIT_ARN: &str =
    "arn:aws:organizations::123456789012:ou/o-a1b2c3d4e5/ou-a1b2-f6g7h8i9";

/// The Marketplace code the paid image carries, with its type.
const PRODUCT_CODE: &str = "aw0evgkw8e5c1q413zjy5pjce";

/// That code's type — one of the modelled `ProductCodeValues` catalog.
const PRODUCT_CODE_TYPE: &str = "marketplace";

/// "Constraints: Up to 10 000 account IDs can be specified in a single request"
/// (`LaunchPermission$UserId`): the published bound on how many grantees one call may name, which
/// the handler holds a request to before it reads any of them.
const MAX_LAUNCH_PERMISSION_MODIFICATIONS: usize = 10_000;

/// A structured `LaunchPermission.Add` list naming `count` distinct accounts from `first`,
/// generated rather than written out: a request at or past the published bound is about the COUNT,
/// so the ids themselves carry nothing but their well-formedness (12 digits).
fn generated_grantees_from(first: usize, count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|index| {
            (
                format!("LaunchPermission.Add.{}.UserId", index + 1),
                format!("{:012}", 100_000_000_000_u64 + (first + index) as u64),
            )
        })
        .collect()
}

/// The first `count` of those grantees.
fn generated_grantees(count: usize) -> Vec<(String, String)> {
    generated_grantees_from(0, count)
}

/// One `(key, value)` request parameter.
fn p(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One `ModifyImageAttribute` call for `image_id` with `params` beside its `ImageId`.
async fn modify(
    router: &MultiServiceRouter,
    image_id: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = vec![p("ImageId", image_id)];
    body.extend_from_slice(params);
    call(router, REGION, "ModifyImageAttribute", &body).await
}

/// One `DescribeImageAttribute` call — the read every write here is proved through.
async fn describe_attribute(
    router: &MultiServiceRouter,
    image_id: &str,
    attribute: &str,
) -> (u16, String) {
    call(
        router,
        REGION,
        "DescribeImageAttribute",
        &[p("ImageId", image_id), p("Attribute", attribute)],
    )
    .await
}

/// Store one AMI of the calling account, PRIVATE and with no product code, and answer its id.
fn insert_own_image(engine: &engine::InMemoryEngine, name: &str) -> String {
    engine.state::<Ec2State>().lock().insert_image(ImageSpec {
        name: name.to_string(),
        description: "The account's own image".to_string(),
        ..ImageSpec::default()
    })
}

/// Every GRANTEE the `element_name` element of the response envelope names, as (element name,
/// its text) in document order — an element carrying no text at all dropped, because an attribute
/// with no value and a list with no entry are what the ec2Query envelope renders as one.
///
/// Read with the XML parser rather than by substring, because the rule under test is about WHICH
/// grantee each permission names: `<group>all</group>` and `<userId>…</userId>` are different
/// permissions of the same image.
fn reported_leaves(case: &str, body: &str, element_name: &str) -> Vec<(String, String)> {
    let document = roxmltree::Document::parse(body).unwrap_or_else(|error| {
        panic!("case {case:?}: the response body must be XML ({error}): {body}")
    });
    document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == element_name)
        .flat_map(|node| {
            // `descendants` yields the element itself first, which is the container rather than
            // a value it carries.
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
        "case {case:?}: DescribeImages for {image_id} must succeed, got {status} with body {body}"
    );
    element(&body, "isPublic").unwrap_or_default()
}

/// One row of the write table: the calls it makes, and what the attribute must read as after.
struct Write {
    /// What this row is a case of, named in every assertion of it.
    case: &'static str,
    /// Calls made before the one under test, each of which must succeed — how a row that
    /// REVOKES a permission, or repeats a grant, gets the image into the state it starts from.
    setup: Vec<Vec<(String, String)>>,
    /// The call under test.
    params: Vec<(String, String)>,
    /// The attribute read back afterwards.
    attribute: &'static str,
    /// The (element, text) pairs that read must report, in order.
    expected: Vec<(&'static str, String)>,
    /// What `Image$Public` must report afterwards.
    public: &'static str,
}

/// Rule: the call writes the ONE attribute it names, and a later read reports exactly that.
#[tokio::test]
async fn writes_the_one_attribute_the_request_names() {
    let (router, engine) = super::ec2_query::emulator();

    // Rows differ along the attribute, the FORM the request names it in, the kind of grantee, the
    // direction and the count — what the rule generalises over. No row is the specification.
    let grant_account = |direction: &str, account: &str| {
        vec![
            p("Attribute", "launchPermission"),
            p("OperationType", direction),
            p("UserId.1", account),
        ]
    };
    let rows: Vec<Write> = vec![
        Write {
            case: "a description named by Attribute with the value beside it",
            setup: Vec::new(),
            params: vec![p("Attribute", "description"), p("Value", "A new description")],
            attribute: "description",
            expected: vec![("value", "A new description".to_string())],
            public: "false",
        },
        Write {
            case: "a description carried as the Description structure",
            setup: Vec::new(),
            params: vec![p("Description.Value", "Carried as the structure")],
            attribute: "description",
            expected: vec![("value", "Carried as the structure".to_string())],
            public: "false",
        },
        Write {
            case: "a description replaced by another",
            setup: vec![vec![p("Description.Value", "The first description")]],
            params: vec![p("Description.Value", "The second description")],
            attribute: "description",
            expected: vec![("value", "The second description".to_string())],
            public: "false",
        },
        Write {
            case: "a description cleared to no text at all",
            setup: vec![vec![p("Description.Value", "A description to take away")]],
            params: vec![p("Attribute", "description")],
            attribute: "description",
            expected: Vec::new(),
            public: "false",
        },
        Write {
            case: "IMDS support named by Attribute with the value beside it",
            setup: Vec::new(),
            params: vec![p("Attribute", "imdsSupport"), p("Value", IMDS_SUPPORT_V2)],
            attribute: "imdsSupport",
            expected: vec![("value", IMDS_SUPPORT_V2.to_string())],
            public: "false",
        },
        Write {
            case: "IMDS support carried as the ImdsSupport structure",
            setup: Vec::new(),
            params: vec![p("ImdsSupport.Value", IMDS_SUPPORT_V2)],
            attribute: "imdsSupport",
            expected: vec![("value", IMDS_SUPPORT_V2.to_string())],
            public: "false",
        },
        Write {
            case: "a launch permission for one account, in the flat form",
            setup: Vec::new(),
            params: grant_account("add", GRANTEE_ACCOUNT),
            attribute: "launchPermission",
            expected: vec![("userId", GRANTEE_ACCOUNT.to_string())],
            public: "false",
        },
        Write {
            case: "a launch permission for one account, in the structured form",
            setup: Vec::new(),
            params: vec![p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT)],
            attribute: "launchPermission",
            expected: vec![("userId", GRANTEE_ACCOUNT.to_string())],
            public: "false",
        },
        Write {
            case: "launch permissions for two accounts at once",
            setup: Vec::new(),
            params: vec![
                p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT),
                p("LaunchPermission.Add.2.UserId", OTHER_GRANTEE_ACCOUNT),
            ],
            attribute: "launchPermission",
            expected: vec![
                ("userId", GRANTEE_ACCOUNT.to_string()),
                ("userId", OTHER_GRANTEE_ACCOUNT.to_string()),
            ],
            public: "false",
        },
        Write {
            case: "the same launch permission granted twice",
            setup: vec![vec![p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT)]],
            params: vec![p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT)],
            attribute: "launchPermission",
            expected: vec![("userId", GRANTEE_ACCOUNT.to_string())],
            public: "false",
        },
        Write {
            case: "public launch permissions, granted to the group in the structured form",
            setup: Vec::new(),
            params: vec![p("LaunchPermission.Add.1.Group", "all")],
            attribute: "launchPermission",
            expected: vec![("group", "all".to_string())],
            public: "true",
        },
        Write {
            case: "public launch permissions, granted to the group in the flat form",
            setup: Vec::new(),
            params: vec![
                p("Attribute", "launchPermission"),
                p("OperationType", "add"),
                p("UserGroup.1", "all"),
            ],
            attribute: "launchPermission",
            expected: vec![("group", "all".to_string())],
            public: "true",
        },
        Write {
            case: "a launch permission for an organization",
            setup: Vec::new(),
            params: vec![p("LaunchPermission.Add.1.OrganizationArn", ORGANIZATION_ARN)],
            attribute: "launchPermission",
            expected: vec![("organizationArn", ORGANIZATION_ARN.to_string())],
            public: "false",
        },
        Write {
            case: "a launch permission for an organizational unit",
            setup: Vec::new(),
            params: vec![p(
                "LaunchPermission.Add.1.OrganizationalUnitArn",
                ORGANIZATIONAL_UNIT_ARN,
            )],
            attribute: "launchPermission",
            expected: vec![(
                "organizationalUnitArn",
                ORGANIZATIONAL_UNIT_ARN.to_string(),
            )],
            public: "false",
        },
        Write {
            case: "the group and an account granted in one call",
            setup: Vec::new(),
            params: vec![
                p("LaunchPermission.Add.1.Group", "all"),
                p("LaunchPermission.Add.2.UserId", GRANTEE_ACCOUNT),
            ],
            attribute: "launchPermission",
            expected: vec![
                ("group", "all".to_string()),
                ("userId", GRANTEE_ACCOUNT.to_string()),
            ],
            public: "true",
        },
        Write {
            case: "an account's launch permission revoked in the flat form",
            setup: vec![grant_account("add", GRANTEE_ACCOUNT)],
            params: grant_account("remove", GRANTEE_ACCOUNT),
            attribute: "launchPermission",
            expected: Vec::new(),
            public: "false",
        },
        Write {
            case: "an account's launch permission revoked in the structured form",
            setup: vec![vec![p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT)]],
            params: vec![p("LaunchPermission.Remove.1.UserId", GRANTEE_ACCOUNT)],
            attribute: "launchPermission",
            expected: Vec::new(),
            public: "false",
        },
        Write {
            case: "one of two accounts revoked, leaving the other",
            setup: vec![vec![
                p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT),
                p("LaunchPermission.Add.2.UserId", OTHER_GRANTEE_ACCOUNT),
            ]],
            params: vec![p("LaunchPermission.Remove.1.UserId", GRANTEE_ACCOUNT)],
            attribute: "launchPermission",
            expected: vec![("userId", OTHER_GRANTEE_ACCOUNT.to_string())],
            public: "false",
        },
        Write {
            case: "the public launch permission revoked, making the image private again",
            setup: vec![vec![p("LaunchPermission.Add.1.Group", "all")]],
            params: vec![p("LaunchPermission.Remove.1.Group", "all")],
            attribute: "launchPermission",
            expected: Vec::new(),
            public: "false",
        },
        Write {
            case: "an organization's launch permission revoked",
            setup: vec![vec![p(
                "LaunchPermission.Add.1.OrganizationArn",
                ORGANIZATION_ARN,
            )]],
            params: vec![p("LaunchPermission.Remove.1.OrganizationArn", ORGANIZATION_ARN)],
            attribute: "launchPermission",
            expected: Vec::new(),
            public: "false",
        },
        Write {
            case: "a launch permission revoked that was never granted",
            setup: Vec::new(),
            params: grant_account("remove", GRANTEE_ACCOUNT),
            attribute: "launchPermission",
            expected: Vec::new(),
            public: "false",
        },
        Write {
            case: "a grantee named in both directions of one call keeps the permission",
            setup: Vec::new(),
            params: vec![
                p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT),
                p("LaunchPermission.Remove.1.UserId", GRANTEE_ACCOUNT),
            ],
            attribute: "launchPermission",
            expected: vec![("userId", GRANTEE_ACCOUNT.to_string())],
            public: "false",
        },
    ];

    for (row_index, row) in rows.iter().enumerate() {
        let case = row.case;
        // Each row acts on its OWN image, so one row's write cannot be read as another's.
        let image_id = insert_own_image(&engine, &format!("modify-image-attribute-{row_index}"));

        for setup in &row.setup {
            let (status, body) = modify(&router, &image_id, setup).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the setup call {setup:?} must succeed, got {status} with \
                 body {body}"
            );
        }

        let (status, body) = modify(&router, &image_id, &row.params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );

        let (read_status, read_body) = describe_attribute(&router, &image_id, row.attribute).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: reading {} back must succeed, got {read_status} with body {read_body}",
            row.attribute
        );
        let expected: Vec<(String, String)> = row
            .expected
            .iter()
            .map(|(element, text)| ((*element).to_string(), text.clone()))
            .collect();
        let actual = reported_leaves(case, &read_body, row.attribute);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected <{}> to report {expected:?}, got {actual:?} in \
             {read_body}",
            row.attribute
        );

        let public = reported_public(&router, case, &image_id).await;
        assert_eq!(
            public, row.public,
            "case {case:?}: expected Image$Public to report {:?}, got {public:?}",
            row.public
        );
    }
}

/// One refusal row: the request, and the status, code and message it must be refused with.
struct Refusal {
    /// What this row is a case of, named in every assertion of it.
    case: &'static str,
    /// The `ImageId` the request carries.
    image_id: String,
    /// The members beside it.
    params: Vec<(String, String)>,
    /// The HTTP status of the refusal.
    status: u16,
    /// Its `<Code>`.
    code: &'static str,
    /// Its `<Message>`, built from the request the row sends.
    message: String,
}

/// Rule: a request that does not describe exactly one modifiable attribute of an AMI the account
/// owns is refused, and nothing is written.
#[tokio::test]
async fn refuses_what_it_cannot_modify() {
    let (router, engine) = super::ec2_query::emulator();
    let own = insert_own_image(&engine, "modify-image-attribute-refusals");
    let (partner_public, partner_private) = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        (
            guard.insert_image(ImageSpec {
                name: "modify-image-attribute-partner-public".to_string(),
                description: "Another account's public image".to_string(),
                public: true,
                owner_id: PARTNER_ACCOUNT.to_string(),
                ..ImageSpec::default()
            }),
            guard.insert_image(ImageSpec {
                name: "modify-image-attribute-partner-private".to_string(),
                description: "Another account's private image".to_string(),
                owner_id: PARTNER_ACCOUNT.to_string(),
                ..ImageSpec::default()
            }),
        )
    };

    let unrecognized = |case: &'static str, params: Vec<(String, String)>| Refusal {
        case,
        image_id: own.clone(),
        params,
        status: 400,
        code: "InvalidParameterValue",
        message: "Unrecognized attribute.".to_string(),
    };
    let rows: Vec<Refusal> = vec![
        // The `Attribute` catalog this operation serves: every other word names an attribute the
        // action does not modify, whether or not it is an `ImageAttributeName` at all.
        unrecognized(
            "an attribute of an AMI that only a register can set",
            vec![p("Attribute", "kernel"), p("Value", "aki-0a1b2c3d")],
        ),
        unrecognized(
            "a catalog value in the wrong case",
            vec![p("Attribute", "Description"), p("Value", "mis-cased")],
        ),
        unrecognized(
            "a name the model declares for no attribute",
            vec![p("Attribute", "sshKey"), p("Value", "a key")],
        ),
        unrecognized(
            "the product codes named by Attribute",
            vec![p("Attribute", "productCodes"), p("ProductCode.1", PRODUCT_CODE)],
        ),
        unrecognized(
            "the product codes carried as a list",
            vec![p("ProductCode.1", PRODUCT_CODE)],
        ),
        Refusal {
            case: "a request that names no attribute at all",
            image_id: own.clone(),
            params: Vec::new(),
            status: 400,
            code: "MissingParameter",
            message: "The request must contain a value for the attribute it modifies".to_string(),
        },
        Refusal {
            case: "a request that carries values for two attributes",
            image_id: own.clone(),
            params: vec![
                p("Description.Value", "a description"),
                p("ImdsSupport.Value", IMDS_SUPPORT_V2),
            ],
            status: 400,
            code: "InvalidParameterCombination",
            message: "Fields for multiple attribute types specified: description, imdsSupport"
                .to_string(),
        },
        Refusal {
            case: "a request that names one attribute and carries a value for another",
            image_id: own.clone(),
            params: vec![
                p("Attribute", "description"),
                p("LaunchPermission.Add.1.Group", "all"),
            ],
            status: 400,
            code: "InvalidParameterCombination",
            message: "Fields for multiple attribute types specified: description, launchPermission"
                .to_string(),
        },
        Refusal {
            case: "the launch permission attribute with no grantee named",
            image_id: own.clone(),
            params: vec![p("Attribute", "launchPermission")],
            status: 400,
            code: "MissingParameter",
            message: "The request must contain the parameter LaunchPermission".to_string(),
        },
        Refusal {
            // A `LaunchPermission` entry the request carried without saying WHO it is for: an
            // entry naming none of the four grantee kinds describes no permission at all.
            case: "a launch permission entry that names no grantee",
            image_id: own.clone(),
            params: vec![p("LaunchPermission.Add.1.UserId", "")],
            status: 400,
            code: "MissingParameter",
            message: "Each launch permission must name a user ID, an organization ARN, an \
                      organizational unit ARN or a group"
                .to_string(),
        },
        Refusal {
            // "Constraints: Up to 10 000 account IDs can be specified in a single request"
            // (`LaunchPermission$UserId`): one grantee past the published bound, generated rather
            // than written out, and refused on the COUNT before any grantee is read.
            case: "one grantee more than a single request may name",
            image_id: own.clone(),
            params: generated_grantees(MAX_LAUNCH_PERMISSION_MODIFICATIONS + 1),
            status: 400,
            code: "InvalidParameterValue",
            message: format!(
                "This request makes {} modifications to the launch permissions of the image, \
                 above the {MAX_LAUNCH_PERMISSION_MODIFICATIONS} a single operation may make",
                MAX_LAUNCH_PERMISSION_MODIFICATIONS + 1
            ),
        },
        Refusal {
            case: "flat grantee lists with no operation type to give them a direction",
            image_id: own.clone(),
            params: vec![p("Attribute", "launchPermission"), p("UserId.1", GRANTEE_ACCOUNT)],
            status: 400,
            code: "MissingParameter",
            message: "The request must contain the parameter OperationType".to_string(),
        },
        Refusal {
            case: "an IMDS support value outside the modelled catalog",
            image_id: own.clone(),
            params: vec![p("Attribute", "imdsSupport"), p("Value", "v1.0")],
            status: 400,
            code: "InvalidParameterValue",
            message: "Value (v1.0) for parameter imdsSupport is invalid. Valid values are v2.0"
                .to_string(),
        },
        Refusal {
            case: "the IMDS support attribute named with no value",
            image_id: own.clone(),
            params: vec![p("Attribute", "imdsSupport")],
            status: 400,
            code: "InvalidParameterValue",
            message: "Value () for parameter imdsSupport is invalid. Valid values are v2.0"
                .to_string(),
        },
        Refusal {
            case: "a grantee that is no account id",
            image_id: own.clone(),
            params: vec![p("LaunchPermission.Add.1.UserId", "not-an-account")],
            status: 400,
            code: "InvalidUserID.Malformed",
            message: "Invalid user id: 'not-an-account'".to_string(),
        },
        Refusal {
            case: "a group outside the modelled PermissionGroup catalog, in the flat form",
            image_id: own.clone(),
            params: vec![
                p("Attribute", "launchPermission"),
                p("OperationType", "add"),
                p("UserGroup.1", "everyone"),
            ],
            status: 400,
            code: "InvalidParameterValue",
            message: "Value (everyone) for parameter group is invalid. Valid values are all"
                .to_string(),
        },
        Refusal {
            // The structured form's `Group` member IS the modelled `PermissionGroup` enum, so the
            // request walk refuses a word outside it before this handler runs — with the
            // validator's own code and the member path it failed at.
            case: "a group outside the modelled PermissionGroup catalog, in the structured form",
            image_id: own.clone(),
            params: vec![p("LaunchPermission.Add.1.Group", "everyone")],
            status: 400,
            code: "ValidationError",
            message: "1 validation error detected: Value 'everyone' at \
                      'launchPermission.add.1.member.group' failed to satisfy constraint: \
                      Member must satisfy enum value set: [all]"
                .to_string(),
        },
        Refusal {
            case: "an organization ARN that names another service's resource",
            image_id: own.clone(),
            params: vec![p(
                "LaunchPermission.Add.1.OrganizationArn",
                "arn:aws:iam::123456789012:role/Shared",
            )],
            status: 400,
            code: "InvalidParameterValue",
            message: "Value (arn:aws:iam::123456789012:role/Shared) for parameter organizationArn \
                      is invalid. It must be the Amazon Resource Name of an organization."
                .to_string(),
        },
        Refusal {
            case: "an organizational unit ARN that names the organization instead",
            image_id: own.clone(),
            params: vec![p(
                "LaunchPermission.Add.1.OrganizationalUnitArn",
                ORGANIZATION_ARN,
            )],
            status: 400,
            code: "InvalidParameterValue",
            message: format!(
                "Value ({ORGANIZATION_ARN}) for parameter organizationalUnitArn is invalid. It \
                 must be the Amazon Resource Name of an organizational unit."
            ),
        },
        Refusal {
            case: "a value that is no AMI id at all",
            image_id: "not-an-ami".to_string(),
            params: vec![p("Description.Value", "a description")],
            status: 400,
            code: "InvalidAMIID.Malformed",
            message: "Invalid id: \"not-an-ami\"".to_string(),
        },
        Refusal {
            case: "a well-formed AMI id that names no image",
            image_id: "ami-0abc1234".to_string(),
            params: vec![p("Description.Value", "a description")],
            status: 400,
            code: "InvalidAMIID.NotFound",
            message: "The image id '[ami-0abc1234]' does not exist".to_string(),
        },
        Refusal {
            case: "another account's private image, which this account cannot see",
            image_id: partner_private.clone(),
            params: vec![p("Description.Value", "a description")],
            status: 400,
            code: "InvalidAMIID.NotFound",
            message: format!("The image id '[{partner_private}]' does not exist"),
        },
        Refusal {
            case: "another account's public image, which this account does not own",
            image_id: partner_public.clone(),
            params: vec![p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT)],
            status: 400,
            code: "AuthFailure",
            message: format!("Not authorized for images: [{partner_public}]"),
        },
        Refusal {
            case: "a dry run of a request that would have succeeded",
            image_id: own.clone(),
            params: vec![
                p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT),
                p("DryRun", "true"),
            ],
            status: 412,
            code: "DryRunOperation",
            message: "Request would have succeeded, but DryRun flag is set.".to_string(),
        },
        Refusal {
            case: "a dry run of a request that would have been refused",
            image_id: own.clone(),
            params: vec![p("Attribute", "kernel"), p("DryRun", "true")],
            status: 400,
            code: "InvalidParameterValue",
            message: "Unrecognized attribute.".to_string(),
        },
    ];

    for row in &rows {
        let case = row.case;
        let (status, body) = modify(&router, &row.image_id, &row.params).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {}, got {status} with body {body}",
            row.status
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: expected the code {:?}, got body {body}",
            row.code
        );
        assert_eq!(
            error_message(&body),
            row.message,
            "case {case:?}: expected the message {:?}, got body {body}",
            row.message
        );
    }

    // Nothing above wrote: the image still carries the description it was stored with, no launch
    // permission at all, and is still private.
    let (status, body) = describe_attribute(&router, &own, "description").await;
    assert_eq!(
        status, 200,
        "the refused calls must leave the image readable, got {status} with body {body}"
    );
    assert_eq!(
        reported_leaves("after the refusals", &body, "description"),
        vec![("value".to_string(), "The account's own image".to_string())],
        "the refused calls must leave the stored description alone, got body {body}"
    );
    let (status, body) = describe_attribute(&router, &own, "launchPermission").await;
    assert_eq!(
        status, 200,
        "the refused calls must leave the launch permissions readable, got {status} with \
         body {body}"
    );
    assert_eq!(
        reported_leaves("after the refusals", &body, "launchPermission"),
        Vec::new(),
        "the refused calls must grant no launch permission, got body {body}"
    );
    assert_eq!(
        reported_public(&router, "after the refusals", &own).await,
        "false",
        "the refused calls must leave the image private"
    );
}

/// Rule: public sharing is refused for an image that cannot be made public and in a Region that
/// no longer admits new public sharing, and neither refusal shares the image.
#[tokio::test]
async fn refuses_public_sharing_an_image_cannot_have() {
    let (router, engine) = super::ec2_query::emulator();
    let paid = engine.state::<Ec2State>().lock().insert_image(ImageSpec {
        name: "modify-image-attribute-paid".to_string(),
        description: "An image sold through Marketplace".to_string(),
        product_codes: vec![ProductCode {
            product_code_id: Some(PRODUCT_CODE.to_string()),
            product_code_type: Some(PRODUCT_CODE_TYPE.to_string()),
        }],
        ..ImageSpec::default()
    });
    let free = insert_own_image(&engine, "modify-image-attribute-free");

    // "Images with an Amazon Web Services Marketplace product code cannot be made public": a fact
    // of the IMAGE, so it refuses the paid one and admits the free one.
    let (status, body) = modify(
        &router,
        &paid,
        &[p("LaunchPermission.Add.1.Group", "all")],
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a paid image made public\": expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "case \"a paid image made public\": got body {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "Image {paid} has Amazon Web Services Marketplace product codes and cannot be made \
             public"
        ),
        "case \"a paid image made public\": got body {body}"
    );
    // Granting an ACCOUNT a launch permission on the same image is not public sharing, so it is
    // served: the rule is about the group alone.
    let (status, body) = modify(
        &router,
        &paid,
        &[p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a paid image shared with one account\": expected HTTP 200, got {status} with \
         body {body}"
    );
    assert_eq!(
        reported_public(&router, "a paid image shared with one account", &paid).await,
        "false",
        "sharing a paid image with one account must not make it public"
    );

    // "Enables block public access for AMIs at the account level in the specified Amazon Web
    // Services Region. This prevents the public sharing of your AMIs": a fact of the ACCOUNT in
    // that Region, so it refuses the free image that would otherwise have been shared.
    let (status, body) = call(
        &router,
        REGION,
        "EnableImageBlockPublicAccess",
        &[p(
            "ImageBlockPublicAccessState",
            IMAGE_BLOCK_PUBLIC_ACCESS_BLOCK_NEW_SHARING,
        )],
    )
    .await;
    assert_eq!(
        status, 200,
        "blocking public access must succeed, got {status} with body {body}"
    );
    let (status, body) = modify(
        &router,
        &free,
        &[p("LaunchPermission.Add.1.Group", "all")],
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"public sharing in a blocked Region\": expected HTTP 400, got {status} with \
         body {body}"
    );
    assert_eq!(
        error_code(&body),
        "OperationNotPermitted",
        "case \"public sharing in a blocked Region\": got body {body}"
    );
    assert_eq!(
        error_message(&body),
        format!(
            "Block public access for AMIs is set to \
             {IMAGE_BLOCK_PUBLIC_ACCESS_BLOCK_NEW_SHARING}, so public sharing cannot be \
             requested for image {free}"
        ),
        "case \"public sharing in a blocked Region\": got body {body}"
    );
    assert_eq!(
        reported_public(&router, "public sharing in a blocked Region", &free).await,
        "false",
        "a refused public sharing must leave the image private"
    );
}

/// The AMI ids `DescribeImages` reports for the account's own images that `term` may launch —
/// the `ExecutableUsers` narrowing this operation's grants feed.
async fn executable_by(router: &MultiServiceRouter, term: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeImages",
        &[p("ExecutableBy.1", term), p("Owner.1", "self")],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeImages for ExecutableBy={term} must succeed, got {status} with body {body}"
    );
    super::ec2_query::elements(&body, "imageId")
}

/// Rule: the launch permissions this operation records are the ones `DescribeImages` narrows its
/// `ExecutableUsers` on, so the writer and that reader cannot disagree about one grant.
#[tokio::test]
async fn the_image_inventory_narrows_on_the_granted_launch_permissions() {
    let (router, engine) = super::ec2_query::emulator();
    let shared = insert_own_image(&engine, "modify-image-attribute-shared");

    // The image is the account's own and private, so nothing but an explicit grant can select it
    // for another account.
    for (case, term) in [
        ("an account nothing has been granted to", GRANTEE_ACCOUNT),
        ("another account nothing has been granted to", UNGRANTED_ACCOUNT),
    ] {
        let reported = executable_by(&router, term).await;
        assert!(
            !reported.contains(&shared),
            "case {case:?}: {shared} must not be launchable by {term} before the grant, \
             got {reported:?}"
        );
    }

    let (status, body) = modify(
        &router,
        &shared,
        &[p("LaunchPermission.Add.1.UserId", GRANTEE_ACCOUNT)],
    )
    .await;
    assert_eq!(
        status, 200,
        "granting the launch permission must succeed, got {status} with body {body}"
    );

    let granted = executable_by(&router, GRANTEE_ACCOUNT).await;
    assert!(
        granted.contains(&shared),
        "case \"the account the permission was granted to\": {shared} must be launchable by \
         {GRANTEE_ACCOUNT}, got {granted:?}"
    );
    let ungranted = executable_by(&router, UNGRANTED_ACCOUNT).await;
    assert!(
        !ungranted.contains(&shared),
        "case \"an account the permission was not granted to\": {shared} must not be \
         launchable by {UNGRANTED_ACCOUNT}, got {ungranted:?}"
    );

    // Revoking it takes the image back out of that narrowing, so the two readers follow the one
    // stored fact rather than keeping their own.
    let (status, body) = modify(
        &router,
        &shared,
        &[p("LaunchPermission.Remove.1.UserId", GRANTEE_ACCOUNT)],
    )
    .await;
    assert_eq!(
        status, 200,
        "revoking the launch permission must succeed, got {status} with body {body}"
    );
    let revoked = executable_by(&router, GRANTEE_ACCOUNT).await;
    assert!(
        !revoked.contains(&shared),
        "case \"after the revoke\": {shared} must no longer be launchable by \
         {GRANTEE_ACCOUNT}, got {revoked:?}"
    );
}

/// Rule: a grant the store's shared byte budget has no room for is refused whole rather than
/// half-applied, so sharing one image cannot grow the store without bound.
#[tokio::test]
async fn refuses_a_grant_the_byte_budget_has_no_room_for() {
    let (router, engine) = super::ec2_query::emulator();
    let image_id = insert_own_image(&engine, "modify-image-attribute-byte-budget");

    // Calls at the published per-request bound, until the budget refuses one. The loop terminates
    // on the budget itself: every grant that is recorded retains at least one byte, so only
    // finitely many can be, and the guard below fails the test rather than looping if the budget
    // never bites.
    let mut granted = 0_usize;
    let (status, body) = loop {
        let params = generated_grantees_from(granted, MAX_LAUNCH_PERMISSION_MODIFICATIONS);
        let (status, body) = modify(&router, &image_id, &params).await;
        if status != 200 {
            break (status, body);
        }
        granted += MAX_LAUNCH_PERMISSION_MODIFICATIONS;
        assert!(
            granted <= MAX_STORED_BYTES,
            "case \"the byte budget\": {granted} grants were recorded inside a budget of \
             {MAX_STORED_BYTES} bytes, so no call was ever refused"
        );
    };

    assert_eq!(
        status, 400,
        "case \"the byte budget\": expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceLimitExceeded",
        "case \"the byte budget\": got body {body}"
    );
    let message = error_message(&body);
    assert!(
        message.contains(&format!(
            "of at most {MAX_STORED_BYTES} bytes of caller-supplied data"
        )),
        "case \"the byte budget\": expected a message naming the budget, got {message:?}"
    );

    // The refused call recorded NOTHING: the image still carries exactly the grants the calls
    // that succeeded made, so the request was refused whole rather than half-applied.
    let (read_status, read_body) = describe_attribute(&router, &image_id, "launchPermission").await;
    assert_eq!(
        read_status, 200,
        "case \"the byte budget\": reading the launch permissions back must succeed, got \
         {read_status}"
    );
    assert_eq!(
        reported_leaves("the byte budget", &read_body, "launchPermission").len(),
        granted,
        "case \"the byte budget\": expected exactly the {granted} grants the served calls made"
    );
}
