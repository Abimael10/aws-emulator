// SPDX-License-Identifier: CC0-1.0

//! The AWS Glue blueprint programming model a layout script finds on its import path: the
//! `awsglue.blueprint` package, and the module that turns what a layout RETURNED into the API
//! calls that deploy it.
//!
//! "A blueprint … contains a Python script that specifies the logic of a workflow and the
//! parameters" (`CreateBlueprint`): that script is published inside the archive at the
//! blueprint's `BlueprintLocation`, and the function `blueprint.cfg` names as its
//! `layoutGenerator` is called with the run's parameters and ANSWERS a `Workflow` object
//! holding the entities the run is to create. So the modules here are the two halves of that
//! contract:
//!
//! * the classes the layout builds its answer out of ([`WORKFLOW_SOURCE`], [`JOB_SOURCE`],
//!   [`CRAWLER_SOURCE`]) — ordinary Python classes that keep the members a layout names,
//!   spelled as the Glue API spells them, so nothing of the layout's own answer is
//!   interpreted or guessed at; and
//! * the walk that reads that answer back out ([`DEPLOYMENT_SOURCE`]) and the entry module
//!   that CREATES each entity through this emulator's own Glue API
//!   ([`super::LAYOUT_ENTRY_SOURCE`]), so every job, crawler and trigger a run reports as
//!   created was created by the operation that owns it and is readable through that
//!   operation's own reader afterwards.
//!
//! Nothing here is a bound or a containment: both are the interpreter's, in host code no
//! layout can reach ([`super`]).

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;

/// The modules this emulator puts on a blueprint layout's import path: the `awsglue.blueprint`
/// package and the deployment walk under it.
///
/// The caller adds the archive's OWN modules and the entry module that runs the layout,
/// because what runs a layout is the run's own concern ([`super`]).
#[must_use]
pub fn blueprint_library_modules() -> Vec<PackageModule> {
    [
        ("awsglue/__init__.py", AWSGLUE_SOURCE),
        ("awsglue/blueprint/__init__.py", BLUEPRINT_SOURCE),
        ("awsglue/blueprint/base_resource.py", BASE_RESOURCE_SOURCE),
        ("awsglue/blueprint/workflow.py", WORKFLOW_SOURCE),
        ("awsglue/blueprint/job.py", JOB_SOURCE),
        ("awsglue/blueprint/crawler.py", CRAWLER_SOURCE),
        ("awsglue/blueprint/_deployment.py", DEPLOYMENT_SOURCE),
    ]
    .into_iter()
    .map(|(path, source)| PackageModule {
        path: path.to_string(),
        source: source.to_string(),
    })
    .collect()
}

/// `awsglue`: the package the blueprint programming model lives under.
const AWSGLUE_SOURCE: &str = r#"
"""The `awsglue` package, as this emulator supplies it to a blueprint's layout."""
"#;

/// `awsglue.blueprint`: the package a layout imports its classes from.
const BLUEPRINT_SOURCE: &str = r#"
"""The AWS Glue blueprint programming model: the classes a layout returns its workflow in."""
"#;

/// `awsglue.blueprint.base_resource`: what every class below is.
///
/// A layout names the members of an entity as the Glue API spells them, so an entity object is
/// a bag of exactly those members: they are kept as given — a string stays a string, a nested
/// object stays that object — and read back by the deployment walk. Positional arguments are
/// bound to the member order each class declares, so a layout that writes its entities
/// positionally and one that writes them by keyword describe the same entity.
const BASE_RESOURCE_SOURCE: &str = r#"
"""The base of every entity a blueprint's layout returns."""


class BaseResource(object):
    """One entity, as the members the layout named it with."""

    # What kind of entity this class is, for an action that names the entity itself.
    ENTITY = None
    # The member order a positional argument is bound to.
    PARAMETERS = ()

    def __init__(self, *args, **kwargs):
        members = {}
        for name, value in zip(self.PARAMETERS, args):
            members[name] = value
        for name, value in kwargs.items():
            members[name] = value
        object.__setattr__(self, "_members", members)

    def __getattr__(self, name):
        members = object.__getattribute__(self, "_members")
        if name in members:
            return members[name]
        raise AttributeError(name)

    def __setattr__(self, name, value):
        object.__getattribute__(self, "_members")[name] = value

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, self.members)

    @property
    def members(self):
        """The members this entity carries, as the API spells them."""
        return dict(object.__getattribute__(self, "_members"))
"#;

/// `awsglue.blueprint.workflow`: the workflow a layout returns, its entities, and the triggers
/// inside it.
///
/// An action and a condition name what they initiate or wait on either by NAME
/// (`Action$JobName`) or by handing over the entity object itself, which is how a layout that
/// has just built that entity writes it; both are read here into the name the API carries.
const WORKFLOW_SOURCE: &str = r#"
"""The workflow a blueprint's layout returns, and what it holds."""

from awsglue.blueprint.base_resource import BaseResource

ON_DEMAND = "ON_DEMAND"
SCHEDULED = "SCHEDULED"
CONDITIONAL = "CONDITIONAL"
EVENT = "EVENT"


def _named_entity(value):
    """The `<kind>Name` member the entity `value` is named by, and the name itself."""
    if isinstance(value, BaseResource):
        kind = value.ENTITY or "Job"
        return ("%sName" % kind, value.members.get("Name"))
    return ("JobName", value)


class Entities(BaseResource):
    """The jobs, crawlers and triggers a workflow is made of."""

    PARAMETERS = ("Jobs", "Crawlers", "Triggers")


class Workflow(BaseResource):
    """The workflow a run deploys: its name and the entities inside it."""

    ENTITY = "Workflow"
    PARAMETERS = (
        "Name",
        "Entities",
        "OnSchedule",
        "Description",
        "DefaultRunProperties",
        "Tags",
        "MaxConcurrentRuns",
    )


class Action(BaseResource):
    """One action a trigger initiates: the job or crawler it runs."""

    PARAMETERS = (
        "JobName",
        "Arguments",
        "Timeout",
        "SecurityConfiguration",
        "NotificationProperty",
    )

    def __init__(self, *args, **kwargs):
        named = {}
        positional = []
        for value in args:
            if isinstance(value, BaseResource):
                member, name = _named_entity(value)
                named[member] = name
            else:
                positional.append(value)
        for member in ("Job", "Crawler"):
            entity = kwargs.pop(member, None)
            if entity is not None:
                named["%sName" % member] = _named_entity(entity)[1]
        named.update(kwargs)
        BaseResource.__init__(self, *positional, **named)


class Condition(BaseResource):
    """One condition a conditional trigger waits on."""

    PARAMETERS = ("LogicalOperator", "JobName", "State", "CrawlerName", "CrawlState")

    def __init__(self, *args, **kwargs):
        named = {}
        positional = []
        for value in args:
            if isinstance(value, BaseResource):
                member, name = _named_entity(value)
                named[member] = name
            else:
                positional.append(value)
        for member in ("Job", "Crawler"):
            entity = kwargs.pop(member, None)
            if entity is not None:
                named["%sName" % member] = _named_entity(entity)[1]
        named.update(kwargs)
        BaseResource.__init__(self, *positional, **named)


class Predicate(BaseResource):
    """When a conditional trigger fires: the conditions, and how they are combined."""

    PARAMETERS = ("Logical", "Conditions")


class Trigger(BaseResource):
    """A trigger of the workflow: what it initiates and when."""

    ENTITY = "Trigger"
    TYPE = None
    PARAMETERS = (
        "Name",
        "Type",
        "Actions",
        "Schedule",
        "Predicate",
        "Description",
        "StartOnCreation",
        "Tags",
        "EventBatchingCondition",
        "WorkflowName",
    )

    def __init__(self, *args, **kwargs):
        if self.TYPE is not None and "Type" not in kwargs:
            kwargs = dict(kwargs, Type=self.TYPE)
        BaseResource.__init__(self, *args, **kwargs)


class OnDemandTrigger(Trigger):
    """A trigger a caller fires."""

    TYPE = ON_DEMAND
    PARAMETERS = (
        "Name",
        "Actions",
        "Description",
        "Tags",
        "WorkflowName",
    )


class ScheduledTrigger(Trigger):
    """A trigger the clock fires, on the cron schedule it carries."""

    TYPE = SCHEDULED
    PARAMETERS = (
        "Name",
        "Schedule",
        "Actions",
        "Description",
        "StartOnCreation",
        "Tags",
        "WorkflowName",
    )


class ConditionalTrigger(Trigger):
    """A trigger the states of this workflow's own entities fire."""

    TYPE = CONDITIONAL
    PARAMETERS = (
        "Name",
        "Predicate",
        "Actions",
        "Description",
        "StartOnCreation",
        "Tags",
        "WorkflowName",
    )


class EventTrigger(Trigger):
    """A trigger the events it batches fire."""

    TYPE = EVENT
    PARAMETERS = (
        "Name",
        "Actions",
        "EventBatchingCondition",
        "Description",
        "StartOnCreation",
        "Tags",
        "WorkflowName",
    )
"#;

/// `awsglue.blueprint.job`: the job a layout adds to its workflow.
const JOB_SOURCE: &str = r#"
"""The job entity of a blueprint's layout."""

from awsglue.blueprint.base_resource import BaseResource


class Job(BaseResource):
    """A job definition the blueprint run creates."""

    ENTITY = "Job"
    PARAMETERS = (
        "Name",
        "Role",
        "Command",
        "Description",
        "DefaultArguments",
        "NonOverridableArguments",
        "Connections",
        "MaxRetries",
        "AllocatedCapacity",
        "Timeout",
        "MaxCapacity",
        "SecurityConfiguration",
        "Tags",
        "NotificationProperty",
        "GlueVersion",
        "NumberOfWorkers",
        "WorkerType",
        "ExecutionProperty",
        "ExecutionClass",
        "JobMode",
        "LogUri",
        "SourceControlDetails",
        "MaintenanceWindow",
        "CodeGenConfigurationNodes",
    )
"#;

/// `awsglue.blueprint.crawler`: the crawler a layout adds to its workflow, and the targets it
/// crawls.
const CRAWLER_SOURCE: &str = r#"
"""The crawler entity of a blueprint's layout, and the targets it crawls."""

from awsglue.blueprint.base_resource import BaseResource


class Crawler(BaseResource):
    """A crawler the blueprint run creates."""

    ENTITY = "Crawler"
    PARAMETERS = (
        "Name",
        "Role",
        "Targets",
        "DatabaseName",
        "Description",
        "Classifiers",
        "TablePrefix",
        "SchemaChangePolicy",
        "RecrawlPolicy",
        "Schedule",
        "Configuration",
        "CrawlerSecurityConfiguration",
        "Tags",
        "LakeFormationConfiguration",
        "LineageConfiguration",
    )


class S3Target(BaseResource):
    """An Amazon S3 prefix a crawler crawls."""

    PARAMETERS = (
        "Path",
        "Exclusions",
        "ConnectionName",
        "SampleSize",
        "EventQueueArn",
        "DlqEventQueueArn",
    )


class JdbcTarget(BaseResource):
    """A JDBC source a crawler crawls."""

    PARAMETERS = ("ConnectionName", "Path", "Exclusions", "EnableAdditionalMetadata")


class DynamoDBTarget(BaseResource):
    """An Amazon DynamoDB table a crawler crawls."""

    PARAMETERS = ("Path", "scanAll", "scanRate")


class MongoDBTarget(BaseResource):
    """A MongoDB collection a crawler crawls."""

    PARAMETERS = ("ConnectionName", "Path", "ScanAll")


class CatalogTarget(BaseResource):
    """Data Catalog tables a crawler crawls."""

    PARAMETERS = (
        "DatabaseName",
        "Tables",
        "ConnectionName",
        "EventQueueArn",
        "DlqEventQueueArn",
    )
"#;

/// `awsglue.blueprint._deployment`: the walk that reads the workflow a layout RETURNED and
/// answers the entities to create, in the order they must be created.
///
/// Every member of every entity travels through unchanged: the walk only unwraps the entity
/// objects into the maps the API takes, so a member this emulator's own `CreateJob`,
/// `CreateCrawler` or `CreateTrigger` refuses is refused by that operation rather than dropped
/// here.
///
/// A trigger the layout carries is attached to the workflow the run deploys, and a layout that
/// carries no trigger able to BEGIN a run of that workflow is given the workflow's own starting
/// trigger over the entities nothing else initiates — scheduled when the workflow names an
/// `OnSchedule`, on-demand otherwise.
///
/// RECORDED BEHAVIOUR, not a choice of this emulator's: a blueprint whose layout returns a
/// workflow with NO entity fails its run with a TRIGGER's own refusal of an empty `Actions`
/// list. A deployment that created only what the layout declared would have created no trigger
/// at all for such a layout, so nothing would have refused it — the refusal is the evidence
/// that the deployment itself creates the workflow's starting trigger from the layout's
/// entities, and that is what this builds. It is also why the sample layouts AWS publishes
/// declare a `Job` and no trigger and still produce a runnable workflow.
const DEPLOYMENT_SOURCE: &str = r#"
"""Turning the workflow a layout returned into the calls that deploy it."""

from awsglue.blueprint.base_resource import BaseResource
from awsglue.blueprint.workflow import CONDITIONAL, ON_DEMAND, SCHEDULED

# The name the workflow's own starting trigger is created under, beside the workflow's name.
STARTING_TRIGGER_SUFFIX = "_starting_trigger"

# The members of the workflow itself a deployment applies to the workflow the run created.
WORKFLOW_MEMBERS = ("Description", "DefaultRunProperties", "MaxConcurrentRuns")


class DeploymentRefused(Exception):
    """A creation the deployment made that the service REFUSED, carrying its own message.

    What the blueprint run then reports is that message and nothing added to it: the refusal
    that failed the deployment is the service's own.
    """


def created(call, parameters):
    """Make one creating call, raising the service's own message when it is refused."""
    try:
        call(**parameters)
    except DeploymentRefused:
        raise
    except Exception as refused:
        raise DeploymentRefused(_refusal_message(refused))
    return parameters.get("Name")


def _refusal_message(refused):
    """The service's own `Message` for a refusal, and the error itself where it carries none."""
    response = getattr(refused, "response", None)
    if isinstance(response, dict):
        error = response.get("Error")
        if isinstance(error, dict):
            message = error.get("Message")
            if message:
                return message
    return str(refused)


def api_value(value):
    """`value` as the API carries it: an entity object as its members, deeply."""
    if isinstance(value, BaseResource):
        return api_value(value.members)
    if isinstance(value, dict):
        return dict((str(name), api_value(held)) for name, held in value.items())
    if isinstance(value, (list, tuple)):
        return [api_value(held) for held in value]
    return value


def _members(value):
    """The members of an entity object, or of a plain mapping, and nothing for anything else."""
    if isinstance(value, BaseResource):
        return value.members
    if isinstance(value, dict):
        return dict(value)
    return {}


def _listed(value):
    """`value` as a list: a single entity counts as a list of one, and nothing as empty."""
    if value is None:
        return []
    if isinstance(value, (list, tuple)):
        return list(value)
    return [value]


def _initiated(triggers):
    """Every (kind, name) pair the triggers already initiate."""
    initiated = set()
    for trigger in triggers:
        for action in _listed(trigger.get("Actions")):
            action = api_value(action)
            for kind in ("Job", "Crawler"):
                named = action.get("%sName" % kind)
                if named:
                    initiated.add((kind, named))
    return initiated


def _starting_trigger(workflow_name, workflow, jobs, crawlers, triggers):
    """The workflow's own starting trigger, or nothing when a trigger can already begin a run.

    A conditional trigger fires on the states of this workflow's own entities, so it can only
    continue a run; any other type fires on something outside the workflow and so is itself an
    entry point. The trigger is built over the entities nothing else initiates — which is NO
    action at all for a layout that created no entity, and such a trigger is what the trigger
    create then refuses, failing the run, which is what the recorded zero-entity run comes to.
    """
    for trigger in triggers:
        if trigger.get("Type") != CONDITIONAL:
            return None
    initiated = _initiated(triggers)
    actions = []
    for job in jobs:
        name = job.get("Name")
        if name and ("Job", name) not in initiated:
            actions.append({"JobName": name})
    for crawler in crawlers:
        name = crawler.get("Name")
        if name and ("Crawler", name) not in initiated:
            actions.append({"CrawlerName": name})
    starting = {
        "Name": "%s%s" % (workflow_name, STARTING_TRIGGER_SUFFIX),
        "WorkflowName": workflow_name,
        "Actions": actions,
    }
    schedule = workflow.get("OnSchedule")
    if schedule:
        starting["Type"] = SCHEDULED
        starting["Schedule"] = schedule
    else:
        starting["Type"] = ON_DEMAND
    return starting


def deployment(workflow, workflow_name):
    """What deploying `workflow` into the workflow `workflow_name` amounts to."""
    workflow = _members(workflow)
    entities = _members(workflow.get("Entities"))
    jobs = [api_value(job) for job in _listed(entities.get("Jobs"))]
    crawlers = [api_value(crawler) for crawler in _listed(entities.get("Crawlers"))]
    triggers = []
    for trigger in _listed(entities.get("Triggers")):
        trigger = api_value(trigger)
        # Every trigger a layout carries belongs to the workflow the run deploys, whatever
        # the layout named, because that workflow is what the run created.
        trigger["WorkflowName"] = workflow_name
        triggers.append(trigger)
    starting = _starting_trigger(workflow_name, workflow, jobs, crawlers, triggers)
    if starting is not None:
        triggers.append(starting)
    applied = {}
    for member in WORKFLOW_MEMBERS:
        held = workflow.get(member)
        if held is not None:
            applied[member] = api_value(held)
    return {"workflow": applied, "jobs": jobs, "crawlers": crawlers, "triggers": triggers}
"#;
