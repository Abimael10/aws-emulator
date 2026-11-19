// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyFpgaImageAttribute`, driven over the wire the service actually
//! speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. A MODIFY WRITES THE ONE ATTRIBUTE THE REQUEST DESCRIBES onto the stored AFI, and that
//!    is what a later `DescribeFpgaImageAttribute` reports — so the table below spans the
//!    whole published `FpgaImageAttributeName` catalog rather than one of its members, and
//!    every row is read back through that describe.
//! 2. THE ATTRIBUTE IS THE ONE THE REQUEST NAMES, or the one it carries a value for: a
//!    request may name `Attribute` and the value together or carry the value alone, and one
//!    that describes no modification, or two, is refused rather than answered with an
//!    attribute nothing changed.
//! 3. A LOAD PERMISSION IS A GRANTEE, HELD OR NOT: adding one twice holds it once, removing
//!    one takes it away, and the two request forms — the structured `LoadPermission` and the
//!    flat `UserIds`/`UserGroups` pair with an `OperationType` — name the same grantees.
//! 4. AN AFI ID IS RESOLVED BY SHAPE: a value that is not shaped like an EC2 resource id is
//!    `InvalidFpgaImageID.Malformed` before any lookup, and every value of that shape the
//!    account holds no AFI under — the all-zero placeholder included — is
//!    `InvalidFpgaImageID.NotFound`, in the sentence the attribute operations word that cause
//!    with.
//!
//! Each row is a distinct input along what the rule generalises over — which attribute, which
//! request form, which grantee, which id shape — so no row is the specification.

use ec2::ops::modify_fpga_image_attribute::MAX_ATTRIBUTE_MODIFICATIONS;
use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{error_code, error_message};
use super::fpga_image_tasks_and_volume_io::seed_fpga_image;
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The HTTP status EC2 answers a `DryRun` request with: 412, carrying `DryRunOperation`.
const DRY_RUN_STATUS: u16 = 412;

/// An account id of the form AWS issues, to grant a load permission to.
const GRANTEE_ACCOUNT: &str = "111122223333";

/// A second account, so the row that adds two grantees does not name one twice.
const OTHER_GRANTEE_ACCOUNT: &str = "444455556666";

/// The one member of the modelled `PermissionGroup` catalog: "the possible value is `all`".
const PERMISSION_GROUP_ALL: &str = "all";

/// The all-zero AFI id: a value shaped like an EC2 resource id that EC2 can never have issued
/// an AFI under, which is the id a client reaches for to name an image that does not exist.
const UNKNOWN_FPGA_IMAGE_ID: &str = "afi-00000000000000000";

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl AsRef<str>) -> (String, String) {
    (key.to_string(), value.as_ref().to_string())
}

/// The fields one call carries beside the action.
type Fields = Vec<(String, String)>;

/// One row of the attribute table: the case, the fields the modify carries, the attribute the
/// read asks for, the member it reports and the value that member must carry.
type AttributeRow = (&'static str, Fields, &'static str, &'static str, String);

/// One row of the refusal table: the case, the fields the call carries, and the status and
/// code it must be refused with.
type Refusal = (&'static str, Fields, u16, &'static str);

async fn op(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// Describe `attribute` of `fpga_image_id`, as the body a successful read answers with.
async fn described(
    case: &str,
    router: &MultiServiceRouter,
    fpga_image_id: &str,
    attribute: &str,
) -> String {
    let (status, body) = op(
        router,
        "DescribeFpgaImageAttribute",
        &[p("FpgaImageId", fpga_image_id), p("Attribute", attribute)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the {attribute} read must succeed, got {status} with body {body}"
    );
    body
}

/// The text of the reported attribute's direct child named `member`.
fn attribute_member(case: &str, body: &str, member: &str) -> Option<String> {
    let parsed = document(case, body);
    let attribute = child(parsed.root_element(), "fpgaImageAttribute")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <fpgaImageAttribute>, got {body}"));
    child_text(attribute, member)
}

/// The `(userId, group)` pairs of the reported `loadPermissions` set.
fn reported_permissions(case: &str, body: &str) -> Vec<(Option<String>, Option<String>)> {
    let parsed = document(case, body);
    let attribute = child(parsed.root_element(), "fpgaImageAttribute")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <fpgaImageAttribute>, got {body}"));
    set_items(attribute, "loadPermissions")
        .into_iter()
        .map(|item| (child_text(item, "userId"), child_text(item, "group")))
        .collect()
}

/// The `(productCodeId, productCodeType)` pairs of the reported `productCodes` set.
fn reported_product_codes(case: &str, body: &str) -> Vec<(Option<String>, Option<String>)> {
    let parsed = document(case, body);
    let attribute = child(parsed.root_element(), "fpgaImageAttribute")
        .unwrap_or_else(|| panic!("case {case:?}: expected a <fpgaImageAttribute>, got {body}"));
    set_items(attribute, "productCodes")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "productCode"),
                child_text(item, "type"),
            )
        })
        .collect()
}

/// Store one AFI of the calling account and answer its id, through the one seeding helper the
/// FPGA-image suites of this crate share — `CreateFpgaImage` reads a design checkpoint out of
/// Amazon S3, which this emulator holds none of, so it mints no AFI.
fn seed_afi(engine: &engine::InMemoryEngine, name: &str) -> String {
    seed_fpga_image(engine, name).0
}

// --- the attribute table ----------------------------------------------------

/// Rule: a modify writes the one attribute the request describes, and the describe of that
/// attribute reports the new value.
#[tokio::test]
async fn each_attribute_is_written_and_read_back() {
    let (router, engine) = super::ec2_query::emulator();
    let fpga_image_id = seed_afi(&engine, "attribute-table");

    // One row per member of the published `FpgaImageAttributeName` catalog
    // (`smithy_query(query="shape",shape="FpgaImageAttributeName")`): the request fields, the
    // attribute the read asks for, and what that read must report.
    let rows: Vec<AttributeRow> = vec![
        (
            "a description, named by Attribute and carried together",
            vec![
                p("Attribute", "description"),
                p("Description", "edge detection, revision 4"),
            ],
            "description",
            "description",
            "edge detection, revision 4".to_string(),
        ),
        (
            "a name, carried alone with no Attribute at all",
            vec![p("Name", "edge-detect-v4")],
            "name",
            "name",
            "edge-detect-v4".to_string(),
        ),
    ];

    for (case, fields, attribute, member, expected) in rows {
        let mut params = vec![p("FpgaImageId", &fpga_image_id)];
        params.extend(fields);
        let (status, body) = op(&router, "ModifyFpgaImageAttribute", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the modify must succeed, got {status} with body {body}"
        );
        let answered = attribute_member(case, &body, member);
        assert_eq!(
            answered.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: the answer must carry the value it wrote, got {answered:?}"
        );

        let read = described(case, &router, &fpga_image_id, attribute).await;
        let reported = attribute_member(case, &read, member);
        assert_eq!(
            reported.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: the describe must report the written value, got {reported:?}"
        );
    }
}

/// Rule: a load permission is a grantee the AFI holds or does not — added once however many
/// times it is named, removed by either request form, and reported by the describe either
/// way.
#[tokio::test]
async fn load_permissions_are_added_and_removed_in_both_request_forms() {
    let case = "load permissions";
    let (router, engine) = super::ec2_query::emulator();
    let fpga_image_id = seed_afi(&engine, "load-permissions");

    // The structured form: two grantees in one call, one of them named twice.
    let (status, body) = op(
        &router,
        "ModifyFpgaImageAttribute",
        &[
            p("FpgaImageId", &fpga_image_id),
            p("LoadPermission.Add.1.UserId", GRANTEE_ACCOUNT),
            p("LoadPermission.Add.2.Group", PERMISSION_GROUP_ALL),
            p("LoadPermission.Add.3.UserId", GRANTEE_ACCOUNT),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: adding load permissions must succeed, got {status} with body {body}"
    );
    let granted = reported_permissions(case, &body);
    assert_eq!(
        granted,
        vec![
            (Some(GRANTEE_ACCOUNT.to_string()), None),
            (None, Some(PERMISSION_GROUP_ALL.to_string())),
        ],
        "case {case:?}: an account named twice is granted once, got {granted:?}"
    );

    // The flat form: the same grantees named through `UserId.N` with an `OperationType`.
    let (status, body) = op(
        &router,
        "ModifyFpgaImageAttribute",
        &[
            p("FpgaImageId", &fpga_image_id),
            p("OperationType", "add"),
            p("UserId.1", OTHER_GRANTEE_ACCOUNT),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the flat form must succeed, got {status} with body {body}"
    );
    let read = described(case, &router, &fpga_image_id, "loadPermission").await;
    let granted = reported_permissions(case, &read);
    assert_eq!(
        granted,
        vec![
            (Some(GRANTEE_ACCOUNT.to_string()), None),
            (None, Some(PERMISSION_GROUP_ALL.to_string())),
            (Some(OTHER_GRANTEE_ACCOUNT.to_string()), None),
        ],
        "case {case:?}: the describe must report every grantee the AFI holds, got {granted:?}"
    );

    // Removing: one grantee by the structured form, one by the flat one.
    let (status, body) = op(
        &router,
        "ModifyFpgaImageAttribute",
        &[
            p("FpgaImageId", &fpga_image_id),
            p("LoadPermission.Remove.1.Group", PERMISSION_GROUP_ALL),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: removing a group must succeed, got {status} with body {body}"
    );
    let (status, body) = op(
        &router,
        "ModifyFpgaImageAttribute",
        &[
            p("FpgaImageId", &fpga_image_id),
            p("OperationType", "remove"),
            p("UserId.1", OTHER_GRANTEE_ACCOUNT),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: removing an account must succeed, got {status} with body {body}"
    );
    let granted = reported_permissions(case, &body);
    assert_eq!(
        granted,
        vec![(Some(GRANTEE_ACCOUNT.to_string()), None)],
        "case {case:?}: only the grantee nothing removed is left, got {granted:?}"
    );

    // `ResetFpgaImageAttribute` clears them, which is what makes the two operations one
    // store: the describe reports an empty set rather than the grantees above.
    let (status, body) = op(
        &router,
        "ResetFpgaImageAttribute",
        &[
            p("FpgaImageId", &fpga_image_id),
            p("Attribute", "loadPermission"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the reset must succeed, got {status} with body {body}"
    );
    let read = described(case, &router, &fpga_image_id, "loadPermission").await;
    let granted = reported_permissions(case, &read);
    assert!(
        granted.is_empty(),
        "case {case:?}: a reset AFI holds no load permission, got {granted:?}"
    );
}

/// Rule: "after you add a product code to an AFI, it can't be removed" — a code the request
/// names is added once, and a second call naming it again changes nothing.
#[tokio::test]
async fn product_codes_are_added_once_and_stay() {
    let case = "product codes";
    let (router, engine) = super::ec2_query::emulator();
    let fpga_image_id = seed_afi(&engine, "product-codes");

    let first = "aaa111bbb222ccc333";
    let second = "ddd444eee555fff666";
    let (status, body) = op(
        &router,
        "ModifyFpgaImageAttribute",
        &[
            p("FpgaImageId", &fpga_image_id),
            p("Attribute", "productCodes"),
            p("ProductCode.1", first),
            p("ProductCode.2", second),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: adding product codes must succeed, got {status} with body {body}"
    );
    let reported = reported_product_codes(case, &body);
    assert_eq!(
        reported,
        vec![
            (
                Some(first.to_string()),
                Some("marketplace".to_string())
            ),
            (
                Some(second.to_string()),
                Some("marketplace".to_string())
            ),
        ],
        "case {case:?}: both codes must be reported, got {reported:?}"
    );

    let (status, body) = op(
        &router,
        "ModifyFpgaImageAttribute",
        &[
            p("FpgaImageId", &fpga_image_id),
            p("ProductCode.1", first),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: naming a held code again must succeed, got {status} with body {body}"
    );
    let read = described(case, &router, &fpga_image_id, "productCodes").await;
    let reported = reported_product_codes(case, &read);
    assert_eq!(
        reported.len(),
        2,
        "case {case:?}: a code named twice is held once, got {reported:?}"
    );
}

// --- the refusals -----------------------------------------------------------

/// Rule: a request this operation cannot carry out is refused with EC2's own code, and the
/// AFI keeps what it held.
#[tokio::test]
async fn a_request_that_cannot_be_carried_out_is_refused() {
    let (router, engine) = super::ec2_query::emulator();
    let fpga_image_id = seed_afi(&engine, "refusals");

    let rows: Vec<Refusal> = vec![
        (
            "a value that is not shaped like a resource id",
            vec![p("FpgaImageId", "image-7"), p("Name", "whatever")],
            400,
            "InvalidFpgaImageID.Malformed",
        ),
        (
            "an id of the right shape the account holds no AFI under",
            vec![
                p("FpgaImageId", UNKNOWN_FPGA_IMAGE_ID),
                p("Name", "whatever"),
            ],
            400,
            "InvalidFpgaImageID.NotFound",
        ),
        (
            "the 8-digit short form, naming no AFI of the account",
            vec![p("FpgaImageId", "afi-1a2b3c4d"), p("Name", "whatever")],
            400,
            "InvalidFpgaImageID.NotFound",
        ),
        (
            "no value for any attribute at all",
            vec![p("FpgaImageId", fpga_image_id.clone())],
            400,
            "MissingParameter",
        ),
        (
            "values for two attributes in one call",
            vec![
                p("FpgaImageId", fpga_image_id.clone()),
                p("Name", "two"),
                p("Description", "attributes"),
            ],
            400,
            "InvalidParameterCombination",
        ),
        (
            "an attribute named beside another attribute's value",
            vec![
                p("FpgaImageId", fpga_image_id.clone()),
                p("Attribute", "description"),
                p("Name", "mismatched"),
            ],
            400,
            "InvalidParameterCombination",
        ),
        (
            "a grantee that is no account id",
            vec![
                p("FpgaImageId", fpga_image_id.clone()),
                p("LoadPermission.Add.1.UserId", "not-an-account"),
            ],
            400,
            "InvalidUserID.Malformed",
        ),
        (
            "a group outside the one the model publishes, in the flat form the model does \
             not bound",
            vec![
                p("FpgaImageId", fpga_image_id.clone()),
                p("OperationType", "add"),
                p("UserGroup.1", "everyone"),
            ],
            400,
            "InvalidParameterValue",
        ),
        (
            "a load permission naming neither a grantee nor a group",
            vec![
                p("FpgaImageId", fpga_image_id.clone()),
                p("LoadPermission.Add.1.UserId", ""),
            ],
            400,
            "MissingParameter",
        ),
        (
            "a dry run that would have succeeded",
            vec![
                p("FpgaImageId", fpga_image_id.clone()),
                p("DryRun", "true"),
                p("Name", "never-written"),
            ],
            DRY_RUN_STATUS,
            "DryRunOperation",
        ),
        (
            "more load permissions than one call may modify",
            {
                let mut params = vec![p("FpgaImageId", fpga_image_id.clone())];
                // One past the modifications a single call may make, so the request is
                // refused before any per-entry work is done.
                for nth in 0..=MAX_ATTRIBUTE_MODIFICATIONS {
                    params.push(p(
                        &format!("LoadPermission.Add.{}.UserId", nth + 1),
                        format!("{:012}", nth + 1),
                    ));
                }
                params
            },
            400,
            "InvalidParameterValue",
        ),
        (
            "more product codes than one call may add",
            {
                let mut params = vec![p("FpgaImageId", fpga_image_id.clone())];
                for nth in 0..=MAX_ATTRIBUTE_MODIFICATIONS {
                    params.push(p(
                        &format!("ProductCode.{}", nth + 1),
                        format!("code{nth:06}"),
                    ));
                }
                params
            },
            400,
            "InvalidParameterValue",
        ),
    ];

    let held = {
        let store = engine.state::<Ec2State>();
        let guard = store.lock();
        guard
            .fpga_images
            .get(&fpga_image_id)
            .and_then(|record| record.view.name.clone())
    };

    for (label, params, status_wanted, code_wanted) in rows {
        let (status, body) = op(&router, "ModifyFpgaImageAttribute", &params).await;
        assert_eq!(
            status, status_wanted,
            "case {label:?}: expected HTTP {status_wanted}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, code_wanted,
            "case {label:?}: expected {code_wanted}, got {code:?} in {body}"
        );
        let read = described(label, &router, &fpga_image_id, "name").await;
        let reported = attribute_member(label, &read, "name");
        assert_eq!(
            reported, held,
            "case {label:?}: the refused call must leave the name at {held:?}, got {reported:?}"
        );
    }
}

/// Rule: the not-found refusal is worded with EC2's common missing-id sentence, naming the
/// resource type and the id the REQUEST sent — the sentence the attribute operations answer,
/// whichever of the two is asked. WHICH ids reach that refusal is each operation's own
/// reading of the id form, so each row names an id its operation looks up: the modify reads
/// an AFI id by SHAPE, so the all-zero placeholder is looked up, where the describe reads it
/// by the stricter issued-id form and refuses that value before any lookup.
#[tokio::test]
async fn the_not_found_sentence_is_the_one_the_attribute_operations_word() {
    let (router, _engine) = super::ec2_query::emulator();

    for (action, fpga_image_id, extra) in [
        (
            "ModifyFpgaImageAttribute",
            UNKNOWN_FPGA_IMAGE_ID,
            vec![p("Name", "whatever")],
        ),
        (
            "DescribeFpgaImageAttribute",
            "afi-0123456789abcdef0",
            vec![p("Attribute", "name")],
        ),
    ] {
        let mut params = vec![p("FpgaImageId", fpga_image_id)];
        params.extend(extra);
        let (status, body) = op(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {action:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "InvalidFpgaImageID.NotFound",
            "case {action:?}: expected InvalidFpgaImageID.NotFound, got {code:?} in {body}"
        );
        let message = error_message(&body);
        let expected = format!("The fpga-image ID '{fpga_image_id}' does not exist");
        assert_eq!(
            message, expected,
            "case {action:?}: expected message {expected:?}, got {message:?}"
        );
    }
}
