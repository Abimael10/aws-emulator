// SPDX-License-Identifier: CC0-1.0

//! A parameter's TAG SET: which ids address it, and what a put does to it.
//!
//! Two rules, each driven over a set of inputs rather than over one parameter:
//!
//! 1. **Every spelling of a parameter's id addresses the one parameter.** A tag request
//!    names a resource by `(ResourceType, ResourceId)` and for `Parameter` that id is "the
//!    name of the resource" — which the service reads the way every read of a parameter
//!    reads "the name or Amazon Resource Name (ARN) of the parameter": the name with its
//!    leading hierarchy separator, the name without it, and the parameter's own ARN all name
//!    the one parameter, so all three report, write and erase the one tag set. An id that
//!    selects a VERSION (`name:version`, `name:label`) names no taggable resource, because
//!    the tag set belongs to the parameter; nor does an ARN of another account or Region, nor
//!    the parameter's name under another `ResourceType`. Those are the modelled
//!    `InvalidResourceId`, which carries no message at all.
//! 2. **Tags belong to the parameter, not to a version.** A put that supplies no tags — the
//!    member absent, or present and carrying nothing — leaves the set exactly as it was,
//!    whatever else the put changes; and a put that supplies tags beside `Overwrite` is
//!    refused `ValidationException` with the service's own fixed sentence, having created and
//!    changed nothing.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`call_in_region`]) dispatches `POST /`
//! with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router — and every
//! parameter is created through `PutParameter` first, so nothing here can pass on a response
//! that is not computed from what a write persisted.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    call, call_in_region, error_code, error_message, router, ACCOUNT, REGION,
};

/// What a put that carries both `Overwrite` and tags is refused with: AWS's own fixed text
/// for the conflict, with no part of it taken from the request.
///
/// Written out here on purpose rather than imported from the handler's own constant
/// (`crate::state`'s sibling, `src/ops/put_parameter.rs`'s `TAGS_WITH_OVERWRITE_REJECTION`),
/// so this assertion fails if the handler's wording drifts away from AWS's.
const TAGS_WITH_OVERWRITE_REFUSAL: &str = "Invalid request: tags and overwrite can't be used \
                                           together. To create a parameter with tags, please \
                                           remove overwrite flag. To update tags for an existing \
                                           parameter, please use AddTagsToResource or \
                                           RemoveTagsFromResource.";

/// A second Region, so an ARN is shown to be read against the endpoint it was sent to.
const OTHER_REGION: &str = "eu-central-1";

/// An account that is not this endpoint's, for the same reason.
const OTHER_ACCOUNT: &str = "210987654321";

/// The tag set a parameter is created with, in the key order a report states it in.
fn created_tags() -> Value {
    json!([
        { "Key": "owner", "Value": "alpha" },
        { "Key": "tier", "Value": "gold" },
    ])
}

/// The parameter name without its leading hierarchy separator — the form the store keys a
/// parameter by, and the form its ARN carries after `parameter/`
/// (`service_com_amazonaws_ssm_amazonssm::state::canonical_name`).
fn bare(name: &str) -> &str {
    name.strip_prefix('/').unwrap_or(name)
}

/// The ARN of the parameter `name` names at [`REGION`]'s endpoint.
fn parameter_arn(name: &str) -> String {
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}", bare(name))
}

/// The three id forms that name the parameter `name`: `(case, id)`.
fn id_forms(name: &str) -> [(&'static str, String); 3] {
    [
        (
            "the name with its leading separator",
            format!("/{}", bare(name)),
        ),
        ("the name without its leading separator", bare(name).to_string()),
        ("the parameter's ARN", parameter_arn(name)),
    ]
}

/// Create the parameter `name` carrying [`created_tags`], and report the ARN `GetParameter`
/// gives it back — the ARN a client actually holds for it.
async fn parameter_with_tags(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        router,
        "PutParameter",
        &json!({ "Name": name, "Value": "v1", "Type": "String", "Tags": created_tags() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "{name} must be created with its tags, got {status} {created}"
    );
    let (status, read) = call(router, "GetParameter", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "{name} must read back, got {status} {read}"
    );
    let arn = read["Parameter"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("GetParameter reports an ARN for {name}, got {read}"))
        .to_string();
    assert_eq!(
        arn,
        parameter_arn(name),
        "the ARN a client holds for {name} is the one this endpoint mints"
    );
    arn
}

/// One tag call of each operation of the plane, for the pair a row names.
async fn tag_calls(
    router: &MultiServiceRouter,
    resource_type: &str,
    resource_id: &str,
) -> Vec<(&'static str, (u16, Value))> {
    vec![
        (
            "AddTagsToResource",
            call(
                router,
                "AddTagsToResource",
                &json!({
                    "ResourceType": resource_type,
                    "ResourceId": resource_id,
                    "Tags": [{ "Key": "owner", "Value": "beta" }],
                }),
            )
            .await,
        ),
        (
            "ListTagsForResource",
            call(
                router,
                "ListTagsForResource",
                &json!({ "ResourceType": resource_type, "ResourceId": resource_id }),
            )
            .await,
        ),
        (
            "RemoveTagsFromResource",
            call(
                router,
                "RemoveTagsFromResource",
                &json!({
                    "ResourceType": resource_type,
                    "ResourceId": resource_id,
                    "TagKeys": ["owner"],
                }),
            )
            .await,
        ),
    ]
}

/// The `TagList` the pair reports, asserting the read was served.
async fn listed(router: &MultiServiceRouter, case: &str, resource_id: &str) -> Value {
    let (status, response) = call(
        router,
        "ListTagsForResource",
        &json!({ "ResourceType": "Parameter", "ResourceId": resource_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTagsForResource {resource_id} must be served, got {status} \
         {response}"
    );
    response["TagList"].clone()
}

/// RULE: every published spelling of a parameter's id reports the ONE tag set that
/// parameter carries. The rows differ along the shape of the name — top level, hierarchical,
/// deep, and one carrying every symbol a parameter name may hold — and each is asked for by
/// all three forms.
#[tokio::test]
async fn every_spelling_of_a_parameters_id_reports_its_one_tag_set() {
    let router = router();

    // (case, the name the parameter was created under)
    let rows: [(&str, &str); 5] = [
        ("a top-level name written bare", "TopLevelTagged"),
        ("a top-level name written with a separator", "/AlsoTopLevel"),
        ("a hierarchical name", "/Tagged/svc/key"),
        (
            "a name carrying every symbol a name may hold",
            "/Tagged/node_1.alpha-beta",
        ),
        (
            "a deep hierarchy",
            "/Tagged/one/two/three/four/five/leaf",
        ),
    ];

    for (case, name) in rows {
        let arn = parameter_with_tags(&router, name).await;
        for (form, resource_id) in id_forms(name) {
            let reported = listed(&router, case, &resource_id).await;
            assert_eq!(
                reported,
                created_tags(),
                "case {case:?} by {form}: expected the create-time tags, got {reported}"
            );
        }
        // The same resolution writes: a tag added through the ARN is a tag the bare name
        // reports, and a key removed through the no-separator spelling is gone from the ARN's
        // own report. One resource, so one tag set.
        let (status, added) = call(
            &router,
            "AddTagsToResource",
            &json!({
                "ResourceType": "Parameter",
                "ResourceId": arn,
                "Tags": [{ "Key": "zone", "Value": "west" }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a tag written through the ARN must be accepted, got {status} \
             {added}"
        );
        let reported = listed(&router, case, bare(name)).await;
        assert_eq!(
            reported,
            json!([
                { "Key": "owner", "Value": "alpha" },
                { "Key": "tier", "Value": "gold" },
                { "Key": "zone", "Value": "west" },
            ]),
            "case {case:?}: the bare name must report the tag the ARN wrote, got {reported}"
        );
        let (status, removed) = call(
            &router,
            "RemoveTagsFromResource",
            &json!({
                "ResourceType": "Parameter",
                "ResourceId": bare(name),
                "TagKeys": ["zone"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a key removed through the bare name must be accepted, got \
             {status} {removed}"
        );
        let reported = listed(&router, case, &arn).await;
        assert_eq!(
            reported,
            created_tags(),
            "case {case:?}: the ARN must report the key the bare name removed as gone, got \
             {reported}"
        );
    }
}

/// RULE, the other half: an id that is not a usable parameter id names no resource, so every
/// operation of the plane answers the messageless `InvalidResourceId`. The rows differ along
/// what makes the id unusable — a version selector, a label selector, an ARN naming another
/// Region or another account, an ARN of another resource type, and a name nothing created.
#[tokio::test]
async fn an_id_that_is_not_a_usable_parameter_id_is_invalid_resource_id() {
    let router = router();
    let name = "/Tagged/refusals/key";
    parameter_with_tags(&router, name).await;

    // (case, the ResourceId sent under ResourceType=Parameter)
    let rows: [(&str, String); 7] = [
        (
            "a version selector on the name",
            format!("{}:1", bare(name)),
        ),
        (
            "a label selector on the name",
            format!("{name}:production"),
        ),
        (
            "a version selector on the ARN",
            format!("{}:1", parameter_arn(name)),
        ),
        (
            "an ARN naming another Region",
            format!(
                "arn:aws:ssm:{OTHER_REGION}:{ACCOUNT}:parameter/{}",
                bare(name)
            ),
        ),
        (
            "an ARN naming another account",
            format!("arn:aws:ssm:{REGION}:{OTHER_ACCOUNT}:parameter/{}", bare(name)),
        ),
        (
            "an ARN of another resource type",
            format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/{}", bare(name)),
        ),
        (
            "a name nothing created",
            "/Tagged/refusals/absent".to_string(),
        ),
    ];

    for (case, resource_id) in rows {
        for (operation, (status, response)) in tag_calls(&router, "Parameter", &resource_id).await {
            assert_eq!(
                status, 400,
                "case {case:?}: {operation} must refuse it, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "InvalidResourceId",
                "case {case:?}: {operation} must answer the existence refusal, got {response}"
            );
            assert_eq!(
                error_message(&response),
                "",
                "case {case:?}: {operation} must carry no message, got {response}"
            );
        }
    }

    // The parameter's own name under another resource TYPE names no resource of that type
    // either, and the refusal is the same one.
    for (operation, (status, response)) in tag_calls(&router, "Document", bare(name)).await {
        assert_eq!(
            status, 400,
            "a parameter name under the Document type: {operation} must refuse it, got \
             {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidResourceId",
            "a parameter name under the Document type: {operation} must answer the existence \
             refusal, got {response}"
        );
    }

    // None of the refusals touched the parameter: its tag set is the one it was created with,
    // read back through every spelling.
    for (form, resource_id) in id_forms(name) {
        let reported = listed(&router, form, &resource_id).await;
        assert_eq!(
            reported,
            created_tags(),
            "by {form}: the refused calls must have left the tags as they were, got {reported}"
        );
    }

    // And the endpoint an id is read through is the Region's own: the very same well-formed
    // id names no parameter elsewhere.
    let (status, elsewhere) = call_in_region(
        &router,
        OTHER_REGION,
        "ListTagsForResource",
        &json!({ "ResourceType": "Parameter", "ResourceId": parameter_arn(name) }),
    )
    .await;
    assert_eq!(
        status, 400,
        "another Region holds no such parameter, got {status} {elsewhere}"
    );
    assert_eq!(
        error_code(&elsewhere),
        "InvalidResourceId",
        "another Region's endpoint must answer the existence refusal, got {elsewhere}"
    );
}

/// RULE: a put that supplies no tags leaves the parameter's tag set exactly as it was. The
/// rows differ along what the overwrite DOES change — the value alone, the value and the
/// type, the value and a description, the tier — and along whether `Tags` is absent or
/// present and empty, which is the same "no tags supplied".
#[tokio::test]
async fn create_time_tags_survive_every_overwrite_that_supplies_none() {
    let router = router();
    let name = "/Tagged/rotated/key";
    let arn = parameter_with_tags(&router, name).await;

    // (case, the version the overwrite mints, the members it carries besides Name and
    // Overwrite)
    let rows: [(&str, u64, Value); 6] = [
        ("the value alone", 2, json!({ "Value": "v2" })),
        (
            "the value and the type",
            3,
            json!({ "Value": "v3", "Type": "String" }),
        ),
        (
            "an empty Tags list",
            4,
            json!({ "Value": "v4", "Type": "String", "Tags": [] }),
        ),
        (
            "an empty Tags list and nothing else",
            5,
            json!({ "Value": "v5", "Tags": [] }),
        ),
        (
            "a description",
            6,
            json!({ "Value": "v6", "Description": "rotated again" }),
        ),
        ("a tier", 7, json!({ "Value": "v7", "Tier": "Advanced" })),
    ];

    for (case, expected_version, members) in rows {
        let mut body = members.clone();
        body["Name"] = json!(name);
        body["Overwrite"] = json!(true);
        let (status, written) = call(&router, "PutParameter", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the overwrite must be accepted, got {status} {written}"
        );
        // The overwrite really happened: the version it minted is the next one, and the
        // value it wrote is what a read reports.
        assert_eq!(
            written["Version"].as_u64(),
            Some(expected_version),
            "case {case:?}: expected version {expected_version}, got {written}"
        );
        let (status, read) = call(&router, "GetParameter", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the parameter must read back, got {status} {read}"
        );
        assert_eq!(
            read["Parameter"]["Value"],
            members["Value"],
            "case {case:?}: expected the value the overwrite wrote, got {read}"
        );
        // And the tag set is untouched, through every spelling of the parameter's id.
        for (form, resource_id) in id_forms(name) {
            let reported = listed(&router, case, &resource_id).await;
            assert_eq!(
                reported,
                created_tags(),
                "case {case:?} by {form}: expected the create-time tags, got {reported}"
            );
        }
    }

    // A tag written through the tag plane survives the next rotation too: the set is the
    // parameter's, not any one version's.
    let (status, added) = call(
        &router,
        "AddTagsToResource",
        &json!({
            "ResourceType": "Parameter",
            "ResourceId": arn,
            "Tags": [{ "Key": "owner", "Value": "beta" }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the tag plane must accept the change, got {status} {added}"
    );
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Value": "v8", "Overwrite": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the rotation after the tag change must be accepted, got {status} {written}"
    );
    let reported = listed(&router, "after a tag change", name).await;
    assert_eq!(
        reported,
        json!([
            { "Key": "owner", "Value": "beta" },
            { "Key": "tier", "Value": "gold" },
        ]),
        "the overwrite must keep the tag set the tag plane left, got {reported}"
    );
}

/// RULE: a put that supplies tags beside `Overwrite` is refused `ValidationException` with
/// the service's own fixed sentence, for an existing name and a new one alike, and nothing
/// of either is created or changed. The rows differ along whether the name exists and along
/// how many tags the request carries.
#[tokio::test]
async fn tags_beside_overwrite_are_refused_and_change_nothing() {
    let router = router();
    let existing = "/Tagged/conflict/key";
    parameter_with_tags(&router, existing).await;
    let absent = "/Tagged/conflict/absent";

    // (case, the name, the tags the request carries)
    let rows: [(&str, &str, Value); 4] = [
        (
            "one tag on an existing name",
            existing,
            json!([{ "Key": "owner", "Value": "beta" }]),
        ),
        (
            "two tags on an existing name",
            existing,
            json!([{ "Key": "owner", "Value": "beta" }, { "Key": "zone", "Value": "west" }]),
        ),
        (
            "one tag on a name nothing created",
            absent,
            json!([{ "Key": "owner", "Value": "beta" }]),
        ),
        (
            "a tag whose key the name does not carry",
            existing,
            json!([{ "Key": "zone", "Value": "west" }]),
        ),
    ];

    for (case, name, tags) in rows {
        let (status, response) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": name,
                "Value": "rejected",
                "Type": "String",
                "Overwrite": true,
                "Tags": tags,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the conflict must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected the validation refusal, got {response}"
        );
        let message = error_message(&response);
        assert_eq!(
            message, TAGS_WITH_OVERWRITE_REFUSAL,
            "case {case:?}: expected {TAGS_WITH_OVERWRITE_REFUSAL:?}, got {message:?}"
        );
    }

    // The existing parameter kept its value, its single version and its tags.
    let (status, read) = call(&router, "GetParameter", &json!({ "Name": existing })).await;
    assert_eq!(
        status, 200,
        "the existing parameter must still read back, got {status} {read}"
    );
    assert_eq!(
        read["Parameter"]["Value"], json!("v1"),
        "the refused puts must have written no value, got {read}"
    );
    assert_eq!(
        read["Parameter"]["Version"].as_u64(),
        Some(1),
        "the refused puts must have minted no version, got {read}"
    );
    let reported = listed(&router, "an existing name", existing).await;
    assert_eq!(
        reported,
        created_tags(),
        "the refused puts must have left the tags as they were, got {reported}"
    );

    // And the new name was not created: nothing holds a parameter under it.
    let (status, missing) = call(&router, "GetParameter", &json!({ "Name": absent })).await;
    assert_eq!(
        status, 400,
        "the refused put must have created nothing, got {status} {missing}"
    );
    assert_eq!(
        error_code(&missing),
        "ParameterNotFound",
        "a name nothing created must be reported as absent, got {missing}"
    );
    let (status, untagged) = call(
        &router,
        "ListTagsForResource",
        &json!({ "ResourceType": "Parameter", "ResourceId": absent }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the refused put must have written no tag set either, got {status} {untagged}"
    );
    assert_eq!(
        error_code(&untagged),
        "InvalidResourceId",
        "a name nothing created names no taggable resource, got {untagged}"
    );
}
