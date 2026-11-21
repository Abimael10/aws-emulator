// SPDX-License-Identifier: CC0-1.0

//! The image a launch boots has to be an AMI the account really has, driven over the wire
//! this service speaks (awsJson1_0, `POST /`,
//! `X-Amz-Target: EUCMIFrontendAPIService.<OperationName>`).
//!
//! RULE under test: `ManagedInstance.ImageId` names an Amazon Machine Image of the
//! caller's own account in the Region the call was signed for — `ManagedInstanceRequest`
//! carries the parameters of an EC2 launch
//! (`smithy_query(query="shape",shape="ManagedInstanceRequest")`) — so a
//! `CreateWorkspaceInstance` whose `ImageId` resolves to no image the account can see is
//! refused at request time with the modelled `ValidationException` (HTTP 400,
//! `smithy_query(query="operation",operation="CreateWorkspaceInstance")`) and creates
//! nothing: no instance id is minted, no record is written and the `ClientToken` the call
//! carried is not spent.
//!
//! The rows span what the rule generalises over: the id FORMS the modelled `ImageId`
//! (`^ami-[0-9a-zA-Z]{1,63}$`, `smithy_query(query="shape",shape="ImageId")`) admits —
//! EC2's long and short id forms, a length between them, an upper-case one and the
//! shortest the model allows — all of them ids no image in this account has.
//!
//! The positive side is what makes the check real rather than a blanket refusal: an image
//! the account holds launches, and an AMI REGISTERED through `ec2:RegisterImage` during
//! the test launches too — so the answer follows Amazon EC2's own image state rather than
//! anything this crate keeps.
//!
//! The image state is spanned in BOTH directions: an AMI the account stops holding (one
//! deregistered into the Recycle Bin, which "is therefore NOT one of the images the account
//! can see") stops being launchable, and the same AMI restored through
//! `ec2:RestoreImageFromRecycleBin` becomes launchable again under the same id — so what a
//! create is accepted for is the IMAGE, at the moment of the call, and not the shape of its
//! id or any answer this service remembers. A refused create is also read back through the
//! TAGS this service reports, not through the id set alone: no instance carrying the tags
//! the refused call sent exists afterwards.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_instance_propagation::ec2_client;
use crate::workspace_instance_lifecycle::{
    call, create, error_code, harness, list_all_ids, ok, stored_tags, TEST_IMAGE_ID,
    TEST_INSTANCE_TYPE,
};

/// The `Message` AWS answers a create whose image cannot be resolved with: fixed text in
/// full, echoing none of what the caller sent — the offending id included.
const INVALID_IMAGE_MESSAGE: &str = "Invalid AMI ID provided";

/// The HTTP status the model gives `ValidationException`
/// (`smithy_query(query="shape",shape="ValidationException")` → `httpError 400`).
const VALIDATION_STATUS: u16 = 400;

/// A create body booting `image_id`, with `token` as its `ClientToken`.
fn create_body(image_id: &str, token: &str) -> Value {
    json!({
        "ClientToken": token,
        "ManagedInstance": { "ImageId": image_id, "InstanceType": TEST_INSTANCE_TYPE },
    })
}

/// The id forms the modelled `ImageId` admits, none of which names an image this account
/// has: EC2's long form (17 hexadecimal digits), its short form (8), a length between the
/// two, an upper-case id, and the shortest id the modelled pattern allows.
fn unresolvable_image_ids() -> Vec<(&'static str, &'static str)> {
    vec![
        ("the long id form", "ami-0000000000deadbee"),
        ("another long id form", "ami-0fedfedfedfedfed0"),
        ("the short id form", "ami-12345678"),
        ("a length between the two id forms", "ami-0123456789"),
        ("an upper-case id", "ami-ABCDEF0123456789A"),
        ("the shortest id the model admits", "ami-z"),
    ]
}

#[tokio::test]
async fn a_create_naming_an_image_the_account_has_not_got_is_refused() {
    for (case, image_id) in unresolvable_image_ids() {
        // One store per row, so what the store holds afterwards is this row's doing.
        let (router, _engine) = harness();
        // One instance the account really has, so the id set a refusal must leave
        // untouched is not the empty one.
        let existing = create(&router, create_body(TEST_IMAGE_ID, "tok-existing")).await;

        let token = "tok-refused";
        let (status, payload) =
            call(&router, "CreateWorkspaceInstance", create_body(image_id, token)).await;
        assert_image_refused(case, image_id, status, &payload);

        // Nothing was created for it: the id set is the one the accepted create left.
        let ids = list_all_ids(&router, json!({})).await;
        assert_eq!(
            ids,
            vec![existing.clone()],
            "case {case:?} ({image_id:?}): expected no instance for the refused create, got \
             {ids:?}"
        );

        // And the refused call spent no `ClientToken`: the same token now carries a create
        // this service can launch, which it could not if the refusal had claimed it.
        let created = create(&router, create_body(TEST_IMAGE_ID, token)).await;
        assert_ne!(
            created, existing,
            "case {case:?}: expected the token to be unspent and mint a new instance"
        );
        let mut ids = list_all_ids(&router, json!({})).await;
        ids.sort();
        let mut expected = vec![existing, created];
        expected.sort();
        assert_eq!(
            ids, expected,
            "case {case:?}: expected exactly the two accepted creates, got {ids:?}"
        );
    }
}

#[tokio::test]
async fn a_create_naming_an_image_the_account_holds_launches() {
    let (router, _engine) = harness();

    // The images the account's own Amazon EC2 holds, each launched and read back.
    for image_id in crate::workspace_instance_lifecycle::ACCOUNT_IMAGES {
        let id = create(&router, create_body(image_id, &format!("tok-{image_id}"))).await;
        let got = ok(
            &router,
            "GetWorkspaceInstance",
            json!({ "WorkspaceInstanceId": &id }),
        )
        .await;
        assert_eq!(
            got["WorkspaceInstanceId"].as_str(),
            Some(id.as_str()),
            "case image {image_id:?}: expected the created instance to read back, got {got}"
        );
    }
}

#[tokio::test]
async fn an_ami_registered_through_ec2_becomes_launchable() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);

    // An AMI the account did not have until this call: `ec2:RegisterImage` mints the id,
    // which is why the test cannot know it in advance.
    let registered = ec2
        .register_image()
        .name("workspaces-instances-launchable-image")
        // "Either imageLocation or rootDeviceName must be specified" — the register names
        // the root device of an EBS-backed AMI.
        .root_device_name("/dev/xvda")
        .send()
        .await
        .expect("ec2:RegisterImage answers")
        .image_id()
        .expect("a registered image carries its id")
        .to_string();

    // A create naming it is accepted, and the instance reads back — so the resolution
    // follows EC2's image state rather than a fixed list inside this service.
    let id = create(&router, create_body(&registered, "tok-registered")).await;
    let got = ok(
        &router,
        "GetWorkspaceInstance",
        json!({ "WorkspaceInstanceId": &id }),
    )
    .await;
    assert_eq!(
        got["WorkspaceInstanceId"].as_str(),
        Some(id.as_str()),
        "case the registered image {registered:?}: expected the instance to read back, got {got}"
    );

    // A well-formed id of the same form that no register minted is still refused: what the
    // create above was accepted for is the registered IMAGE, not the shape of its id.
    let unregistered = "ami-0999999999999999a";
    assert_ne!(
        unregistered, registered,
        "case the unregistered id: the fixture must not name the registered image"
    );
    let (status, payload) = call(
        &router,
        "CreateWorkspaceInstance",
        create_body(unregistered, "tok-unregistered"),
    )
    .await;
    assert_image_refused("the unregistered id", unregistered, status, &payload);
}

/// The whole refusal a create whose image cannot be resolved is answered with: the modelled
/// `ValidationException` at HTTP 400, carrying the fixed message in full and echoing none of
/// the submitted id.
fn assert_image_refused(case: &str, image_id: &str, status: u16, payload: &Value) {
    assert_eq!(
        status, VALIDATION_STATUS,
        "case {case:?} ({image_id:?}): expected HTTP {VALIDATION_STATUS}, got {status} with \
         {payload}"
    );
    assert_eq!(
        error_code(payload),
        "ValidationException",
        "case {case:?} ({image_id:?}): expected ValidationException, got {payload}"
    );
    let message = payload["Message"]
        .as_str()
        .or_else(|| payload["message"].as_str());
    assert_eq!(
        message,
        Some(INVALID_IMAGE_MESSAGE),
        "case {case:?} ({image_id:?}): expected {INVALID_IMAGE_MESSAGE:?}, got {payload}"
    );
    assert!(
        !payload.to_string().contains(image_id),
        "case {case:?}: the refusal must echo none of the submitted id, got {payload}"
    );
}

/// A create body booting `image_id` with `token` as its `ClientToken`, carrying the one
/// caller tag `Name=<marker>`: the marker is what makes "this call created nothing" readable
/// through the tags the service reports for the instances it holds, not through the id set
/// alone.
fn tagged_create_body(image_id: &str, token: &str, marker: &str) -> Value {
    json!({
        "ClientToken": token,
        "ManagedInstance": { "ImageId": image_id, "InstanceType": TEST_INSTANCE_TYPE },
        "Tags": [{ "Key": "Name", "Value": marker }],
    })
}

/// The `Name` tag of every WorkSpace Instance this service reports, read back through
/// `ListWorkspaceInstances` (to the last page) and `ListTagsForResource`.
async fn reported_name_markers(router: &MultiServiceRouter) -> Vec<String> {
    let mut markers = Vec::new();
    for id in list_all_ids(router, json!({})).await {
        if let Some(marker) = stored_tags(router, &id).await.get("Name") {
            markers.push(marker.clone());
        }
    }
    markers.sort();
    markers
}

#[tokio::test]
async fn a_refused_create_creates_no_instance_carrying_its_tags() {
    for (case, image_id) in unresolvable_image_ids() {
        // One store per row, so what the store holds afterwards is this row's doing.
        let (router, _engine) = harness();
        // One instance the account really has, carrying a marker of its own: the tags the
        // refusal must leave untouched are not the empty set.
        let held_marker = format!("held-{case}");
        let held = create(
            &router,
            tagged_create_body(TEST_IMAGE_ID, "tok-held", &held_marker),
        )
        .await;

        let refused_marker = format!("refused-{case}");
        let (status, payload) = call(
            &router,
            "CreateWorkspaceInstance",
            tagged_create_body(image_id, "tok-refused", &refused_marker),
        )
        .await;
        assert_image_refused(case, image_id, status, &payload);

        // No instance carries the marker the refused call sent, and the one the accepted
        // create made still carries its own.
        let markers = reported_name_markers(&router).await;
        assert_eq!(
            markers,
            vec![held_marker.clone()],
            "case {case:?} ({image_id:?}): expected only the accepted create's tag, got \
             {markers:?}"
        );
        let tags = stored_tags(&router, &held).await;
        assert_eq!(
            tags.get("Name").map(String::as_str),
            Some(held_marker.as_str()),
            "case {case:?}: expected the held instance to keep its own tag, got {tags:?}"
        );
    }
}

/// How long a deregistered AMI is held in the Recycle Bin in the rows below: one day, a
/// retention period a Recycle Bin rule can carry, so the entry is still restorable at the
/// end of the row however slowly the host runs it.
const RECYCLE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// Deregister the AMI `image_id` into the account's Recycle Bin, where it is no longer one
/// of the images the account can see.
///
/// Driven through Amazon EC2's own store because `ec2:DeregisterImage` is still a scaffold
/// (`issues/things-that-do-not-work/20261023T102725Z-ec2-deregisterimage-stub.md`, which is
/// what moves this helper onto the operation once it lands): the image leaves that service's
/// image store either way, which is the state this row needs — and the row reads the effect
/// back through `ec2:RestoreImageFromRecycleBin`, which is the operation wired to the bin.
fn deregister_into_recycle_bin(engine: &Arc<engine::InMemoryEngine>, case: &str, image_id: &str) {
    let store = ec2_service::state::service_state(&**engine)
        .expect("the in-memory engine carries the account's EC2 store");
    let moved = store.lock().recycle_image(image_id, RECYCLE_RETENTION);
    assert!(
        moved,
        "case {case:?}: expected the AMI {image_id:?} to move into the Recycle Bin"
    );
}

/// How the AMI a row takes away came to be one of the images the account can see: one it
/// already held when the row started, and one `ec2:RegisterImage` minted during the row at an
/// id the test cannot know in advance. The rule turns on the image EXISTING at the moment of
/// the call, so the rows differ in how it came to exist.
enum RecyclableImage {
    /// One of the AMIs the account's own Amazon EC2 already holds, named by its id.
    AlreadyHeld(&'static str),
    /// An AMI registered during the row, under this name.
    RegisteredDuringTheRow(&'static str),
}

fn recyclable_image_rows() -> Vec<(&'static str, RecyclableImage)> {
    vec![
        (
            "an AMI the account already held",
            // Not the id this suite's other fixtures boot from, so taking it away cannot
            // disturb them.
            RecyclableImage::AlreadyHeld(crate::workspace_instance_lifecycle::ACCOUNT_IMAGES[1]),
        ),
        (
            "an AMI registered during the row",
            RecyclableImage::RegisteredDuringTheRow("workspaces-instances-recyclable-image"),
        ),
    ]
}

/// The id of this row's AMI, registering it first where the row asks for one it did not hold.
async fn recyclable_image_id(ec2: &::aws_sdk_ec2::Client, image: &RecyclableImage) -> String {
    match image {
        RecyclableImage::AlreadyHeld(image_id) => (*image_id).to_string(),
        RecyclableImage::RegisteredDuringTheRow(name) => ec2
            .register_image()
            .name(*name)
            // "Either imageLocation or rootDeviceName must be specified" — the register
            // names the root device of an EBS-backed AMI.
            .root_device_name("/dev/xvda")
            .send()
            .await
            .expect("ec2:RegisterImage answers")
            .image_id()
            .expect("a registered image carries its id")
            .to_string(),
    }
}

#[tokio::test]
async fn an_image_the_account_stops_holding_stops_being_launchable() {
    for (index, (case, image)) in recyclable_image_rows().into_iter().enumerate() {
        // One store per row, so what the account holds is this row's doing.
        let (router, engine) = harness();
        let ec2 = ec2_client(&router);
        let registered = recyclable_image_id(&ec2, &image).await;

        // While the account holds it, a create booting it is accepted and reads back.
        let launched =
            create(&router, create_body(&registered, &format!("tok-held-{index}"))).await;
        let got = ok(
            &router,
            "GetWorkspaceInstance",
            json!({ "WorkspaceInstanceId": &launched }),
        )
        .await;
        assert_eq!(
            got["WorkspaceInstanceId"].as_str(),
            Some(launched.as_str()),
            "case {case:?} ({registered:?}): expected the instance to read back, got {got}"
        );

        // Once it is deregistered it is no image the account can see, so the SAME id — one
        // a create was accepted for moments ago — is refused, and nothing is created for it.
        deregister_into_recycle_bin(&engine, case, &registered);
        let (status, payload) = call(
            &router,
            "CreateWorkspaceInstance",
            create_body(&registered, &format!("tok-recycled-{index}")),
        )
        .await;
        assert_image_refused(case, &registered, status, &payload);
        let ids = list_all_ids(&router, json!({})).await;
        assert_eq!(
            ids,
            vec![launched.clone()],
            "case {case:?} ({registered:?}): expected no instance for the refused create, got \
             {ids:?}"
        );

        // And restored through EC2's own operation it is launchable again, under the same
        // id: the create follows the account's image state at the moment of the call.
        ec2.restore_image_from_recycle_bin()
            .image_id(&registered)
            .send()
            .await
            .expect("ec2:RestoreImageFromRecycleBin answers");
        let relaunched = create(
            &router,
            create_body(&registered, &format!("tok-restored-{index}")),
        )
        .await;
        assert_ne!(
            relaunched, launched,
            "case {case:?}: expected the restored image to mint a second instance"
        );
        let got = ok(
            &router,
            "GetWorkspaceInstance",
            json!({ "WorkspaceInstanceId": &relaunched }),
        )
        .await;
        assert_eq!(
            got["WorkspaceInstanceId"].as_str(),
            Some(relaunched.as_str()),
            "case {case:?} ({registered:?}): expected the second instance to read back, got \
             {got}"
        );
    }
}
