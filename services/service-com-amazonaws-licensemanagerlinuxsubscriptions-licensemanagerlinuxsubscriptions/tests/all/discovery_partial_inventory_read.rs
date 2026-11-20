// SPDX-License-Identifier: CC0-1.0

//! A discovery pass that read PART of a source Region's EC2 inventory records that part,
//! and leaves the rest of the Region for a later pass.
//!
//! The rule under test: a read the owner answered in part established that part, so the
//! instances the answered pages carried are recorded — merged, never replacing what an
//! earlier pass found in the Region, because this one did not establish what it never
//! reached — and the Region keeps the unread remainder as the point a later pass resumes
//! it; only a read the owner answered NOT AT ALL is evidence of nothing, and is the one
//! outcome a pass records nothing for and goes on past. Whether the pages that were not
//! answered ran out, were refused, or were never asked for makes no difference to the pages
//! that WERE: a pass that discards them reports an inventory the account does not have, and
//! one that calls the Region finished leaves its remainder unread.
//!
//! The rows span what the rule generalises over: WHICH hop of a Region's inventory walk is
//! the one that is not answered — the first (nothing was established), the second (one page
//! was), a later one (several pages were), and none of them (the whole inventory was) —
//! each asserted over the instances the discovery list reports, the stopping point the pass
//! recorded, and what the NEXT report then discovers.
//!
//! The instances and the AMIs are EC2's own, launched and published exactly as in
//! `tests/all/discovery_from_ec2.rs`, on the same one engine. What this suite puts in front
//! of EC2 is only HOW IT ANSWERS the pass's hops ([`PagedEc2`]): it serves each page of the
//! inventory at a small page size — a page smaller than the `MaxResults` asked for, plus a
//! token for the rest, is an answer `DescribeInstances` documents ("the token to include in
//! another request to get the next page of items") — and refuses ONE named hop with EC2's
//! own throttle, which is the hop that cannot be arranged from the client side of a read.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use serde_json::json;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, Service,
};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::DiscoveryResume;
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::ComAmazonawsLicensemanagerlinuxsubscriptionsLicensemanagerlinuxsubscriptionsState as DiscoveryState;

use crate::discovery_from_ec2::{reported_instance_ids, Fleet, ENABLED, RHEL};
use crate::harness::REGION;

/// The routing key of the inventory read this suite answers for: EC2's `ec2Query` requests
/// name their operation in the `Action` field of the form-encoded body.
const DESCRIBE_INSTANCES: &str = "DescribeInstances";
/// The field that names it.
const ACTION_FIELD: &str = "Action";
/// The page-size field of that read (`DescribeInstancesRequest$MaxResults`), which
/// [`with_page_size`] answers a smaller page for.
const MAX_RESULTS_FIELD: &str = "MaxResults";

/// The code EC2 throttles a read with, which is one of the ways a hop of a walk can fail
/// after an earlier hop of the same walk was answered.
const THROTTLE_CODE: &str = "RequestLimitExceeded";

/// How many items of a Region's inventory each answered page carries, so a fleet one
/// `RunInstances` call launches still spans several pages.
const PAGE: usize = 5;

/// How many instances the discovered Region holds: three pages at [`PAGE`] items each, the
/// last of them short, so a row can leave the walk unanswered at its first hop, in the
/// middle of it, or not at all.
const FLEET: usize = 12;

/// Which hops of the inventory walk are answered, shared between the test and the service
/// in front of EC2 so a row can state the hop that is not.
#[derive(Debug)]
struct InventoryHops {
    /// The items each answered page carries.
    page_size: usize,
    /// The hop that is refused, counted from one; `None` refuses none of them.
    refused: Option<usize>,
    /// Whether the inventory reads arriving now are the PASS's hops. A row's setup reads
    /// the inventory itself — it waits for the state EC2's clock owes its launches — and
    /// those reads are neither counted nor answered differently: until a row arms this, EC2
    /// answers its own `DescribeInstances` whole.
    armed: AtomicBool,
    /// How many hops have arrived so far.
    seen: AtomicUsize,
}

impl InventoryHops {
    fn new(page_size: usize, refused: Option<usize>) -> Self {
        Self {
            page_size,
            refused,
            armed: AtomicBool::new(false),
            seen: AtomicUsize::new(0),
        }
    }

    /// Count the inventory reads from here on as the pass's hops.
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// Whether the reads arriving now are the pass's hops.
    fn armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    /// Count one arriving hop and answer whether it is the refused one.
    fn refuses_this_hop(&self) -> bool {
        let hop = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        self.refused == Some(hop)
    }

    /// How many hops have arrived.
    fn seen(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }
}

/// EC2, with the pass's inventory walk answered a page at a time and one hop of it refused.
///
/// Its identity, protocol and operations are EC2's own — the definition is the real
/// service's, so every other operation the pass calls (the AMI reads) is served by EC2
/// itself — and only the inventory read is answered through [`InventoryHop`].
struct PagedEc2 {
    inner: ec2::EmulatedService<engine::InMemoryEngine>,
    hop: InventoryHop,
}

impl Service for PagedEc2 {
    fn definition(&self) -> &dyn ErasedServiceDefinition {
        self.inner.definition()
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        self.inner.can_handle(parts)
    }

    fn override_operation(&self, req: &RtRequest) -> Option<&dyn ErasedOperation> {
        let action = form_field(req, ACTION_FIELD);
        (action.as_deref() == Some(DESCRIBE_INSTANCES)).then_some(&self.hop as &dyn ErasedOperation)
    }
}

/// One hop of the inventory walk: refused when it is the hop the row named, and otherwise
/// answered by EC2's own `DescribeInstances` over a smaller page.
struct InventoryHop {
    hops: Arc<InventoryHops>,
}

impl ErasedOperation for InventoryHop {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        let armed = self.hops.armed();
        let refused = armed && self.hops.refuses_this_hop();
        let page_size = self.hops.page_size;
        Box::pin(async move {
            if refused {
                return throttled();
            }
            let describe = svc
                .definition()
                .routing_table()
                .get(DESCRIBE_INSTANCES)
                .expect("EC2 models the inventory read");
            if !armed {
                // A read of the row's own setup: EC2's own answer, whole.
                return describe.run(svc, req, ctx).await;
            }
            let paged = with_page_size(req, page_size);
            describe.run(svc, &paged, ctx).await
        })
    }
}

/// The value of the form-encoded field `name` in `req`'s body, if it carries one.
fn form_field(req: &RtRequest, name: &str) -> Option<String> {
    let body = req.body().bytes()?;
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|field| field.split_once('='))
        .find(|(field, _)| *field == name)
        .map(|(_, value)| value.to_string())
}

/// `req` with its page size replaced by `page_size`, so the owner answers a page of that
/// many items and a token for the rest of the inventory.
fn with_page_size(req: &RtRequest, page_size: usize) -> RtRequest {
    let body = String::from_utf8_lossy(req.body().bytes().unwrap_or_default()).into_owned();
    let paged: String = body
        .split('&')
        .map(|field| match field.split_once('=') {
            Some((name, _)) if name == MAX_RESULTS_FIELD => {
                format!("{MAX_RESULTS_FIELD}={page_size}")
            }
            _ => field.to_string(),
        })
        .collect::<Vec<String>>()
        .join("&");
    let mut smaller = RtRequest::new(SdkBody::from(paged.clone()));
    smaller.set_uri(req.uri()).expect("EC2's own request URI");
    smaller
        .set_method(req.method())
        .expect("EC2's own request method");
    for (name, value) in req.headers().iter() {
        smaller
            .headers_mut()
            .append(name.to_string(), value.to_string());
    }
    // The body this request carries is the one the server reads, so its declared length is
    // the length of that body.
    smaller
        .headers_mut()
        .insert("content-length", paged.len().to_string());
    smaller
}

/// EC2 throttling one hop of the walk, in its own error envelope.
fn throttled() -> RtResponse {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <Response><Errors><Error><Code>{THROTTLE_CODE}</Code>\
         <Message>Request limit exceeded.</Message></Error></Errors></Response>"
    );
    let mut throttled = RtResponse::new(
        StatusCode::try_from(503).expect("a throttle is a 503"),
        SdkBody::from(body),
    );
    throttled
        .headers_mut()
        .insert("content-type", "text/xml".to_string());
    throttled
}

/// The stopping point a row's pass must record.
#[derive(Debug)]
enum Stopped {
    /// Nothing at all: the pass left no unread inventory behind — it either read its whole
    /// plan, or established nothing about a Region, which the next pass then reads from the
    /// beginning of its inventory.
    Nowhere,
    /// Inside the Region, at the cursor its unread remainder begins at: the pass established
    /// part of that inventory and did not reach the rest.
    InsideTheRegion,
}

/// One row: the hop of the Region's inventory walk that is not answered, and what the pass
/// that ran into it must leave behind.
struct Row {
    case: &'static str,
    /// The hop refused, counted from one.
    refused: Option<usize>,
    /// How many hops the pass must make before it stops.
    hops: usize,
    /// How many of the Region's instances, in the order EC2 pages them, the pass must have
    /// recorded.
    recorded: usize,
    stopped: Stopped,
}

#[tokio::test]
async fn a_pass_records_the_inventory_pages_the_owner_answered() {
    let rows = vec![
        Row {
            case: "the first hop of the walk is refused",
            refused: Some(1),
            hops: 1,
            // Not one page was answered, so the pass established nothing about the Region.
            recorded: 0,
            stopped: Stopped::Nowhere,
        },
        Row {
            case: "the second hop of the walk is refused",
            refused: Some(2),
            hops: 2,
            recorded: PAGE,
            stopped: Stopped::InsideTheRegion,
        },
        Row {
            case: "a later hop of the walk is refused",
            refused: Some(3),
            hops: 3,
            recorded: 2 * PAGE,
            stopped: Stopped::InsideTheRegion,
        },
        Row {
            case: "every hop of the walk is answered",
            refused: None,
            hops: 3,
            recorded: FLEET,
            stopped: Stopped::Nowhere,
        },
    ];

    for row in rows {
        let case = row.case;
        let hops = Arc::new(InventoryHops::new(PAGE, row.refused));
        let fleet = Fleet::with_ec2({
            let hops = Arc::clone(&hops);
            move |inner| PagedEc2 {
                inner,
                hop: InventoryHop { hops },
            }
        });
        let image_id = fleet.publish_image(RHEL);
        // Launch order IS instance-id order, which is the order EC2 pages its inventory in,
        // so the sorted ids are the pages the walk reads in the order it reads them.
        let mut launched = fleet.launch_now(REGION, &image_id, FLEET).await;
        launched.sort();
        // The instance list is of the RUNNING instances, so the fleet is given EC2's own
        // clock to bring the launches up before the pass reads the inventory. Those reads
        // are the row's own, not the pass's hops, which is why the paging and the refusal
        // are armed only now.
        for instance_id in &launched {
            fleet.settled_running(REGION, instance_id).await;
        }
        hops.arm();
        fleet.set_discovery(ENABLED, &[REGION]).await;

        // ONE report, which materializes ONE pass.
        let reported = {
            let mut reported = reported_instance_ids(&fleet.instances(&json!({})).await, case);
            reported.sort();
            reported
        };
        let expected: Vec<String> = launched[..row.recorded].to_vec();
        assert_eq!(
            reported, expected,
            "case {case:?}: the pass must record the {} instances of the pages EC2 \
             answered, got {reported:?}",
            row.recorded
        );
        assert_eq!(
            hops.seen(),
            row.hops,
            "case {case:?}: the pass must make {} inventory hops, it made {}",
            row.hops,
            hops.seen()
        );

        // What the pass recorded about where it stopped, which is what decides whether the
        // Region's unread remainder is read next or the Region counts as finished.
        let stopped = fleet
            .h
            .engine
            .state::<DiscoveryState>()
            .lock()
            .discovery_resume
            .clone();
        match row.stopped {
            Stopped::Nowhere => assert!(
                stopped.is_none(),
                "case {case:?}: a pass that left no unread inventory records no stopping \
                 point, got {stopped:?}"
            ),
            Stopped::InsideTheRegion => {
                let inside = matches!(
                    &stopped,
                    Some(DiscoveryResume::Inside(region, cursor))
                        if region == REGION && cursor.is_some()
                );
                assert!(
                    inside,
                    "case {case:?}: the Region still holds unread inventory, so the pass \
                     must stop inside it at the cursor that remainder begins at, got \
                     {stopped:?}"
                );
            }
        }

        // And the remainder is read: the next report's pass takes the Region up where this
        // one stopped, so the whole inventory is discovered however the walk was cut short.
        let later = {
            let mut later = reported_instance_ids(&fleet.instances(&json!({})).await, case);
            later.sort();
            later
        };
        assert_eq!(
            later, launched,
            "case {case:?}: a later pass must discover the whole inventory of the Region, \
             got {later:?}"
        );
    }
}
