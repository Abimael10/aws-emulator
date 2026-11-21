// SPDX-License-Identifier: CC0-1.0

//! The DOCUMENTS a record of an EC2 call carries: the parameters it was dispatched with,
//! what it answered, and the resources it acted on.
//!
//! RULE: CloudTrail records a call in the SERVING API's own document form — each member
//! under the name that API's wire protocol gives it, a repeated member held in one member
//! whose elements sit under `items` — and names in the event's `Resources` each resource
//! the call acted on, by the resource type AWS publishes for it beside that resource's own
//! id. For EC2, whose protocol is a query/XML dialect, the wire name of every member is the
//! one the model carries as its `@xmlName`
//! (`smithy_query(query="shape",shape="Reservation,Instance,InstanceStateChange,InstanceState")`
//! -> `instancesSet`, `instanceState`, `currentState`, `subnetId`, `reservationId`), which
//! is why a launch's record reports `instancesSet.items[0].instanceId` where the SDK's
//! output names `Instances[0].InstanceId`.
//!
//! The rows below span the shapes the rule generalises over rather than the one launch that
//! filed it: a scalar member, a repeated member of ids, a repeated member of structures
//! that nests a repeated member of its own, a repeated member held under a name of its own
//! (`tags`), a response that reports a list of structures with a nested structure, an
//! operation that answers nothing at all, and a read, which acts on no resource and answers
//! no `responseElements`. Each row names the record it is about, the path through that
//! record and the value the path must hold.
//!
//! The records are read out of the shared log the recorder writes into
//! (`service_common::management_events::ManagementEventLog`, the store CloudTrail's
//! `LookupEvents` serves from): the envelope half of a lookup — `Resources` as
//! `ResourceType`/`ResourceName` pairs — is what that lookup projects from the same
//! records, so this suite asserts the record and `core/router/tests/all/ec2_management_events.rs`
//! the lookup over it.

use serde_json::{json, Value};
use service_common::management_events::{ManagementEventLog, ManagementEvent};

use super::ec2_query::{call, element};

/// The Region every call here is signed for, and so the Region its records live in.
///
/// Shared with the suites beside this one that drive a call through [`served`], so a record
/// and the catalog a case resolves against are read in the ONE Region.
pub(crate) const REGION: &str = "us-west-2";

/// The CIDR of the VPC these calls launch into.
const VPC_CIDR: &str = "10.42.0.0/16";

/// The CIDR of the subnet inside it.
const SUBNET_CIDR: &str = "10.42.1.0/24";

/// A `(key, value)` request parameter, as the ec2Query harness takes them.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One call's record: the JSON document a lookup over event history reports, and the
/// resource pairs that lookup's envelope names.
///
/// Shared with the suites beside this one that assert on a record of an EC2 call, so the
/// record of a call is found the one way rather than once per suite.
pub(crate) struct Recorded {
    /// The record as `LookupEvents` serves its `CloudTrailEvent`.
    pub(crate) document: Value,
    /// `(ResourceType, ResourceName)` for each resource the event names, as the envelope of
    /// a lookup reports them.
    pub(crate) resources: Vec<(String, String)>,
}

/// Call `action` and answer the record of that very call — the one naming the request id the
/// caller was answered under, which is how a client finds the record of its own call.
pub(crate) async fn served(
    router: &smithy_server_runtime::MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> (String, Recorded) {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with body {body}"
    );
    let request_id = element(&body, "requestId")
        .unwrap_or_else(|| panic!("case {case:?}: the response names no request: {body}"));
    let state = engine.state::<ManagementEventLog>();
    let found: Vec<Recorded> = {
        let log = state.lock();
        log.iter()
            .filter(|event| event.request_id == request_id)
            .map(|event| Recorded {
                document: event.to_cloudtrail_event_json(),
                resources: named_resources(event),
            })
            .collect()
    };
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one record of the call answered under \
         {request_id:?}, got {} of them",
        found.len()
    );
    let record = found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the record just counted"));
    (body, record)
}

/// The `(ResourceType, ResourceName)` pairs a lookup's envelope reports for one record.
fn named_resources(event: &ManagementEvent) -> Vec<(String, String)> {
    event
        .call
        .resources
        .iter()
        .map(|resource| {
            (
                resource.resource_type.clone(),
                resource.resource_name.clone(),
            )
        })
        .collect()
}

/// One row of the document table: the record it is about, the path through it, and the
/// value that path must hold.
struct DocumentCase {
    /// What this row is about, named in the assertion.
    case: &'static str,
    /// The record this row reads, by the name [`served`] was called with.
    record: &'static str,
    /// A JSON pointer into that record.
    path: String,
    /// What the pointer must hold.
    expected: Value,
}

/// A row stating that `path` of `record` holds `expected`.
fn row(case: &'static str, record: &'static str, path: &str, expected: Value) -> DocumentCase {
    DocumentCase {
        case,
        record,
        path: path.to_string(),
        expected,
    }
}

/// RULE: a record reports a call in the serving API's own document form, and names the
/// resources the call acted on.
#[tokio::test]
async fn a_record_reports_the_call_in_the_serving_apis_own_document_form() {
    let (router, engine) = super::ec2_query::emulator();

    // The network the launch goes into, created through the API so every id below is one
    // this account really holds.
    let (vpc_body, vpc_record) = served(
        &router,
        &engine,
        "a VPC create",
        "CreateVpc",
        &[p("CidrBlock", VPC_CIDR)],
    )
    .await;
    let vpc_id = element(&vpc_body, "vpcId")
        .unwrap_or_else(|| panic!("the create must answer a VPC id: {vpc_body}"));
    let (subnet_body, _) = served(
        &router,
        &engine,
        "a subnet create",
        "CreateSubnet",
        &[p("VpcId", vpc_id.clone()), p("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    let subnet_id = element(&subnet_body, "subnetId")
        .unwrap_or_else(|| panic!("the create must answer a subnet id: {subnet_body}"));

    // An AMI of the published catalog, resolved the way a client does.
    let (images_status, images_body) = call(&router, REGION, "DescribeImages", &[]).await;
    assert_eq!(
        images_status, 200,
        "DescribeImages must be served, got {images_status} with body {images_body}"
    );
    let image_id = element(&images_body, "imageId")
        .unwrap_or_else(|| panic!("the catalog must publish an image: {images_body}"));

    // The launch the issue is about, and the three calls that follow it.
    let (run_body, run_record) = served(
        &router,
        &engine,
        "a launch",
        "RunInstances",
        &[
            p("ImageId", image_id.clone()),
            p("InstanceType", "t3.micro"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", subnet_id.clone()),
            p("TagSpecification.1.ResourceType", "instance"),
            p("TagSpecification.1.Tag.1.Key", "Name"),
            p("TagSpecification.1.Tag.1.Value", "audited"),
        ],
    )
    .await;
    let instance_id = element(&run_body, "instanceId")
        .unwrap_or_else(|| panic!("the launch must answer an instance id: {run_body}"));
    let reservation_id = element(&run_body, "reservationId")
        .unwrap_or_else(|| panic!("the launch must answer a reservation id: {run_body}"));

    let (_, tag_record) = served(
        &router,
        &engine,
        "a tag write",
        "CreateTags",
        &[
            p("ResourceId.1", instance_id.clone()),
            p("Tag.1.Key", "Owner"),
            p("Tag.1.Value", "audit"),
        ],
    )
    .await;
    let (_, read_record) = served(
        &router,
        &engine,
        "a read narrowed by a filter",
        "DescribeInstances",
        &[
            p("Filter.1.Name", "instance-state-name"),
            p("Filter.1.Value.1", "pending"),
            p("Filter.1.Value.2", "running"),
        ],
    )
    .await;
    let (terminate_body, terminate_record) = served(
        &router,
        &engine,
        "a terminate",
        "TerminateInstances",
        &[p("InstanceId.1", instance_id.clone())],
    )
    .await;
    let current_state = element(&terminate_body, "name").unwrap_or_default();
    assert_eq!(
        current_state, "shutting-down",
        "the terminate must answer the state the record then carries, got \
         {current_state:?} in {terminate_body}"
    );

    let rows = vec![
        // The launch's parameters: the per-instance launch specification, the launch-wide
        // members beside it, and the tags it carried.
        row(
            "the launched image, in the launch specification",
            "a launch",
            "/requestParameters/instancesSet/items/0/imageId",
            json!(image_id),
        ),
        row(
            "the launch-wide instance type",
            "a launch",
            "/requestParameters/instanceType",
            json!("t3.micro"),
        ),
        row(
            "the subnet the launch named",
            "a launch",
            "/requestParameters/subnetId",
            json!(subnet_id),
        ),
        row(
            "a tag specification's own resource type",
            "a launch",
            "/requestParameters/tagSpecificationSet/items/0/resourceType",
            json!("instance"),
        ),
        row(
            "the tags a launch carried, held directly under their own member",
            "a launch",
            "/requestParameters/tagSpecificationSet/items/0/tags/0",
            json!({"key": "Name", "value": "audited"}),
        ),
        // What the launch answered, as EC2's own document names it.
        row(
            "the reservation a launch answered",
            "a launch",
            "/responseElements/reservationId",
            json!(reservation_id),
        ),
        row(
            "the instance a launch minted",
            "a launch",
            "/responseElements/instancesSet/items/0/instanceId",
            json!(instance_id),
        ),
        row(
            "the state that instance was answered in",
            "a launch",
            "/responseElements/instancesSet/items/0/instanceState/name",
            json!("pending"),
        ),
        row(
            "the subnet that instance was placed in",
            "a launch",
            "/responseElements/instancesSet/items/0/subnetId",
            json!(subnet_id),
        ),
        // The terminate: a repeated member of ids on the way in, a list of structures with
        // nested structures on the way out.
        row(
            "the instance a terminate named",
            "a terminate",
            "/requestParameters/instancesSet/items/0/instanceId",
            json!(instance_id),
        ),
        row(
            "the state the terminate answered",
            "a terminate",
            "/responseElements/instancesSet/items/0/currentState/name",
            json!(current_state),
        ),
        row(
            "the instance the terminate answered for",
            "a terminate",
            "/responseElements/instancesSet/items/0/instanceId",
            json!(instance_id),
        ),
        // A create whose output reports one structure, and one whose output has no member at
        // all: the empty document, there being nothing the call answered.
        row(
            "the VPC a create answered",
            "a VPC create",
            "/responseElements/vpc/vpcId",
            json!(vpc_id),
        ),
        row(
            "the resources a tag write named",
            "a tag write",
            "/requestParameters/resourcesSet/items/0/resourceId",
            json!(instance_id),
        ),
        row(
            "the tags a tag write carried",
            "a tag write",
            "/requestParameters/tagSet/items/0",
            json!({"key": "Owner", "value": "audit"}),
        ),
        // A read: the filter it narrowed on, nested repeated member and all, and no
        // response at all to record.
        row(
            "the filter a read narrowed on",
            "a read narrowed by a filter",
            "/requestParameters/filterSet/items/0/name",
            json!("instance-state-name"),
        ),
        row(
            "a filter's own values",
            "a read narrowed by a filter",
            "/requestParameters/filterSet/items/0/valueSet/items/1/value",
            json!("running"),
        ),
        row(
            "a read answers no response elements",
            "a read narrowed by a filter",
            "/responseElements",
            Value::Null,
        ),
    ];

    let records: Vec<(&str, &Recorded)> = vec![
        ("a VPC create", &vpc_record),
        ("a launch", &run_record),
        ("a tag write", &tag_record),
        ("a read narrowed by a filter", &read_record),
        ("a terminate", &terminate_record),
    ];
    for DocumentCase {
        case,
        record,
        path,
        expected,
    } in rows
    {
        let found = records
            .iter()
            .find(|(name, _)| *name == record)
            .map(|(_, recorded)| &recorded.document)
            .unwrap_or_else(|| panic!("case {case:?}: no record named {record:?}"));
        let actual = found.pointer(&path).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {path} of the {record} record to be {expected}, got \
             {actual} in {found}"
        );
    }

    // The resources each record names: the ones the call acted on, by the type AWS
    // publishes for them, and none at all for the read.
    let instance_resource = ("AWS::EC2::Instance".to_string(), instance_id.clone());
    let subnet_resource = ("AWS::EC2::Subnet".to_string(), subnet_id.clone());
    assert!(
        run_record.resources.contains(&instance_resource)
            && run_record.resources.contains(&subnet_resource),
        "case \"a launch\": expected the instance and its subnet among the resources, got \
         {:?}",
        run_record.resources
    );
    assert!(
        terminate_record.resources.contains(&instance_resource),
        "case \"a terminate\": expected the instance among the resources, got {:?}",
        terminate_record.resources
    );
    assert!(
        vpc_record
            .resources
            .contains(&("AWS::EC2::VPC".to_string(), vpc_id.clone())),
        "case \"a VPC create\": expected the VPC among the resources, got {:?}",
        vpc_record.resources
    );
    assert!(
        read_record.resources.is_empty(),
        "case \"a read narrowed by a filter\": a read acts on no resource, got {:?}",
        read_record.resources
    );
}
