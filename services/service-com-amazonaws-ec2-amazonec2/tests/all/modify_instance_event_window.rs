// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyInstanceEventWindow`, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//! The window's creation, its filters and its paging are covered by
//! [`super::instance_event_windows`], and its targets by
//! [`super::instance_event_window_associations`].
//!
//! The rules under test are:
//!
//! 1. A MODIFY WRITES THE MEMBERS THE REQUEST NAMES AND LEAVES EVERY OTHER MEMBER OF THE
//!    WINDOW AS IT WAS: the window the call reports is the one
//!    `DescribeInstanceEventWindows` reports afterwards, so a call that renames a window
//!    keeps its schedule, a call that reschedules it keeps its name, and a call that names
//!    neither leaves both — the window's id, its own tags and its targets throughout;
//! 2. A WINDOW SAYS WHEN IT OPENS WITH EXACTLY ONE of `CronExpression` and `TimeRanges`, so
//!    naming one form REPLACES the other and naming both is refused, and whichever form a
//!    request names is held to the constraints the model states for it — the same ones the
//!    create of a window holds it to;
//! 3. A REFUSED MODIFY WRITES NOTHING: an id the account holds no window for, one that is
//!    no window id at all, a request carrying none, a schedule outside those constraints and
//!    a `DryRun` all leave the window exactly as it was.
//!
//! Each table row is a distinct input along what the rules generalise over — which member
//! the request names, how many it names, which form the schedule takes before and after,
//! how many time ranges, which days (including a range that runs across the end of the
//! week), each published bound and one past it — so no row is the specification.

use service_com_amazonaws_ec2_amazonec2::state::{
    INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND, INVALID_INSTANCE_EVENT_WINDOW_ID_MALFORMED,
    MAX_EVENT_WINDOW_CRON_BYTES, MAX_EVENT_WINDOW_TIME_RANGES, MIN_EVENT_WINDOW_RANGE_HOURS,
    MIN_EVENT_WINDOW_TOTAL_HOURS,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message};
use super::instance_event_window_associations::Target;
// The wire shape of a window's `TimeRange.N.*` group, of a `WeekDay` and of the
// `TagSpecification` that tags a window are read once, in the suite that covers the create
// of one, rather than a second time here.
use super::instance_event_windows::{time_ranges, week_day, window_tags};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for. An event window carries no ARN and no zone, so
/// nothing asserted here is derived from it — it is only the scope the ec2Query envelope is
/// selected by.
const REGION: &str = "us-west-2";

/// A well-formed event window id the account holds no window for.
const UNKNOWN_WINDOW: &str = "iew-0fedcba987654321f";

/// A string that is not an event window id at all.
const MALFORMED_WINDOW: &str = "iew-not-an-id";

/// The tags every window created here carries. A tag is a member of the window no modify
/// speaks about, so it is what rule 1 is asked about beside the schedule and the name.
const WINDOW_TAGS: [(&str, &str); 2] = [("Owner", "platform"), ("Tier", "web")];

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// How a window says when it opens, in one of the two forms a request may name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Schedule {
    /// A cron expression, as `CronExpression` carries it.
    Cron(&'static str),
    /// Time ranges, each `(start day, start hour, end day, end hour)`.
    Ranges(Vec<(usize, i64, usize, i64)>),
}

impl Schedule {
    /// This schedule as request parameters.
    fn params(&self) -> Params {
        match self {
            Self::Cron(expression) => vec![p("CronExpression", *expression)],
            Self::Ranges(ranges) => time_ranges(ranges),
        }
    }

    /// The `cronExpression` a window on this schedule must report — absent for one defined
    /// by time ranges.
    fn reported_cron(&self) -> Option<String> {
        match self {
            Self::Cron(expression) => Some((*expression).to_string()),
            Self::Ranges(_) => None,
        }
    }

    /// The `timeRangeSet` a window on this schedule must report — empty for one defined by
    /// a cron expression.
    fn reported_ranges(&self) -> Vec<(String, String, String, String)> {
        match self {
            Self::Cron(_) => Vec::new(),
            Self::Ranges(ranges) => ranges
                .iter()
                .map(|(start_day, start_hour, end_day, end_hour)| {
                    (
                        week_day(*start_day).to_string(),
                        start_hour.to_string(),
                        week_day(*end_day).to_string(),
                        end_hour.to_string(),
                    )
                })
                .collect(),
        }
    }
}

/// One event window as a response reports it: everything about it a modify may or may not
/// change, so one comparison covers both halves of rule 1.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    id: String,
    name: Option<String>,
    cron_expression: Option<String>,
    ranges: Vec<(String, String, String, String)>,
    tags: Vec<(String, String)>,
}

/// The one event window `body` carries, read through the XML tree: the window's OWN tags are
/// its `tagSet`, which a substring read would confuse with the `tagSet` of the targets
/// inside its `associationTarget`.
fn reported(case: &str, body: &str) -> Reported {
    let parsed = document(case, body);
    let window = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "instanceEventWindowId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no event window in {body}"));
    Reported {
        id: child_text(window, "instanceEventWindowId").unwrap_or_default(),
        name: child_text(window, "name"),
        cron_expression: child_text(window, "cronExpression"),
        ranges: set_items(window, "timeRangeSet")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "startWeekDay").unwrap_or_default(),
                    child_text(item, "startHour").unwrap_or_default(),
                    child_text(item, "endWeekDay").unwrap_or_default(),
                    child_text(item, "endHour").unwrap_or_default(),
                )
            })
            .collect(),
        tags: set_items(window, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "key").unwrap_or_default(),
                    child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect(),
    }
}

/// Create one tagged window on `schedule`, with `name` when it is named at all, and answer
/// the id the service minted for it.
async fn create_window(
    router: &MultiServiceRouter,
    case: &str,
    name: Option<&str>,
    schedule: &Schedule,
) -> String {
    let mut params: Params = name.map(|name| p("Name", name)).into_iter().collect();
    params.extend(schedule.params());
    params.extend(window_tags(&WINDOW_TAGS));
    let (status, body) = api(router, "CreateInstanceEventWindow", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateInstanceEventWindow must succeed, got {status} with body {body}"
    );
    reported(case, &body).id
}

/// The window `window_id` as `DescribeInstanceEventWindows` reports it.
async fn described(router: &MultiServiceRouter, case: &str, window_id: &str) -> Reported {
    let (status, body) = api(
        router,
        "DescribeInstanceEventWindows",
        &[p("InstanceEventWindowId.1", window_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must succeed, got {status} with body {body}"
    );
    reported(case, &body)
}

/// One row of the round-trip table: a window created one way, and the members one modify
/// names for it.
struct ModifyCase {
    case: &'static str,
    /// The name the window was created with, or `None` for one created without a name.
    created_name: Option<&'static str>,
    /// The schedule the window was created on.
    created: Schedule,
    /// The name the modify names, or `None` for a modify that leaves the name alone.
    named: Option<&'static str>,
    /// The schedule the modify names, or `None` for a modify that leaves the schedule
    /// alone.
    schedule: Option<Schedule>,
}

impl ModifyCase {
    /// The name the window must report after the modify: the one it was given, else the one
    /// it already had.
    fn expected_name(&self) -> Option<String> {
        self.named
            .or(self.created_name)
            .map(|name| name.to_string())
    }

    /// The schedule the window must report after the modify: the one it was given, else the
    /// one it already had.
    fn expected_schedule(&self) -> &Schedule {
        self.schedule.as_ref().unwrap_or(&self.created)
    }
}

/// The modifies the round-trip table makes. They differ along which members one call names
/// (the name alone, the schedule alone, both, neither), along which form the schedule takes
/// before and after it (cron to cron, cron to time ranges, time ranges to cron, time ranges
/// to more time ranges), and along how many ranges and which days those ranges name —
/// including one that runs across the end of the week.
fn modify_cases() -> Vec<ModifyCase> {
    vec![
        ModifyCase {
            case: "the name alone, on a window defined by a cron expression",
            created_name: Some("nightly-maintenance"),
            created: Schedule::Cron("* 0-4 * * 1,5"),
            named: Some("renamed-nightly-maintenance"),
            schedule: None,
        },
        ModifyCase {
            case: "the name alone, on a window defined by time ranges",
            created_name: Some("monday-window"),
            created: Schedule::Ranges(vec![(1, 2, 3, 8)]),
            named: Some("midweek-window"),
            schedule: None,
        },
        ModifyCase {
            case: "a name for a window created without one",
            created_name: None,
            created: Schedule::Cron("* 0-23 * * *"),
            named: Some("now-named"),
            schedule: None,
        },
        ModifyCase {
            case: "a cron expression replacing a cron expression",
            created_name: Some("two-windows-a-day"),
            created: Schedule::Cron("* 0-4 * * 1,5"),
            named: None,
            schedule: Some(Schedule::Cron("* 2-4,20-23 * * SUN,SAT")),
        },
        ModifyCase {
            case: "time ranges replacing a cron expression",
            created_name: Some("was-cron"),
            created: Schedule::Cron("* 0-4 * * 1,5"),
            named: None,
            schedule: Some(Schedule::Ranges(vec![(1, 0, 1, 4)])),
        },
        ModifyCase {
            case: "a cron expression replacing time ranges",
            created_name: Some("was-ranges"),
            created: Schedule::Ranges(vec![(1, 2, 3, 8)]),
            named: None,
            schedule: Some(Schedule::Cron("* 20-23,0-2 * * 6")),
        },
        ModifyCase {
            case: "three time ranges replacing one, one across the end of the week",
            created_name: Some("one-range"),
            created: Schedule::Ranges(vec![(1, 2, 3, 8)]),
            named: None,
            schedule: Some(Schedule::Ranges(vec![(1, 0, 1, 2), (2, 0, 2, 2), (6, 20, 0, 4)])),
        },
        ModifyCase {
            // `InstanceEventWindowTimeRangeRequest$StartHour` / `$EndHour` target `Hour`,
            // modelled `@range(min: 0, max: 23)`
            // (`smithy_query(query="shape",shape="InstanceEventWindowTimeRangeRequest")`),
            // so this row reschedules the window onto a range running from one published
            // bound to the other.
            case: "a name and a schedule spanning the published hours of one day together",
            created_name: Some("short-range"),
            created: Schedule::Ranges(vec![(1, 0, 1, 4)]),
            named: Some("all-day-monday"),
            schedule: Some(Schedule::Ranges(vec![(1, 0, 1, 23)])),
        },
        ModifyCase {
            case: "neither a name nor a schedule, on a window defined by a cron expression",
            created_name: Some("unchanged-cron"),
            created: Schedule::Cron("* 0-4 * * 1,5"),
            named: None,
            schedule: None,
        },
        ModifyCase {
            case: "neither a name nor a schedule, on a window defined by time ranges",
            created_name: Some("unchanged-ranges"),
            created: Schedule::Ranges(vec![(1, 2, 3, 8)]),
            named: None,
            schedule: None,
        },
    ]
}

#[tokio::test]
async fn a_modify_writes_the_members_it_names_and_leaves_the_rest() {
    for row in modify_cases() {
        let case = row.case;
        // A fresh store per row, so the describe below reports this row's window alone.
        let router = emulator();
        let window_id = create_window(&router, case, row.created_name, &row.created).await;

        let mut params = vec![p("InstanceEventWindowId", window_id.clone())];
        if let Some(name) = row.named {
            params.push(p("Name", name));
        }
        if let Some(schedule) = row.schedule.as_ref() {
            params.extend(schedule.params());
        }
        let (status, body) = api(&router, "ModifyInstanceEventWindow", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ModifyInstanceEventWindow, \
             got {status} with body {body}"
        );

        let expected = Reported {
            id: window_id.clone(),
            name: row.expected_name(),
            cron_expression: row.expected_schedule().reported_cron(),
            ranges: row.expected_schedule().reported_ranges(),
            tags: WINDOW_TAGS
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        };
        let actual = reported(case, &body);
        assert_eq!(
            actual, expected,
            "case {case:?}: the modify must report the window it left, \
             expected {expected:?}, got {actual:?} in body {body}"
        );

        // And what it left is what every later reader sees.
        let read_back = described(&router, case, &window_id).await;
        assert_eq!(
            read_back, expected,
            "case {case:?}: the describe must report the window the modify left, \
             expected {expected:?}, got {read_back:?}"
        );
    }
}

#[tokio::test]
async fn a_modify_leaves_the_windows_targets_alone() {
    let router = emulator();
    let case = "a window associated with instance tags";
    let window_id = create_window(
        &router,
        case,
        Some("targeted"),
        &Schedule::Cron("* 0-4 * * 1,5"),
    )
    .await;

    // "To modify the targets associated with the event window, use the
    // AssociateInstanceEventWindow and DisassociateInstanceEventWindow API"
    // (`ModifyInstanceEventWindow`), so the targets that call put on the window survive a
    // modify of what the window is named and of when it opens.
    let (status, body) = api(
        &router,
        "AssociateInstanceEventWindow",
        &[
            p("InstanceEventWindowId", window_id.clone()),
            p("AssociationTarget.InstanceTag.1.Key", "Stage"),
            p("AssociationTarget.InstanceTag.1.Value", "prod"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the association must succeed, got {status} with body {body}"
    );
    let associated = Target::of_tags(&[("Stage", "prod")]);

    let (status, body) = api(
        &router,
        "ModifyInstanceEventWindow",
        &[
            p("InstanceEventWindowId", window_id.clone()),
            p("Name", "retargeted-by-nobody"),
            p("TimeRange.1.StartWeekDay", week_day(1)),
            p("TimeRange.1.StartHour", "0"),
            p("TimeRange.1.EndWeekDay", week_day(1)),
            p("TimeRange.1.EndHour", "4"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the modify must succeed, got {status} with body {body}"
    );
    assert_eq!(
        super::instance_event_window_associations::reported_target(case, &body),
        Some(associated.clone()),
        "case {case:?}: the modify must report the targets the window still carries, \
         got body {body}"
    );

    let (status, body) = api(
        &router,
        "DescribeInstanceEventWindows",
        &[p("InstanceEventWindowId.1", window_id.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must succeed, got {status} with body {body}"
    );
    assert_eq!(
        super::instance_event_window_associations::reported_target(case, &body),
        Some(associated),
        "case {case:?}: the describe must report the targets the modify left alone, \
         got body {body}"
    );
}

/// Which `InstanceEventWindowId` a refusal row names.
#[derive(Debug, Clone, Copy)]
enum Names {
    /// The window the row created: the refusal is about the rest of the request.
    TheWindow,
    /// A well-formed id the account holds no window for.
    AnUnknownWindow,
    /// A value that is no event window id at all.
    AMalformedId,
    /// An id that is the empty string: the parameter is on the wire, carrying no value.
    AnEmptyId,
    /// No id at all: `InstanceEventWindowId` is `@required`, so the boundary refuses a
    /// request that leaves it out before the handler runs.
    NoWindow,
}

impl Names {
    /// The `InstanceEventWindowId` parameter this row carries, if any.
    fn params(self, window_id: &str) -> Params {
        let id = match self {
            Self::TheWindow => window_id,
            Self::AnUnknownWindow => UNKNOWN_WINDOW,
            Self::AMalformedId => MALFORMED_WINDOW,
            Self::AnEmptyId => "",
            Self::NoWindow => return Params::new(),
        };
        vec![p("InstanceEventWindowId", id)]
    }
}

/// One row of the refusal table: `(case, which window it names, the rest of the request, the
/// EC2 error code)`.
type RefusalRow = (&'static str, Names, Params, &'static str);

/// Every way a modify is outside what the service serves: the window it names, and the
/// schedule it names for it. The schedule rows are the constraints
/// `CreateInstanceEventWindowRequest$CronExpression` documents — each range at least two
/// hours and the ranges at least four hours between them, five cron fields of which only the
/// hour and the day of the week carry values — each at a published bound or one past it, and
/// each held against the modify exactly as the create holds it.
fn refusals() -> Vec<RefusalRow> {
    let min_range = MIN_EVENT_WINDOW_RANGE_HOURS;
    let min_total = MIN_EVENT_WINDOW_TOTAL_HOURS;
    // The member's own code, answered for every way its expression can break the documented
    // rules — the create of a window answers the same one.
    const CRON: &str = "InvalidInstanceEventWindowCronExpression";
    vec![
        (
            "an id the account holds no window for",
            Names::AnUnknownWindow,
            vec![p("Name", "renamed")],
            INVALID_INSTANCE_EVENT_WINDOW_ID_DOT_NOT_FOUND,
        ),
        (
            "a value that is no event window id",
            Names::AMalformedId,
            vec![p("Name", "renamed")],
            INVALID_INSTANCE_EVENT_WINDOW_ID_MALFORMED,
        ),
        (
            "an event window id carrying no value",
            Names::AnEmptyId,
            vec![p("Name", "renamed")],
            "MissingParameter",
        ),
        (
            // `ModifyInstanceEventWindowRequest$InstanceEventWindowId` is `@required`, so
            // the boundary refuses a request that names no window before the handler runs,
            // with EC2's own code for a parameter the request left out.
            "no event window id at all",
            Names::NoWindow,
            vec![p("Name", "renamed")],
            "MissingParameter",
        ),
        (
            "both a cron expression and a time range",
            Names::TheWindow,
            {
                let mut params = vec![p("CronExpression", "* 0-4 * * 1")];
                params.extend(time_ranges(&[(1, 2, 3, 8)]));
                params
            },
            "InvalidParameterValue",
        ),
        (
            "a cron expression of four fields",
            Names::TheWindow,
            vec![p("CronExpression", "* 0-4 * *")],
            CRON,
        ),
        (
            "a cron expression whose minute is not a wildcard",
            Names::TheWindow,
            vec![p("CronExpression", "0 0-4 * * 1")],
            CRON,
        ),
        (
            "a cron hour that is a single hour rather than a range",
            Names::TheWindow,
            vec![p("CronExpression", "* 5 * * 1")],
            CRON,
        ),
        (
            "a cron hour range one hour short of the smallest span",
            Names::TheWindow,
            vec![p("CronExpression", format!("* 0-{} * * 1", min_range - 1))],
            CRON,
        ),
        (
            "cron hour ranges one hour short of the smallest total",
            Names::TheWindow,
            vec![p("CronExpression", format!("* 0-{} * * 1", min_total - 1))],
            CRON,
        ),
        (
            "a cron hour past the last hour of the day",
            Names::TheWindow,
            vec![p("CronExpression", "* 0-24 * * 1")],
            CRON,
        ),
        (
            "a cron day of the week past the last day",
            Names::TheWindow,
            vec![p("CronExpression", "* 0-4 * * 7")],
            CRON,
        ),
        (
            "a cron expression longer than the service reads",
            Names::TheWindow,
            vec![p(
                "CronExpression",
                format!("* 0-4 * * {}", "1,".repeat(MAX_EVENT_WINDOW_CRON_BYTES)),
            )],
            CRON,
        ),
        (
            "a time range one hour short of the smallest span",
            Names::TheWindow,
            time_ranges(&[(1, 1, 1, 1 + min_range - 1)]),
            "InvalidParameterValue",
        ),
        (
            "time ranges one hour short of the smallest total",
            Names::TheWindow,
            time_ranges(&[(1, 0, 1, min_total - 1)]),
            "InvalidParameterValue",
        ),
        (
            "one time range more than the service holds",
            Names::TheWindow,
            time_ranges(
                &(0..=MAX_EVENT_WINDOW_TIME_RANGES)
                    .map(|_| (1, 0, 1, 2))
                    .collect::<Vec<_>>(),
            ),
            "InvalidParameterValue",
        ),
        (
            // `$StartWeekDay` targets the modelled `WeekDay` enum and `$StartHour` the
            // `@range(min: 0, max: 23)` `Hour`, so the boundary validator refuses a value
            // outside either before the handler runs.
            "a time range week day that names no day",
            Names::TheWindow,
            vec![
                p("TimeRange.1.StartWeekDay", "funday"),
                p("TimeRange.1.StartHour", "2"),
                p("TimeRange.1.EndWeekDay", "wednesday"),
                p("TimeRange.1.EndHour", "8"),
            ],
            "ValidationError",
        ),
        (
            "a time range start hour one past the last hour of the day",
            Names::TheWindow,
            time_ranges(&[(1, 24, 2, 8)]),
            "ValidationError",
        ),
        (
            "a time range end hour one before the first hour of the day",
            Names::TheWindow,
            time_ranges(&[(1, 2, 2, -1)]),
            "ValidationError",
        ),
        (
            "a time range with no end hour",
            Names::TheWindow,
            vec![
                p("TimeRange.1.StartWeekDay", "monday"),
                p("TimeRange.1.StartHour", "2"),
                p("TimeRange.1.EndWeekDay", "wednesday"),
            ],
            "MissingParameter",
        ),
    ]
}

#[tokio::test]
async fn a_refused_modify_leaves_the_window_exactly_as_it_was() {
    let created_name = Some("as-it-was");
    let created = Schedule::Cron("* 0-4 * * 1,5");
    for (case, names, rest, code) in refusals() {
        // A fresh store per row: what the row must leave untouched is this window.
        let router = emulator();
        let window_id = create_window(&router, case, created_name, &created).await;
        let before = described(&router, case, &window_id).await;

        let mut params = names.params(&window_id);
        params.extend(rest);
        let (status, body) = api(&router, "ModifyInstanceEventWindow", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected the error code {code}, got body {body}"
        );

        let after = described(&router, case, &window_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused modify must write nothing, \
             expected {before:?}, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_request_that_names_both_forms_of_schedule_is_refused_in_aws_words() {
    let router = emulator();
    let case = "both a cron expression and a time range";
    let window_id =
        create_window(&router, case, Some("says-both"), &Schedule::Cron("* 0-4 * * 1")).await;
    let mut params = vec![
        p("InstanceEventWindowId", window_id),
        p("CronExpression", "* 0-4 * * 1"),
    ];
    params.extend(time_ranges(&[(1, 2, 3, 8)]));
    let (status, body) = api(&router, "ModifyInstanceEventWindow", &params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    // The sentence names the two members and nothing the caller sent.
    assert_eq!(
        error_message(&body),
        "The parameter cronExpression cannot be used with the timeRangesSet.",
        "case {case:?}: expected the refusal of the two forms together, got body {body}"
    );
}

#[tokio::test]
async fn a_dry_run_modify_reports_what_it_would_have_done_and_writes_nothing() {
    let created_name = Some("dry-run-subject");
    let created = Schedule::Cron("* 0-4 * * 1,5");
    // Each row is a request that WOULD have been answered differently: the flag reports the
    // refusal a request had earned, and the dry run itself for one that had earned none.
    let rows: Vec<(&str, Params, u16, &str)> = vec![
        (
            "a modify that would have succeeded",
            vec![p("Name", "renamed-by-nobody"), p("DryRun", "true")],
            412,
            "DryRunOperation",
        ),
        (
            "a modify that would have been refused for its schedule",
            vec![p("CronExpression", "* 5 * * 1"), p("DryRun", "true")],
            400,
            "InvalidInstanceEventWindowCronExpression",
        ),
    ];
    for (case, rest, status, code) in rows {
        let router = emulator();
        let window_id = create_window(&router, case, created_name, &created).await;
        let before = described(&router, case, &window_id).await;

        let mut params = vec![p("InstanceEventWindowId", window_id.clone())];
        params.extend(rest);
        let (actual, body) = api(&router, "ModifyInstanceEventWindow", &params).await;
        assert_eq!(
            actual, status,
            "case {case:?}: expected HTTP {status}, got {actual} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            code,
            "case {case:?}: expected the error code {code}, got body {body}"
        );

        let after = described(&router, case, &window_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a DryRun modify must write nothing, \
             expected {before:?}, got {after:?}"
        );
    }
}
