// SPDX-License-Identifier: CC0-1.0

//! What a resolver step whose data source is of the `AMAZON_EVENTBRIDGE` type DOES: it
//! PUBLISHES the events its request half returned onto the bus the data source was configured
//! with ([`EventBridgeDataSourceConfig$eventBusArn`], "Amazon EventBridge settings"), under the
//! permissions of the role the data source names (`DataSource$serviceRoleArn`: "The system
//! assumes this role when accessing the data source").
//!
//! The request half of such a step returns the data source's own request object: an
//! `operation` naming the call to make — `PutEvents` is the one an EventBridge data source
//! serves — and `events`, the list of events to publish, each carrying `source`, `detailType`
//! and a `detail` OBJECT, with `resources` and `time` where the event states them. The result
//! the response half is then handed is the EventBridge answer, `Entries` (one entry per event,
//! in the order they were published, carrying the `EventId` of the event or the `ErrorCode`
//! and `ErrorMessage` of the entry that failed) and `FailedEntryCount`.
//!
//! Two things make this a real publication rather than a reported one:
//!
//! * the events reach EventBridge ITSELF — the in-process hop into this same emulator
//!   (`service_common::eventbridge_client`), so the bus's own rules decide which of them
//!   reach which target, and the `EventId` a step reports is the id that service minted;
//! * the role the data source names has to ALLOW the publication. AppSync acts under that
//!   role rather than under the caller's own credentials, so a role whose policies withhold
//!   `events:PutEvents` on the configured bus publishes nothing, and the step is reported as
//!   one that did not resolve with the sentence AWS refuses an unauthorized call with
//!   (`service_common::iam_authorization`).
//!
//! An event whose `detail` is not a JSON object is not an event this data source can publish,
//! and the whole request is refused before any of it is published: a request is read in full
//! before the hop, so a batch carrying one such event delivers none of its events rather than
//! some.

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::PlaneContext;

/// The `AMAZON_EVENTBRIDGE` data source a step resolves against, as the step's own record
/// carries it: the name the step names it by, the bus its events go to and the role the
/// publication is authorized against.
#[derive(Debug, Clone, Copy)]
pub struct Configured<'a> {
    /// The data source's name, which an error of the step names it by.
    pub name: &'a str,
    /// `eventBridgeConfig.eventBusArn` — "The ARN of the event bus" the events go to.
    pub event_bus_arn: Option<&'a str>,
    /// `serviceRoleArn`, the role AppSync publishes under, or `None` for a data source that
    /// names none.
    pub service_role_arn: Option<&'a str>,
}

/// Publish the events `requested` carries onto `source`'s bus and answer the result the step's
/// response half is handed, or what the step did not resolve with — a [`Refused`], whose type
/// is what the step's own `ctx.error.type` carries.
pub async fn published(
    context: &PlaneContext,
    source: &Configured<'_>,
    requested: &Json,
) -> Result<Json, Refused> {
    let Some(bus) = source
        .event_bus_arn
        .map(str::trim)
        .filter(|arn| !arn.is_empty())
    else {
        return Err(Refused::request(format!(
            "{NO_EVENT_BUS} {}.",
            source.name
        )));
    };
    let operation = requested
        .get(OPERATION_MEMBER)
        .and_then(Json::as_str)
        .unwrap_or_default();
    if operation != PUT_EVENTS_OPERATION {
        return Err(Refused::request(format!(
            "{ONE_OPERATION} {PUT_EVENTS_OPERATION}, and this request names {operation:?}."
        )));
    }
    let entries = read_entries(requested).map_err(Refused::request)?;
    // The publication is made UNDER the data source's role, so whether that role may make it
    // is decided before anything is published: a role that withholds it publishes nothing.
    if let Some(role_arn) = source.service_role_arn {
        if let Some(refusal) = withheld(context, role_arn, bus).await {
            return Err(Refused {
                message: refusal,
                error_type: answered_error_type(ACCESS_DENIED_EXCEPTION),
            });
        }
    }
    let answer = ::service_common::eventbridge_client::eventbridge_client(
        &context.callback,
        &context.region,
    )
    .put_events()
    .set_entries(Some(
        entries
            .iter()
            .map(|entry| entry.published(bus))
            .collect::<Vec<_>>(),
    ))
    .send()
    .await;
    let answer = match answer {
        Ok(answer) => answer,
        Err(error) => {
            // What the owning service ANSWERED about the publication, which is what a client
            // of this field has to act on: the error class and the sentence EventBridge
            // published, as every caller of another service in this tree reports one.
            use ::aws_sdk_eventbridge::error::ProvideErrorMetadata;
            let code = error.code().unwrap_or(UNKNOWN_ERROR_CODE);
            return Err(Refused {
                message: error.message().unwrap_or(NOT_PUBLISHED).to_string(),
                error_type: answered_error_type(code),
            });
        }
    };
    let published: Vec<Json> = answer
        .entries()
        .iter()
        .map(|entry| {
            let mut reported = JsonMap::new();
            if let Some(event_id) = entry.event_id() {
                reported.insert(EVENT_ID_MEMBER.to_string(), json!(event_id));
            }
            if let Some(code) = entry.error_code() {
                reported.insert(ERROR_CODE_MEMBER.to_string(), json!(code));
            }
            if let Some(message) = entry.error_message() {
                reported.insert(ERROR_MESSAGE_MEMBER.to_string(), json!(message));
            }
            Json::Object(reported)
        })
        .collect();
    Ok(json!({
        ENTRIES_MEMBER: published,
        FAILED_ENTRY_COUNT_MEMBER: answer.failed_entry_count(),
    }))
}

/// Why a step resolving against an `AMAZON_EVENTBRIDGE` data source did not resolve: the
/// sentence it states, and the `errorType` it is of.
///
/// AppSync reports both to the step's own `ctx.error` (`message` and `type`), which is what a
/// definition reads to decide the field — and what it re-raises with where it calls
/// `util.error(ctx.error.message, ctx.error.type)`.
#[derive(Debug, Clone)]
pub struct Refused {
    pub message: String,
    pub error_type: String,
}

impl Refused {
    /// A request this data source cannot act on: the step's own definition produced it, so it
    /// is an error of that CODE rather than anything the owning service answered.
    fn request(message: String) -> Self {
        Self {
            message,
            error_type: REQUEST_ERROR_TYPE.to_string(),
        }
    }
}

/// The `errorType` of what the OWNING service answered about the publication: AppSync reports
/// a downstream refusal as the service it came from and that service's own error class.
fn answered_error_type(code: &str) -> String {
    format!("{ANSWERED_ERROR_TYPE_PREFIX}{code}")
}

/// One event of the request, read as the entry it is published as.
#[derive(Debug, Clone)]
struct Entry {
    source: String,
    detail_type: String,
    /// The event's `detail`, as the JSON TEXT of the object it is: `Detail` is "a valid JSON
    /// object" on the wire, and only an object is one.
    detail: String,
    resources: Vec<String>,
    time: Option<::aws_smithy_types::DateTime>,
}

impl Entry {
    /// This entry as the one `PutEvents` publishes onto `bus`.
    fn published(&self, bus: &str) -> ::aws_sdk_eventbridge::types::PutEventsRequestEntry {
        let mut entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
            .source(self.source.clone())
            .detail_type(self.detail_type.clone())
            .detail(self.detail.clone())
            .event_bus_name(bus.to_string())
            .set_resources(Some(self.resources.clone()));
        if let Some(time) = self.time {
            entry = entry.time(time);
        }
        entry.build()
    }
}

/// The events `requested` publishes, in the order it states them, or the sentence the whole
/// request is refused with.
///
/// Every event is read BEFORE any of them is published, so a request carrying one event this
/// data source cannot publish publishes none of them.
fn read_entries(requested: &Json) -> Result<Vec<Entry>, String> {
    let Some(events) = requested.get(EVENTS_MEMBER) else {
        return Err(format!("{NO_EVENTS} {EVENTS_MEMBER}."));
    };
    let Some(events) = events.as_array() else {
        return Err(format!("{EVENTS_ARE_A_LIST} {EVENTS_MEMBER}."));
    };
    if events.is_empty() || events.len() > MAX_ENTRIES {
        return Err(format!(
            "{EVENTS_PER_REQUEST} {MAX_ENTRIES}, and this request carries {}.",
            events.len()
        ));
    }
    events
        .iter()
        .enumerate()
        .map(|(index, event)| read_entry(index, event))
        .collect()
}

/// One event of the request as the entry it is published as, or the sentence the request is
/// refused with — which LOCATES the offending value in the request the step's own definition
/// produced ([`member_path`]), because a request carries several events and the value that
/// cannot be published is the one a client has to correct.
fn read_entry(index: usize, event: &Json) -> Result<Entry, String> {
    let Some(members) = event.as_object() else {
        return Err(format!(
            "{AN_EVENT_IS_AN_OBJECT} {}.",
            event_path(index)
        ));
    };
    let text = |member: &str| -> Result<String, String> {
        members
            .get(member)
            .and_then(Json::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!(
                    "{AN_EVENT_STATES} {SOURCE_MEMBER}, {DETAIL_TYPE_MEMBER} and \
                     {DETAIL_MEMBER}, and no value is carried at {}.",
                    member_path(index, member)
                )
            })
    };
    let source = text(SOURCE_MEMBER)?;
    let detail_type = text(DETAIL_TYPE_MEMBER)?;
    // `Detail` is "a valid JSON object. There is no other schema imposed", and a rule's
    // pattern reaches into it, so a detail that is not an object is no event's detail: the
    // request carrying it is refused rather than published in part.
    let Some(detail) = members.get(DETAIL_MEMBER).filter(|detail| detail.is_object()) else {
        return Err(format!(
            "{A_DETAIL_IS_AN_OBJECT} {}.",
            member_path(index, DETAIL_MEMBER)
        ));
    };
    let resources = match members.get(RESOURCES_MEMBER) {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::Array(named)) => named
            .iter()
            .map(|resource| {
                resource.as_str().map(str::to_string).ok_or_else(|| {
                    format!(
                        "{A_RESOURCE_IS_AN_ARN} {}.",
                        member_path(index, RESOURCES_MEMBER)
                    )
                })
            })
            .collect::<Result<Vec<String>, String>>()?,
        Some(_) => {
            return Err(format!(
                "{RESOURCES_ARE_A_LIST} {}.",
                member_path(index, RESOURCES_MEMBER)
            ))
        }
    };
    let time = match members.get(TIME_MEMBER) {
        None | Some(Json::Null) => None,
        Some(Json::String(at)) => Some(
            ::aws_smithy_types::DateTime::from_str(
                at,
                ::aws_smithy_types::date_time::Format::DateTime,
            )
            .map_err(|_| format!("{A_TIME_IS_RFC3339} {}.", member_path(index, TIME_MEMBER)))?,
        ),
        Some(_) => {
            return Err(format!(
                "{A_TIME_IS_RFC3339} {}.",
                member_path(index, TIME_MEMBER)
            ))
        }
    };
    Ok(Entry {
        source,
        detail_type,
        detail: detail.to_string(),
        resources,
        time,
    })
}

/// Where one event of the request sits, as a refusal locates it: the member path of that
/// event within the request the step's definition produced.
fn event_path(index: usize) -> String {
    format!("[{EVENTS_MEMBER}][{index}]")
}

/// Where one MEMBER of one event sits, the same way.
fn member_path(index: usize, member: &str) -> String {
    format!("{}[{member}]", event_path(index))
}

/// The sentence AWS refuses the publication with when the permissions in force on the data
/// source's role do not allow it on the configured bus, or `None` when nothing is known
/// against it.
///
/// Which answer counts is `::service_common::iam_role`'s own rule: a role that STATES
/// identity-based policies is the authority on what it may do, so it has to be ALLOWED the
/// publication; a role IAM holds no policy for says nothing about the action — a data source's
/// service role is routinely provisioned out of band — so only a statement that DENIES it holds
/// the publication back. Everything that is not an answer (no IAM to ask, a value naming no
/// role of this account, a failed hop) withholds nothing.
async fn withheld(context: &PlaneContext, role_arn: &str, bus_arn: &str) -> Option<String> {
    let states_permissions = ::service_common::iam_role::role_states_permissions_through(
        &context.callback,
        &context.region,
        role_arn,
    )
    .await;
    let withheld = if states_permissions {
        ::service_common::iam_role::role_withholds_action_through(
            &context.callback,
            &context.region,
            role_arn,
            PUT_EVENTS_ACTION,
            bus_arn,
        )
        .await
    } else {
        ::service_common::iam_role::role_denies_action_through(
            &context.callback,
            &context.region,
            role_arn,
            PUT_EVENTS_ACTION,
            bus_arn,
        )
        .await
    };
    withheld.then(|| {
        let reason = if states_permissions {
            ::service_common::iam_authorization::no_identity_policy_allows(PUT_EVENTS_ACTION)
        } else {
            ::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
        };
        ::service_common::iam_authorization::not_authorized_by_identity_policy(
            &assumed_role_principal(role_arn),
            PUT_EVENTS_ACTION,
            bus_arn,
            &reason,
        )
    })
}

/// The principal AppSync makes the publication as: a SESSION of the role the data source names
/// (`arn:<partition>:sts::<account>:assumed-role/<role name>/<session name>`), which is the
/// identity an authorization refusal is about — a role is never acted as directly.
///
/// The partition and the account are the role's own, read off its ARN through the tree's one
/// ARN parse, so a role of another partition or account is named as that role's session rather
/// than as this emulator's.
fn assumed_role_principal(role_arn: &str) -> String {
    let parsed = ::service_common::arn::parse(role_arn);
    let partition = parsed.map_or(DEFAULT_PARTITION, |arn| arn.partition);
    let account = parsed.map_or("", |arn| arn.account);
    // The role's own name is the last segment of its resource, so a role held under a path
    // (`role/service-role/<name>`) is named the way IAM holds it, and a value that names no
    // role at all is named as it stands.
    let role_name = parsed
        .map(|arn| arn.resource)
        .and_then(|resource| resource.strip_prefix(ROLE_RESOURCE_PREFIX))
        .and_then(|name| name.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or(role_arn);
    format!("arn:{partition}:sts::{account}:assumed-role/{role_name}/{ROLE_SESSION_NAME}")
}

/// The resource type an IAM role ARN carries, which its name follows.
const ROLE_RESOURCE_PREFIX: &str = "role/";

/// The name of the role SESSION AppSync publishes a data source's events under, which a
/// refusal naming the session principal carries.
const ROLE_SESSION_NAME: &str = "APPSYNC_ASSUME_ROLE";

/// The partition a role ARN this emulator cannot parse is named under.
const DEFAULT_PARTITION: &str = "aws";

/// The IAM action a publication onto an event bus is authorized by ("Actions defined by Amazon
/// EventBridge", `PutEvents`).
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The error class a call no identity-based policy allows is refused with, which every AWS
/// service words an authorization refusal as; the SENTENCE is the tree's one wording of that
/// refusal (`::service_common::iam_authorization`).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The `errorType` a step's failure is reported under.
///
/// AppSync publishes no document stating these two strings, so they are this emulator's, chosen
/// for the ONE thing a client branches on: WHOSE failure it is. A request the data source
/// cannot act on is a failure of the resolver's own definition — the `code` or the template
/// that produced it — so it is reported as one of that code; a refusal the OWNING service
/// answered is reported as that service and that service's own error class, the
/// `<service>:<error class>` form a downstream refusal is read by (`AccessDeniedException` for
/// a publication no identity-based policy allows, which is how every AWS service words an
/// authorization refusal).
const REQUEST_ERROR_TYPE: &str = "Code";
const ANSWERED_ERROR_TYPE_PREFIX: &str = "EventBridge:";

/// The one call an `AMAZON_EVENTBRIDGE` data source serves, as the request's own `operation`
/// member names it.
const PUT_EVENTS_OPERATION: &str = "PutEvents";

/// The members of the request this data source is handed, and of the result it answers with.
const OPERATION_MEMBER: &str = "operation";
const EVENTS_MEMBER: &str = "events";
const SOURCE_MEMBER: &str = "source";
const DETAIL_TYPE_MEMBER: &str = "detailType";
const DETAIL_MEMBER: &str = "detail";
const RESOURCES_MEMBER: &str = "resources";
const TIME_MEMBER: &str = "time";
const ENTRIES_MEMBER: &str = "Entries";
const EVENT_ID_MEMBER: &str = "EventId";
const ERROR_CODE_MEMBER: &str = "ErrorCode";
const ERROR_MESSAGE_MEMBER: &str = "ErrorMessage";
const FAILED_ENTRY_COUNT_MEMBER: &str = "FailedEntryCount";

/// How many events one publication carries. Source: `PutEventsRequestEntryList`'s own length
/// (`smithy_query(query="shape",shape="PutEventsRequestEntryList")`, 1..10), which is the list
/// the events of one request are published as.
const MAX_ENTRIES: usize = 10;

/// The sentences a step that did not publish states.
///
/// AppSync publishes no wording for a request its own data source cannot act on, so these are
/// this emulator's own sentences. What each of them owes the client is the same: it names the
/// rule the request broke, and every one that is about a VALUE of the request ends in the
/// member path of that value ([`member_path`]), so a client reads which value to correct out of
/// the sentence. Nothing of one request's own values is written into them.
const NO_EVENT_BUS: &str = "No event bus is configured for the AMAZON_EVENTBRIDGE data source";
const ONE_OPERATION: &str = "An AMAZON_EVENTBRIDGE data source serves the operation";
const NO_EVENTS: &str = "An AMAZON_EVENTBRIDGE request states the events to publish in";
const EVENTS_ARE_A_LIST: &str = "An AMAZON_EVENTBRIDGE request carries a list of events in";
const EVENTS_PER_REQUEST: &str = "One publication carries between 1 and";
const AN_EVENT_IS_AN_OBJECT: &str =
    "An event of an AMAZON_EVENTBRIDGE request is a JSON object, which is not what is carried \
     at";
const AN_EVENT_STATES: &str = "An event of an AMAZON_EVENTBRIDGE request states its";
const A_DETAIL_IS_AN_OBJECT: &str =
    "The detail of an event is a valid JSON object, which is not what is carried at";
const A_RESOURCE_IS_AN_ARN: &str =
    "A resource of an event is named by its ARN, which is not what is carried at";
const RESOURCES_ARE_A_LIST: &str =
    "The resources of an event are a list of ARNs, which is not what is carried at";
const A_TIME_IS_RFC3339: &str =
    "The time of an event is a timestamp per RFC 3339, which is not what is carried at";
const NOT_PUBLISHED: &str = "The events of this field were not published.";
const UNKNOWN_ERROR_CODE: &str = "Unknown";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every event of a request is read before any of them is published, and an event
    /// whose `detail` is not a JSON object is no event at all — so a request carrying one is
    /// refused whole. Source: `PutEventsRequestEntry$Detail`, "A valid JSON object".
    #[test]
    fn an_event_whose_detail_is_not_an_object_refuses_the_whole_request() {
        let object = json!({"orderId": "A-1"});
        for (case, detail) in [
            ("a string", json!("A-1")),
            ("a number", json!(7)),
            ("a boolean", json!(true)),
            ("a list", json!([{"orderId": "A-1"}])),
            ("null", Json::Null),
        ] {
            let requested = json!({
                "operation": PUT_EVENTS_OPERATION,
                "events": [
                    {"source": "com.demo", "detailType": "Ok", "detail": object},
                    {"source": "com.demo", "detailType": "Bad", "detail": detail},
                ],
            });
            let read = read_entries(&requested);
            assert!(
                read.is_err(),
                "case {case:?}: expected the whole request to be refused, got {read:?}"
            );
        }
        // And a request whose every detail IS an object is read in full, in order.
        let requested = json!({
            "operation": PUT_EVENTS_OPERATION,
            "events": [
                {"source": "com.demo", "detailType": "One", "detail": {"n": 1}},
                {"source": "com.demo", "detailType": "Two", "detail": {"n": 2}},
            ],
        });
        let read = read_entries(&requested).expect("a well-formed request is read");
        let types: Vec<String> = read.iter().map(|entry| entry.detail_type.clone()).collect();
        assert_eq!(
            types,
            vec!["One".to_string(), "Two".to_string()],
            "case {:?}: expected the events in the order the request states them, got {types:?}",
            "two well-formed events"
        );
    }

    /// RULE: a publication carries between 1 and 10 events, and each states `source`,
    /// `detailType` and `detail`. Source: `PutEventsRequestEntryList`'s length and
    /// `PutEventsRequestEntry`'s own required members.
    #[test]
    fn a_request_outside_what_one_publication_carries_is_refused() {
        let event = json!({"source": "com.demo", "detailType": "Ok", "detail": {"n": 1}});
        for (case, events, served) in [
            ("no events at all", json!([]), false),
            ("one event", json!([event.clone()]), true),
            (
                "the most events one publication carries",
                Json::Array(vec![event.clone(); MAX_ENTRIES]),
                true,
            ),
            (
                "one event past that",
                Json::Array(vec![event.clone(); MAX_ENTRIES + 1]),
                false,
            ),
            (
                "an event that states no source",
                json!([{"detailType": "Ok", "detail": {"n": 1}}]),
                false,
            ),
            (
                "an event that states no detailType",
                json!([{"source": "com.demo", "detail": {"n": 1}}]),
                false,
            ),
            (
                "an event that states no detail",
                json!([{"source": "com.demo", "detailType": "Ok"}]),
                false,
            ),
        ] {
            let read = read_entries(&json!({"operation": PUT_EVENTS_OPERATION, "events": events}));
            assert_eq!(
                read.is_ok(),
                served,
                "case {case:?}: expected served={served}, got {read:?}"
            );
        }
    }

    /// RULE: a refusal of a request this data source cannot act on LOCATES the offending
    /// value in the request the step's definition produced, as the member path of that value.
    #[test]
    fn a_refusal_locates_the_offending_member_of_the_request() {
        for (case, events, located) in [
            (
                "a detail that is not an object",
                json!([{"source": "com.demo", "detailType": "Ok", "detail": "text"}]),
                "[events][0][detail]",
            ),
            (
                "the second event's detail",
                json!([
                    {"source": "com.demo", "detailType": "Ok", "detail": {"n": 1}},
                    {"source": "com.demo", "detailType": "Ok", "detail": 7},
                ]),
                "[events][1][detail]",
            ),
            (
                "an event that states no source",
                json!([{"detailType": "Ok", "detail": {"n": 1}}]),
                "[events][0][source]",
            ),
            (
                "an event that states no detailType",
                json!([{"source": "com.demo", "detail": {"n": 1}}]),
                "[events][0][detailType]",
            ),
            (
                "a time that is no timestamp",
                json!([{
                    "source": "com.demo", "detailType": "Ok", "detail": {"n": 1},
                    "time": "half past two",
                }]),
                "[events][0][time]",
            ),
            (
                "resources that are not a list",
                json!([{
                    "source": "com.demo", "detailType": "Ok", "detail": {"n": 1},
                    "resources": "arn:aws:s3:::a-bucket",
                }]),
                "[events][0][resources]",
            ),
            (
                "an event that is not an object",
                json!(["not-an-event"]),
                "[events][0]",
            ),
        ] {
            let refused = read_entries(&json!({"operation": PUT_EVENTS_OPERATION, "events": events}))
                .expect_err("a request this data source cannot act on is refused");
            assert!(
                refused.contains(located),
                "case {case:?}: expected the refusal to locate {located}, got {refused:?}"
            );
        }
    }

    /// RULE: the refusal of a publication a role withholds names the role SESSION AppSync
    /// publishes under, not the role itself — a role is acted as through a session.
    #[test]
    fn the_refusal_names_the_sessions_of_the_data_sources_role() {
        for (case, role_arn, expected) in [
            (
                "a role of this account",
                "arn:aws:iam::123456789012:role/appsync-eventbridge",
                "arn:aws:sts::123456789012:assumed-role/appsync-eventbridge/APPSYNC_ASSUME_ROLE",
            ),
            (
                "a role under a path",
                "arn:aws:iam::123456789012:role/service-role/appsync-eb",
                "arn:aws:sts::123456789012:assumed-role/appsync-eb/APPSYNC_ASSUME_ROLE",
            ),
            (
                "a role of another partition",
                "arn:aws-us-gov:iam::210987654321:role/published",
                "arn:aws-us-gov:sts::210987654321:assumed-role/published/APPSYNC_ASSUME_ROLE",
            ),
        ] {
            let named = assumed_role_principal(role_arn);
            assert_eq!(
                named, expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
    }
}
