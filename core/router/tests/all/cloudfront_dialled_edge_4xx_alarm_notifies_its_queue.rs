// SPDX-License-Identifier: CC0-1.0

//! The 4xx alerting chain driven by a viewer that DIALS the address a CloudFront distribution
//! published as its `DomainName` — a socket of its own, no router dispatch and no credential —
//! through the WHOLE mounted service set, the one `core/router/src/lib.rs` assembles for the
//! binary.
//!
//! RULE: **the address a service publishes for a client to dial is an address this emulator
//! itself answers on, and traffic served there is traffic like any other.
//! `Distribution$DomainName` is "the domain name that CloudFront assigns when you create a
//! distribution", the one name a viewer is handed, so the value a create reports is a
//! `host:port` a client on the machine this emulator runs on can open a connection to; every
//! answer the edge serves for a request that arrives there is counted in the `AWS/CloudFront`
//! namespace against that distribution — dimensioned by its own `DistributionId` beside
//! `Region=Global` — with the 4xx error rate carrying the share of the period's requests
//! answered in that status class, so a metric alarm over that series whose threshold the share
//! crosses transitions to ALARM, publishes its SNS alarm action and the queue subscribed to
//! that topic receives the notification; a distribution whose published address nothing dialled
//! produces no datapoint, so its own alarm stays `OK` under `TreatMissingData: notBreaching`.**
//!
//! Only a cross-service suite whose viewer holds a SOCKET can state this. The published
//! address and the edge are CloudFront's, the origin object is Amazon S3's, the series and the
//! evaluation are CloudWatch's, the fan-out is SNS's and what arrived is SQS's — and the
//! publish CloudWatch makes is an in-process call addressed the way a client's own `sns`-scoped
//! call is, so only the production service set can show the whole chain.
//! [`super::cloudfront_edge_metrics_drive_the_alarm_chain`] drives the same chain with the
//! viewer request DISPATCHED in process with the published name set as its `Host`, which says
//! nothing about whether that name is an address at all;
//! [`super::cloudfront_dialled_edge_address_over_a_private_s3_origin`] dials the published
//! address but reads no metric; and the CloudFront crate's own `dialled_edge_address_metrics`
//! suite dials one and reads the series without the alarm, the topic or the queue behind it.
//! The dial, the origin pipeline and the alarm wiring are each reused from those rather than
//! built again here.
//!
//! The rows differ along what the rule generalises over: the share of the dialled burst that is
//! answered 4xx — the whole burst, most of it, and a quarter of it, which is just past the
//! alarm's threshold — beside the untouched control distribution whose alarm carries the same
//! configuration and whose published address nothing dials. The sibling
//! [`super::cloudfront_edge_4xx_share_decides_the_alarm`] suite drives shares that do NOT
//! breach the threshold, including the bound the comparison excludes.
//!
//! Every expected value is DERIVED from the answers the dials received — their status class and
//! how many there were — so no row states a rate of its own. The distribution's id and its
//! published address are CloudFront's to mint: what is asserted about the address is its FORM
//! (a `host:port` a client can dial), that `GetDistribution` reports the same one, and what it
//! answers.

use std::time::Instant;

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudfront_dialled_edge_address_over_a_private_s3_origin::{dial, is_dialable};
use crate::cloudfront_edge_metrics_drive_the_alarm_chain::{
    alarm_state, emulator, given_alarm, now_secs, reported_rate, ALARM_THRESHOLD, CHAIN_DEADLINE,
    REGION,
};
use crate::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, create_bucket, create_distribution, create_origin_access_control,
    policy, put_bucket_policy, put_object, API_VERSION, CLOUDFRONT_HOST, CLOUDFRONT_PRINCIPAL,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    drain, given_topic_with_subscriber, Grant,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero (the id
/// CloudFront's own managed cache policy catalogue publishes for it): no request of a burst is
/// absorbed by a cache hit, so each one is answered — and counted — on its own.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The origin bucket every distribution of this suite takes its objects from, the object it
/// holds and the key it does not.
const BUCKET: &str = "cloudfront-dialled-alarm-origin";
const PRESENT_KEY: &str = "present.txt";
const PRESENT_BYTES: &str = "the object the dialled address serves";
const MISSING_KEY: &str = "absent.txt";

/// The whole percentage a rate carries when every one of its observations is of its class,
/// which is what the share of the dialled answers is computed against.
const WHOLE_PERCENT: f64 = 100.0;

/// How close the reported share has to be to the share the dials received: the fold is exact
/// arithmetic over whole requests, so the slack is only against the last bit of a division.
const TOLERANCE: f64 = 1e-9;

/// The state an alarm the burst breached reports, and the one an alarm nothing fed keeps.
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// One row: how many of the dialled burst ask for a key the origin does not hold, and how many
/// ask for the one it does.
struct Row {
    case: &'static str,
    missing: usize,
    present: usize,
}

/// The rows: shares of the burst answered 4xx, from the whole burst down to a quarter of it,
/// which is the smallest share here still past the alarm's threshold.
const ROWS: &[Row] = &[
    Row {
        case: "every dialled request answered 4xx",
        missing: 2,
        present: 0,
    },
    Row {
        case: "two of three dialled requests answered 4xx",
        missing: 2,
        present: 1,
    },
    Row {
        case: "a quarter of the dialled requests answered 4xx, just past the threshold",
        missing: 2,
        present: 6,
    },
];

/// The case the untouched control distribution is named by.
const CONTROL_CASE: &str = "a distribution whose published address nothing dials";

/// What one row left behind for the dialling phase: the distribution it created, the address
/// that create published, and the alarm and queue its traffic is graded through.
struct Prepared<'row> {
    row: &'row Row,
    id: String,
    domain: String,
    alarm: String,
    queue: String,
}

/// The `DomainName` `GetDistribution` reports for `id` — the address the distribution
/// publishes, read back through the control plane.
async fn published_domain(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let read = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetDistribution({id}) must answer, got {} with {:?}",
        read.status,
        read.text()
    );
    read.element("DomainName")
}

/// Create one distribution over the shared origin, holding the address it published to the one
/// rule about it: a `host:port` a client on this machine can DIAL.
async fn subject(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    control: &str,
) -> (String, String) {
    let (id, domain, _arn) = create_distribution(
        router,
        case,
        caller_reference,
        BUCKET,
        control,
        CACHING_DISABLED,
    )
    .await;
    assert!(
        is_dialable(&domain),
        "case {case:?}: expected a host:port a viewer on this machine can dial as the published \
         DomainName, got {domain:?}"
    );
    (id, domain)
}

/// Dial `domain` for every request of `row`'s burst and answer how many of those answers were
/// in the 4xx class — read off the answers themselves, which is what the series is compared
/// against.
async fn dialled_burst(case: &str, domain: &str, row: &Row) -> usize {
    let mut four_xx = 0;
    for request in 0..row.missing {
        let served = dial(case, domain, &format!("/{MISSING_KEY}")).await;
        assert!(
            (400..500).contains(&served.status),
            "case {case:?}: dialled request {request} for a key the origin does not hold must be \
             answered 4xx, got {} with {:?}",
            served.status,
            served.body
        );
        four_xx += 1;
    }
    for request in 0..row.present {
        let served = dial(case, domain, &format!("/{PRESENT_KEY}")).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: dialled request {request} for the key the origin holds must be \
             answered 200, got {} with {:?}",
            served.status, served.body
        );
        assert_eq!(
            served.body, PRESENT_BYTES,
            "case {case:?}: dialled request {request} must be served the origin object's own \
             bytes, got {:?}",
            served.body
        );
    }
    four_xx
}

/// Look again at `queue` until the alarm's notification has arrived, or fail the case once
/// [`CHAIN_DEADLINE`] has passed. The wait is on what ARRIVED — each look is a long poll of the
/// queue — never on a span of the clock.
async fn awaited_notification(
    router: &MultiServiceRouter,
    case: &str,
    queue: &str,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + CHAIN_DEADLINE;
    let mut delivered: Vec<Value> = Vec::new();
    while delivered.is_empty() {
        delivered.extend(drain(router, case, REGION, queue).await);
        assert!(
            !delivered.is_empty() || Instant::now() < giving_up_at,
            "case {case:?}: the alarm's action never reached its queue"
        );
    }
    delivered
}

#[tokio::test]
async fn a_4xx_burst_dialled_at_the_published_address_fires_the_alarm_to_its_queue() {
    let engine = aws_emulator_router::new_engine();
    // The setup plane carries a credential scope without a computed signature; the VIEWER
    // below presents no credential at all and never goes through a router — it opens a socket
    // to the address the create published.
    let router = emulator(&engine, false);

    // The origin: one bucket holding one object, granting the CloudFront service principal the
    // read its origin access control makes. Nothing grants a listing, so a request for a key
    // the bucket does not hold is answered by the origin's own refusal, which is a 4xx.
    create_bucket(&router, CONTROL_CASE, BUCKET).await;
    put_object(&router, CONTROL_CASE, BUCKET, PRESENT_KEY, PRESENT_BYTES, &[]).await;
    put_bucket_policy(
        &router,
        CONTROL_CASE,
        BUCKET,
        &policy(
            CLOUDFRONT_PRINCIPAL,
            "s3:GetObject",
            &format!("arn:aws:s3:::{BUCKET}/*"),
            None,
        ),
    )
    .await;
    let control = create_origin_access_control(&router, CONTROL_CASE, "dialled-alarm", "always").await;

    // Every row's distribution and the untouched control are created first, so the propagation
    // the clock owes them is waited out once for all of them.
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let (id, domain) = subject(
            &router,
            row.case,
            &format!("dialled-alarm-subject-{index}"),
            &control,
        )
        .await;
        prepared.push(Prepared {
            row,
            id,
            domain,
            alarm: format!("dialled-edge-4xx-subject-{index}"),
            queue: String::new(),
        });
    }
    let (quiet_id, quiet_domain) =
        subject(&router, CONTROL_CASE, "dialled-alarm-control", &control).await;
    for row in &prepared {
        awaited_deployment(&router, row.row.case, &row.id).await;
    }
    awaited_deployment(&router, CONTROL_CASE, &quiet_id).await;

    // Each alarm publishes to its OWN topic and queue, so the control's silence is read where a
    // client reads it rather than inferred from a shared queue.
    for row in &mut prepared {
        let (topic, queue) = given_topic_with_subscriber(
            &router,
            row.row.case,
            REGION,
            &row.alarm,
            Grant::Unscoped,
        )
        .await;
        given_alarm(&router, row.row.case, &row.alarm, &row.id, &topic).await;
        row.queue = queue;
    }
    let quiet_alarm = "dialled-edge-4xx-control";
    let (quiet_topic, quiet_queue) =
        given_topic_with_subscriber(&router, CONTROL_CASE, REGION, quiet_alarm, Grant::ScopedToTopic)
            .await;
    given_alarm(&router, CONTROL_CASE, quiet_alarm, &quiet_id, &quiet_topic).await;

    // Every row's burst is dialled before any row's chain is waited for: the evaluation the
    // clock owes grades the alarms of one period together, so the rows share one wait instead
    // of each paying for an evaluation of its own.
    for row in &prepared {
        let case = row.row.case;
        // The address a READ reports is the one the create published: the name is the
        // distribution's, not the create response's.
        let read_back = published_domain(&router, case, &row.id).await;
        assert_eq!(
            read_back, row.domain,
            "case {case:?}: expected GetDistribution to report the published address {:?}, got \
             {read_back:?}",
            row.domain
        );

        // The instant THIS row's burst is served at, which is what its own series is read
        // against.
        let at = now_secs();
        let four_xx = dialled_burst(case, &row.domain, row.row).await;
        let requests = row.row.missing + row.row.present;

        // What the edge counted is the share of the dialled burst that was answered 4xx.
        #[allow(clippy::cast_precision_loss)]
        let expected = WHOLE_PERCENT * four_xx as f64 / requests as f64;
        let reported = reported_rate(&router, case, &row.id, at).await;
        assert!(
            (reported - expected).abs() < TOLERANCE,
            "case {case:?}: expected the traffic dialled at the published address to be reported \
             as a 4xx rate of {expected}, got {reported}"
        );
        assert!(
            reported > ALARM_THRESHOLD,
            "case {case:?}: the dialled burst must breach the alarm's threshold \
             ({ALARM_THRESHOLD}), got {reported}"
        );
    }

    for row in &prepared {
        let case = row.row.case;
        // The chain: the evaluation the clock owes moves the alarm, its action publishes, and
        // the subscribed queue receives the notification — with no call forcing any of it.
        let delivered = awaited_notification(&router, case, &row.queue).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the alarm's action must reach its queue once per transition, got \
             {delivered:?}"
        );
        assert_eq!(
            delivered[0]["AlarmName"].as_str(),
            Some(row.alarm.as_str()),
            "case {case:?}: the notification must name the alarm, got {}",
            delivered[0]
        );
        assert_eq!(
            delivered[0]["NewStateValue"].as_str(),
            Some(ALARM),
            "case {case:?}: the notification must carry the state moved to, got {}",
            delivered[0]
        );
        let state = alarm_state(&router, case, &row.alarm).await;
        assert_eq!(
            state, ALARM,
            "case {case:?}: the alarm the dialled burst breached must report {ALARM}, got \
             {state:?}"
        );
    }

    // The untouched control: its address was published — a client could have dialled it — and
    // nothing did, so its series holds nothing. Read once every row's own chain has completed,
    // which is the evidence that the evaluation has run over this account's alarms.
    assert!(
        is_dialable(&quiet_domain),
        "case {CONTROL_CASE:?}: expected a dialable published DomainName, got {quiet_domain:?}"
    );
    let quiet_state = alarm_state(&router, CONTROL_CASE, quiet_alarm).await;
    assert_eq!(
        quiet_state, OK,
        "case {CONTROL_CASE:?}: the untouched distribution's alarm must stay {OK}, got \
         {quiet_state:?}"
    );
    let withheld = drain(&router, CONTROL_CASE, REGION, &quiet_queue).await;
    assert!(
        withheld.is_empty(),
        "case {CONTROL_CASE:?}: nothing may be delivered for an alarm that never fired, got \
         {withheld:?}"
    );
}
