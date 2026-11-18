// SPDX-License-Identifier: CC0-1.0

//! The Amazon SNS topics a stack's events are published to are the ones its LAST stack
//! operation left in force — read through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **the topics an event of a stack is published to are the stack's
//! `NotificationARNs` as the operation that recorded it left them — an `UpdateStack` that
//! restates the list replaces it (an explicitly empty list clears it) and one that omits it
//! leaves the list in force — and that list governs every later operation's events, the
//! `DeleteStack` that ends the stack included; the events of a window in which the stack
//! carried no topic, or carried another one, are not delivered to a topic named again
//! afterwards.**
//!
//! Three services have to agree for that to hold — CloudFormation publishes, Amazon SNS fans
//! out, Amazon SQS holds what arrived — and the publish CloudFormation makes is an in-process
//! call addressed the way a client's own `sns`-scoped call is, so WHICH mounted model claims
//! it is only decidable with every service mounted.
//!
//! [`super::cloudformation_stack_event_notifications`] pins the same rule for the operations
//! that CREATE a stack and for the execution of a change set, plus the field set one body
//! carries, and this suite reads the stream with THAT suite's harness — its wire, its
//! poll-and-drain and its readers — so the two cannot drift on how a member is encoded or on
//! what counts as delivered. What only this suite covers is the pair of operations that end a
//! stack's configuration and the stack itself: an `UpdateStack` restating, clearing or omitting
//! the list, and the `DeleteStack` after it.
//!
//! The rows differ along what the rule generalises over: what the update STATES for the list
//! (nothing at all, the empty list, a different topic) and therefore which topic the events of
//! that window are owed to.

use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_stack_event_notifications::{
    cfn_ok, cloudformation, drain_bodies, event_ids, field_of, notification_fields,
    reported_topics, POLL_INTERVAL, STACK_RESOURCE_TYPE,
};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::element;

/// The Region every request of the harness above is signed for, and so the Region the stacks
/// of this suite live in.
const REGION: &str = "us-west-2";

/// How many times a leg of THIS suite looks again before calling what it waits for
/// undelivered, [`POLL_INTERVAL`] apart. Every one of them waits for something to ARRIVE or to
/// SETTLE and stops as soon as it has, so the count is a deadline rather than a budget:
/// generous, so that a loaded machine delays a leg instead of failing it. A row here runs five
/// stack operations one after another, so the suite waits on its own deadline rather than on
/// the one the harness sizes for a leg with a single operation in it.
const LOOKS: usize = 300;

/// How long a leg that must see NOTHING keeps looking before concluding nothing came: enough
/// looks to span several of the emulator's own publishing passes, driven by the same reads that
/// make its clock-owed work due.
const QUIET_LOOKS: usize = 40;

/// One revision of the template every row deploys: one wait-condition handle (a resource no
/// other service of this emulator has to be asked about) and an output carrying `revision`, so
/// each leg submits a template that differs from the one the stack carries.
fn template(revision: &str) -> String {
    format!(
        r#"{{"Resources":{{"Marker":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
        "Outputs":{{"Revision":{{"Value":"{revision}"}}}}}}"#
    )
}

/// Read the stack once, which is what makes the transitions its clock owes due — and with them
/// the notifications their events owe. What it reports is not what a caller is after: the read
/// itself is.
async fn made_due(router: &MultiServiceRouter, stack: &str) {
    let _answered = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
}

/// Poll the stack until it leaves every `*_IN_PROGRESS` status, and answer the status it settled
/// in. A read is what makes the transitions its clock owes due, so this is also what drives the
/// stack forward.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut last = String::new();
    for _ in 0..LOOKS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        if status == 200 {
            last = element(&body, "StackStatus");
            if !last.ends_with("_IN_PROGRESS") {
                return last;
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {stack:?} never left an in-progress status; last read {last:?}");
}

/// Every notification that has arrived on one queue, keeping what arrived on earlier looks:
/// the subscriber's own record of the stream, each body as the fields it states
/// ([`notification_fields`]).
#[derive(Debug, Default)]
struct Subscriber {
    arrived: Vec<Vec<(String, String)>>,
}

impl Subscriber {
    /// The `EventId` of every notification that has arrived.
    fn event_ids(&self) -> Vec<String> {
        self.arrived
            .iter()
            .filter_map(|fields| field_of(fields, "EventId"))
            .collect()
    }

    /// The notification of the event `id`, if one arrived.
    fn of(&self, id: &str) -> Option<&Vec<(String, String)>> {
        self.arrived
            .iter()
            .find(|fields| field_of(fields, "EventId").as_deref() == Some(id))
    }

    /// Take everything waiting on the queue at `url` now onto the record.
    async fn look(&mut self, router: &MultiServiceRouter, case: &str, url: &str) {
        for body in drain_bodies(router, case, url).await {
            self.arrived.push(notification_fields(&body));
        }
    }
}

/// Look again until every event in `wanted` has been delivered to the queue at `url`, reading
/// the stack each time so the transitions it owes — and the notifications of them — come due,
/// and assert each of them arrived.
async fn assert_delivered(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    url: &str,
    subscriber: &mut Subscriber,
    wanted: &[String],
    window: &str,
) {
    for _ in 0..LOOKS {
        made_due(router, stack).await;
        subscriber.look(router, case, url).await;
        let delivered = subscriber.event_ids();
        if wanted.iter().all(|id| delivered.contains(id)) {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    for id in wanted {
        assert!(
            subscriber.of(id).is_some(),
            "case {case:?}: event {id} of {window} must be delivered to the topic in force for \
             it; that queue held {:?}",
            subscriber.event_ids()
        );
    }
}

/// Look again until a notification about the STACK itself reporting `status` has arrived.
async fn delivered_stack_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    url: &str,
    subscriber: &mut Subscriber,
    status: &str,
) -> bool {
    for _ in 0..LOOKS {
        made_due(router, stack).await;
        subscriber.look(router, case, url).await;
        let found = subscriber.arrived.iter().any(|fields| {
            field_of(fields, "ResourceType").as_deref() == Some(STACK_RESOURCE_TYPE)
                && field_of(fields, "ResourceStatus").as_deref() == Some(status)
        });
        if found {
            return true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    false
}

/// What one row's `UpdateStack` states for `NotificationARNs`.
#[derive(Debug, Clone, Copy)]
enum Stated {
    /// Nothing at all: the request carries no member of the list.
    Nothing,
    /// The explicitly empty list, which is what an SDK sends for `NotificationARNs=[]`.
    Empty,
    /// The OTHER topic of the row, so the stream moves to it.
    TheOtherTopic,
}

/// The `NotificationARNs` members one `UpdateStack` carries for `stated`.
fn stated_members(stated: Stated, other_topic: &str) -> Vec<(String, String)> {
    match stated {
        Stated::Nothing => Vec::new(),
        // The empty-list marker an SDK sends for a list with no entries.
        Stated::Empty => vec![("NotificationARNs".to_string(), String::new())],
        Stated::TheOtherTopic => vec![(
            "NotificationARNs.member.1".to_string(),
            other_topic.to_string(),
        )],
    }
}

/// The topics an `UpdateStack` stating `stated` leaves in force, for a stack created with
/// `first` and a row whose other topic is `other`.
fn topics_in_force(stated: Stated, first: &str, other: &str) -> Vec<String> {
    match stated {
        Stated::Nothing => vec![first.to_string()],
        Stated::Empty => Vec::new(),
        Stated::TheOtherTopic => vec![other.to_string()],
    }
}

/// An `UpdateStack`'s `NotificationARNs` decide where the events of that update go, the list it
/// leaves in force governs every later operation, and the `DeleteStack` publishes its own
/// transitions to whatever is in force then — while the events of a window owed to no topic, or
/// to another one, are never delivered to a topic named again afterwards.
#[tokio::test]
async fn an_updates_notification_arns_govern_its_own_events_and_the_delete_after_it() {
    // (case, the stack, what the update states for the list)
    let rows: &[(&str, &str, Stated)] = &[
        (
            "an update that states nothing about the list",
            "notified-update-kept",
            Stated::Nothing,
        ),
        (
            "an update stating the empty list",
            "notified-update-cleared",
            Stated::Empty,
        ),
        (
            "an update stating another topic",
            "notified-update-repointed",
            Stated::TheOtherTopic,
        ),
    ];
    for (case, stack, stated) in rows {
        let case = *case;
        let router = router();
        let (first_topic, first_url) = given_topic_with_subscriber(
            &router,
            case,
            REGION,
            &format!("{stack}-first"),
            Grant::ScopedToTopic,
        )
        .await;
        let (other_topic, other_url) = given_topic_with_subscriber(
            &router,
            case,
            REGION,
            &format!("{stack}-other"),
            Grant::ScopedToTopic,
        )
        .await;
        let mut on_first = Subscriber::default();
        let mut on_other = Subscriber::default();

        // The positive control: a stack created naming the first topic publishes every event of
        // its own creation to it.
        let created = cfn_ok(
            &router,
            case,
            "CreateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &template("r1")),
                ("NotificationARNs.member.1", &first_topic),
            ],
        )
        .await;
        let stack_id = element(&created, "StackId");
        assert!(
            !stack_id.is_empty(),
            "case {case:?}: CreateStack must answer the stack's id, got {created}"
        );
        let settled_status = settled(&router, case, &stack_id).await;
        assert_eq!(
            settled_status, "CREATE_COMPLETE",
            "case {case:?}: the stack must complete, got {settled_status:?}"
        );
        let on_create = event_ids(&router, case, &stack_id).await;
        assert!(
            !on_create.is_empty(),
            "case {case:?}: the stack must report events of its own creation"
        );
        assert_delivered(
            &router,
            case,
            &stack_id,
            &first_url,
            &mut on_first,
            &on_create,
            "the create",
        )
        .await;
        for id in &on_create {
            let fields = on_first
                .of(id)
                .unwrap_or_else(|| panic!("case {case:?}: event {id} was asserted delivered"));
            assert_eq!(
                field_of(fields, "StackId").as_ref(),
                Some(&stack_id),
                "case {case:?}: the notification of {id} must name the stack, got {fields:?}"
            );
        }

        // The update under test: whatever it states for the list is what the list becomes, and
        // the events of the update itself are owed to that list.
        let mut members: Vec<(String, String)> = vec![
            ("StackName".to_string(), stack.to_string()),
            ("TemplateBody".to_string(), template("r2")),
        ];
        members.extend(stated_members(*stated, &other_topic));
        let update_members: Vec<(&str, &str)> = members
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        cfn_ok(&router, case, "UpdateStack", &update_members).await;
        let settled_status = settled(&router, case, &stack_id).await;
        assert_eq!(
            settled_status, "UPDATE_COMPLETE",
            "case {case:?}: the update must complete, got {settled_status:?}"
        );
        let in_force = topics_in_force(*stated, &first_topic, &other_topic);
        assert_eq!(
            reported_topics(&router, case, &stack_id).await,
            in_force,
            "case {case:?}: the update must leave the topics it states in force"
        );
        let after_update = event_ids(&router, case, &stack_id).await;
        let of_the_update: Vec<String> = after_update
            .iter()
            .filter(|id| !on_create.contains(id))
            .cloned()
            .collect();
        assert!(
            !of_the_update.is_empty(),
            "case {case:?}: the update must record events of its own"
        );
        if in_force.contains(&first_topic) {
            assert_delivered(
                &router,
                case,
                &stack_id,
                &first_url,
                &mut on_first,
                &of_the_update,
                "the update",
            )
            .await;
        }
        if in_force.contains(&other_topic) {
            assert_delivered(
                &router,
                case,
                &stack_id,
                &other_url,
                &mut on_other,
                &of_the_update,
                "the update",
            )
            .await;
        }

        // ...and a later update naming the first topic again resumes its stream, whatever the
        // update before it left in force.
        cfn_ok(
            &router,
            case,
            "UpdateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &template("r3")),
                ("NotificationARNs.member.1", &first_topic),
            ],
        )
        .await;
        let settled_status = settled(&router, case, &stack_id).await;
        assert_eq!(
            settled_status, "UPDATE_COMPLETE",
            "case {case:?}: the restoring update must complete, got {settled_status:?}"
        );
        assert_eq!(
            reported_topics(&router, case, &stack_id).await,
            vec![first_topic.clone()],
            "case {case:?}: naming the topic again must put it back in force"
        );
        let after_restore = event_ids(&router, case, &stack_id).await;
        let of_the_restore: Vec<String> = after_restore
            .iter()
            .filter(|id| !after_update.contains(id))
            .cloned()
            .collect();
        assert!(
            !of_the_restore.is_empty(),
            "case {case:?}: the restoring update must record events of its own"
        );
        assert_delivered(
            &router,
            case,
            &stack_id,
            &first_url,
            &mut on_first,
            &of_the_restore,
            "the restoring update",
        )
        .await;

        // The window the first topic was not in force for is not delivered to it afterwards:
        // every notification the restored stream owed has arrived by now, so one owed earlier
        // would have arrived with it.
        if !in_force.contains(&first_topic) {
            for id in &of_the_update {
                assert!(
                    on_first.of(id).is_none(),
                    "case {case:?}: event {id} was recorded with that topic out of force and \
                     must not be delivered to it; the queue held {:?}",
                    on_first.event_ids()
                );
            }
        }

        // ...and the delete publishes its own transitions to the list in force when it runs.
        cfn_ok(&router, case, "DeleteStack", &[("StackName", stack)]).await;
        let settled_status = settled(&router, case, &stack_id).await;
        assert_eq!(
            settled_status, "DELETE_COMPLETE",
            "case {case:?}: the delete must complete, got {settled_status:?}"
        );
        for status in ["DELETE_IN_PROGRESS", "DELETE_COMPLETE"] {
            assert!(
                delivered_stack_status(
                    &router,
                    case,
                    &stack_id,
                    &first_url,
                    &mut on_first,
                    status
                )
                .await,
                "case {case:?}: the delete's own {status} must be published to the topic in \
                 force; the queue held {:?}",
                on_first
                    .arrived
                    .iter()
                    .filter_map(|fields| field_of(fields, "ResourceStatus"))
                    .collect::<Vec<String>>()
            );
        }

        // The other topic of the row only ever received the window that named it: nothing of
        // the create, the restore or the delete, all of which the first topic received. Looked
        // at over a QUIET window, so a late delivery to it fails this rather than being missed.
        for _ in 0..QUIET_LOOKS {
            made_due(&router, &stack_id).await;
            on_other.look(&router, case, &other_url).await;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let owed_to_other: Vec<String> = if in_force.contains(&other_topic) {
            of_the_update.clone()
        } else {
            Vec::new()
        };
        let unexpected: Vec<String> = on_other
            .event_ids()
            .into_iter()
            .filter(|id| !owed_to_other.contains(id))
            .collect();
        assert!(
            unexpected.is_empty(),
            "case {case:?}: the other topic must receive only the events recorded while it was \
             in force, got {unexpected:?} besides them"
        );
    }
}
