// SPDX-License-Identifier: CC0-1.0

//! The EVENTS a cluster records about its own objects, read over the address the
//! cluster itself publishes.
//!
//! The RULE under test: **what a component of the cluster does to an object is
//! recorded as an `Event` of that object's namespace, and a client reads those
//! events back from `/api/v1/namespaces/{namespace}/events` — narrowed by the fields
//! the API server indexes an event by (`involvedObject.name`, `reason`, `type`,
//! `source`, its own name), and refused for a field label it does not index.** A
//! pod the cluster could place on no node is the one the SCHEDULER records for here:
//! `FailedScheduling`, with the sentence the pod's own `PodScheduled` condition
//! carries — so the event and the object tell one story.
//!
//! The rows differ along what the rule generalises over: the namespace the pod was
//! created in (the one Kubernetes creates with a cluster and one the client created),
//! how many containers it declares, and — for the listing — which field the client
//! selected on. The negative rows are the two a client can reach: a field label the
//! server does not index, and an event name no event of the namespace carries.
//!
//! What the events of a pod a node DID take look like — the image pull its kubelet
//! made and what the registry answered — is asserted where a node exists at all,
//! which needs the services a node group's capacity lives in:
//! `core/router/tests/all/eks_pod_image_pull_events.rs`.

use serde_json::{json, Value};

use crate::certificate_authority::REGION;
use crate::kubernetes_api::{dial, provisioned_cluster, Kubeconfig};
use crate::wire::emulator;

/// The reason the scheduler records for a pod it could place on no node, and the
/// sentence it words it in — the one the pod's own condition carries.
const FAILED_SCHEDULING: &str = "FailedScheduling";
const NO_NODES_MESSAGE: &str = "no nodes available to schedule pods";

/// The component that records a scheduling decision.
const SCHEDULER: &str = "default-scheduler";

/// One pod this suite creates, and what the cluster must have recorded about it.
struct Row {
    case: &'static str,
    namespace: &'static str,
    pod: &'static str,
    containers: usize,
}

/// The rows: two namespaces and two container counts, so neither the namespace an
/// event is recorded in nor the shape of the pod decides whether it is recorded.
const ROWS: &[Row] = &[
    Row {
        case: "a pod of the namespace a cluster is created with",
        namespace: "default",
        pod: "events-default",
        containers: 1,
    },
    Row {
        case: "a pod of a namespace the client created",
        namespace: "events-ns",
        pod: "events-created",
        containers: 2,
    },
    Row {
        case: "a pod of several containers",
        namespace: "default",
        pod: "events-many",
        containers: 3,
    },
];

/// A pod manifest naming `containers` containers, each running an image no registry
/// this emulator holds answers for — what is under test here is the SCHEDULING the
/// cluster recorded, and this cluster has no node to schedule onto at all.
fn manifest(pod: &str, containers: usize) -> Value {
    let containers: Vec<Value> = (0..containers)
        .map(|index| json!({ "name": format!("app-{index}"), "image": "registry.example.com/app:1" }))
        .collect();
    json!({ "metadata": { "name": pod }, "spec": { "containers": containers } })
}

/// The events of `namespace`, as a client lists them — with `query` appended to the
/// collection's path when the client narrowed the listing.
async fn events(config: &Kubeconfig, case: &str, namespace: &str, query: &str) -> Vec<Value> {
    let path = format!("/api/v1/namespaces/{namespace}/events{query}");
    let answered = dial(config, case, "GET", &path, None).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: the events of {namespace:?} must be served, got {answered:?}"
    );
    assert_eq!(
        answered.body["kind"], json!("EventList"),
        "case {case:?}: the collection is an EventList, got {}",
        answered.body
    );
    answered.body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn the_cluster_records_what_its_scheduler_did_to_a_pod_and_lists_it_back() {
    let router = emulator();
    let config = provisioned_cluster(&router, REGION, "pod-events-host").await;

    for row in ROWS {
        let Row {
            case,
            namespace,
            pod,
            containers,
        } = row;
        if *namespace != "default" {
            // Created once per name; a second create of the same namespace is the
            // already-exists refusal, which says the namespace is there either way.
            let created = dial(
                &config,
                case,
                "POST",
                "/api/v1/namespaces",
                Some(&json!({ "metadata": { "name": namespace } })),
            )
            .await;
            assert!(
                matches!(created.status, 201 | 409),
                "case {case:?}: the namespace must be there, got {created:?}"
            );
        }
        let created = dial(
            &config,
            case,
            "POST",
            &format!("/api/v1/namespaces/{namespace}/pods"),
            Some(&manifest(pod, *containers)),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: the pod must be created, got {created:?}"
        );
        let uid = created.body["metadata"]["uid"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the pod must carry a uid, got {created:?}"))
            .to_string();

        // THE SCHEDULER'S OWN RECORD: the pod could be placed on no node, which is
        // what the event says and what the pod's condition says.
        let listed = events(&config, case, namespace, "").await;
        let recorded = listed
            .iter()
            .find(|event| {
                event["involvedObject"]["uid"] == json!(uid)
                    && event["reason"] == json!(FAILED_SCHEDULING)
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a {FAILED_SCHEDULING} event, got {listed:?}")
            });
        assert_eq!(
            recorded["message"], json!(NO_NODES_MESSAGE),
            "case {case:?}: the event carries the sentence the pod's condition does, got \
             {recorded}"
        );
        assert_eq!(
            recorded["type"], json!("Warning"),
            "case {case:?}: a pod nothing could place is a warning, got {recorded}"
        );
        assert_eq!(
            recorded["source"]["component"], json!(SCHEDULER),
            "case {case:?}: the scheduler recorded it, got {recorded}"
        );
        assert_eq!(
            recorded["involvedObject"]["kind"], json!("Pod"),
            "case {case:?}: the event is about the pod, got {recorded}"
        );
        assert_eq!(
            recorded["involvedObject"]["name"], json!(pod),
            "case {case:?}: the event names the pod, got {recorded}"
        );
        assert_eq!(
            recorded["involvedObject"]["namespace"], json!(namespace),
            "case {case:?}: the event names the pod's namespace, got {recorded}"
        );
        assert_eq!(
            recorded["count"], json!(1),
            "case {case:?}: one thing happened, so the event reports one occurrence, got \
             {recorded}"
        );
        let name = recorded["metadata"]["name"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the event must be named, got {recorded}"))
            .to_string();
        assert!(
            name.starts_with(&format!("{pod}.")),
            "case {case:?}: an event is named after the object it is about, got {name:?}"
        );

        // The pod's own condition tells the same story as the event.
        let read = dial(
            &config,
            case,
            "GET",
            &format!("/api/v1/namespaces/{namespace}/pods/{pod}"),
            None,
        )
        .await;
        let condition = read.body["status"]["conditions"]
            .as_array()
            .and_then(|conditions| {
                conditions
                    .iter()
                    .find(|condition| condition["type"] == json!("PodScheduled"))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case:?}: expected a PodScheduled condition, got {read:?}"));
        assert_eq!(
            condition["message"], json!(NO_NODES_MESSAGE),
            "case {case:?}: the condition and the event word one answer, got {condition}"
        );

        // A READ CHANGES NOTHING: listing again reports the same events under the
        // same names rather than recording one per read.
        let again = events(&config, case, namespace, "").await;
        assert_eq!(
            again.len(),
            listed.len(),
            "case {case:?}: a read records no event, got {again:?} after {listed:?}"
        );

        // NARROWED BY THE FIELDS THE SERVER INDEXES: each selector answers the
        // events it names and no others.
        let selectors: [(&str, String, bool); 4] = [
            (
                "the pod's own events",
                format!("involvedObject.name={pod}"),
                true,
            ),
            ("the reason", format!("reason={FAILED_SCHEDULING}"), true),
            (
                "the component that reported it",
                format!("source={SCHEDULER}"),
                true,
            ),
            (
                "a type no event of this pod carries",
                "type=Normal".to_string(),
                false,
            ),
        ];
        for (what, selector, expected) in selectors {
            let narrowed = events(&config, case, namespace, &format!("?fieldSelector={selector}"))
                .await;
            let found = narrowed
                .iter()
                .any(|event| event["metadata"]["name"] == json!(name));
            assert_eq!(
                found, expected,
                "case {case:?} ({what}): selecting on {selector:?} must answer {expected}, got \
                 {narrowed:?}"
            );
            for event in &narrowed {
                let (field, value) = selector.split_once('=').expect("a field selector");
                let held = match field {
                    "involvedObject.name" => event["involvedObject"]["name"].clone(),
                    "reason" => event["reason"].clone(),
                    "source" => event["source"]["component"].clone(),
                    "type" => event["type"].clone(),
                    other => panic!("case {case:?}: unexpected field {other:?}"),
                };
                assert_eq!(
                    held, json!(value),
                    "case {case:?} ({what}): every event the answer carries satisfies the \
                     selector, got {event}"
                );
            }
        }

        // ONE event by name, and the two refusals a client can reach.
        let one = dial(
            &config,
            case,
            "GET",
            &format!("/api/v1/namespaces/{namespace}/events/{name}"),
            None,
        )
        .await;
        assert_eq!(
            one.status, 200,
            "case {case:?}: the event must be readable by name, got {one:?}"
        );
        assert_eq!(
            one.body["metadata"]["name"], json!(name),
            "case {case:?}: the read answers that event, got {one:?}"
        );
        let missing = dial(
            &config,
            case,
            "GET",
            &format!("/api/v1/namespaces/{namespace}/events/{pod}.0000000000000000"),
            None,
        )
        .await;
        assert_eq!(
            missing.status, 404,
            "case {case:?}: an event the namespace does not hold is not found, got {missing:?}"
        );
        let unindexed = dial(
            &config,
            case,
            "GET",
            &format!("/api/v1/namespaces/{namespace}/events?fieldSelector=spec.nodeName%3Dnode"),
            None,
        )
        .await;
        assert_eq!(
            unindexed.status, 400,
            "case {case:?}: a field label the server does not index is refused, got \
             {unindexed:?}"
        );
        assert_eq!(
            unindexed.body["message"], json!("field label not supported: spec.nodeName"),
            "case {case:?}: the refusal names the label, got {unindexed:?}"
        );
    }

    // The core group's resource list names `events`, so a client resolves the kind
    // before it addresses the collection above.
    let resources = dial(&config, "the core resource list", "GET", "/api/v1", None).await;    let events_resource = resources.body["resources"]
        .as_array()
        .and_then(|resources| {
            resources
                .iter()
                .find(|resource| resource["name"] == json!("events"))
                .cloned()
        })
        .unwrap_or_else(|| panic!("the resource list must name events, got {resources:?}"));
    assert_eq!(
        events_resource["kind"], json!("Event"),
        "the row names the kind, got {events_resource}"
    );
    assert_eq!(
        events_resource["namespaced"], json!(true),
        "an event belongs to a namespace, got {events_resource}"
    );

    // EVERY collection this server lists is held to the fields ITS registry indexes:
    // a label one serves narrows that collection, and a label it does not serve is
    // refused there — whichever collection it is.
    let collections: [(&str, &str, &str, &str); 5] = [
        (
            "the pods of a namespace",
            "/api/v1/namespaces/default/pods",
            "status.phase=Pending",
            "status.phase=Pending,metadata.annotations=x",
        ),
        (
            "the events of a namespace",
            "/api/v1/namespaces/default/events",
            "reason=FailedScheduling",
            "spec.nodeName=node",
        ),
        (
            "the config maps of a namespace",
            "/api/v1/namespaces/default/configmaps",
            "metadata.name=absent",
            "data.key=value",
        ),
        (
            "the service accounts of a namespace",
            "/api/v1/namespaces/default/serviceaccounts",
            "metadata.name=default",
            "secrets=x",
        ),
        (
            "the namespaces of the cluster",
            "/api/v1/namespaces",
            "metadata.name=default",
            "metadata.namespace=default",
        ),
    ];
    for (case, path, served, unindexed) in collections {
        let narrowed = dial(
            &config,
            case,
            "GET",
            &format!("{path}?fieldSelector={}", served.replace('=', "%3D")),
            None,
        )
        .await;
        assert_eq!(
            narrowed.status, 200,
            "case {case:?}: a field label the collection serves narrows it, got {narrowed:?}"
        );
        let refused = dial(
            &config,
            case,
            "GET",
            &format!("{path}?fieldSelector={}", unindexed.replace('=', "%3D")),
            None,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a field label the collection does not index is refused rather \
             than narrowing it, got {refused:?}"
        );
        let label = unindexed
            .rsplit(',')
            .next()
            .and_then(|term| term.split('=').next())
            .unwrap_or_default();
        assert_eq!(
            refused.body["message"],
            json!(format!("field label not supported: {label}")),
            "case {case:?}: the refusal names the label, got {refused:?}"
        );
    }
}
