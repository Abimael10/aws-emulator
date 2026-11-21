Issue-Source: trace-sweep-divergence
Issue-Trace: elasticbeanstalk-catalog-list-platform-branches
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticbeanstalk-catalog-list-platform-branches
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk|ListPlatformBranches|2xx->2xx|$.ListPlatformBranchesResponse.ListPlatformBranchesResult.PlatformBranchSummaryList.member[*]
Issue-Commit: 5e1b36ff45c8b7f0c028358ee44b1cec115a0c51

# ListPlatformBranches still omits the older generations of five platforms (Java, PHP, Python, Ruby, Tomcat), so their lists are shorter than the published inventory

> _Split out of `issues/things-that-now-work/20261029T235806Z-elasticbeanstalk-elastic-beanstalk-platform-branch-catalog-returns-wrong-branch.md`. That report's causes 1, 2, 3, 4 and 6 (the `.NET Core on Linux`, `.NET on Windows Server`, `Docker`, `GlassFish`, `Go` and `Node.js` platform filters) are fixed and their fixture steps replay clean; this report carries the replay key that does NOT pass yet, re-filed here so it is not recorded as working. A later fixer starts from here rather than from scratch._

## Operation
ListPlatformBranches

## Description
Five `PlatformName` filters still report a branch list SHORTER than the one the service
publishes: `Java` (task#6), `PHP` (task#8), `Python` (task#10), `Ruby` (task#11) and `Tomcat`
(task#12). Every member the parent report named is now served exactly as it named it — the
Corretto branches on Amazon Linux 2023 lead the `Java` platform at BranchOrder 11400 and
downwards, the `(BETA)` Amazon Linux 2 preview branches of PHP, Python and Ruby are published
beside their general branches, the Amazon Linux generation of Ruby is named the way the
service publishes it (`Puma with Ruby 2.6 running on 64bit Amazon Linux`), and the Supported
`Tomcat 11 with Corretto 25 / 21 / 17 running on 64bit Amazon Linux 2023` branches lead the
Tomcat platform at 12800 and downwards. What remains is INVENTORY DEPTH: each of these five
platforms published more generations than the parent report enumerates, and the ones still
missing sit at list positions the emulator does not reach.

AWS (rule): for every `PlatformName`, the operation reports that platform's whole published
branch inventory — every generation the platform ever published, the preview branch issued
beside a general branch and the older operating-system generations behind it — each under the
branch name the service publishes it by, with that branch's life-cycle state, with the
per-platform `BranchOrder` the platform issues in steps of 100 (newest highest, older
operating-system generations in lower bands), and with `SupportedTierList` holding both
documented tiers for a Linux branch. The branch inventory itself is service data: the Smithy
model constrains the member shapes (`PlatformBranchSummary{PlatformName, BranchName,
LifecycleState, BranchOrder, SupportedTierList}`) and says nothing about which branches exist,
so the requirement for each of these five platforms is the RECORDED catalog for that filter.

Emulator: HTTP 200 with a well-formed list that is correct through the generations the
emulator knows and then ends early, so a client enumerating a platform's history sees the
current and Amazon Linux 2 generations but not the oldest ones.

Known counts for the recorded lists, from the evidence of the two earlier reports of this same
fixture (`issues/things-that-now-work/20261022T233132Z-elasticbeanstalk-elastic-beanstalk-listplatformbranches-serves-a-placeholder-branch-catalog.md`):
`PHP` 26 members (the emulator now serves 19), `Ruby` 29 (now 16), `Tomcat` 26 (now 12),
`Python` 16 (now 13), `Java` at least 13 (now 9). The generations to add are the ones that
inventory has and this catalog does not: the 32-bit Amazon Linux branches of these runtimes,
the oldest 64-bit Amazon Linux ones (PHP 5.5/5.3, the Tomcat-with-Java-6 and Tomcat 6
branches, the older Puma/Passenger Standalone Ruby pairs), and the Amazon Linux 2 preview
branches of the Java SE and Tomcat platforms.

What was already tried, so it is not tried twice: the replay reports WHICH member positions
diverge but withholds the recorded values, and it reports only the lexicographically smallest
few paths of the first diverging step — so the names, states and ordinals of the missing
members cannot be read out of it, and a position below the first divergence cannot be observed
at all until every position above it matches. Two candidate generations for the `Java`
platform (its Amazon Linux 2 `(BETA) Corretto 11 / 8` preview branches at the band below the
Amazon Linux 2023 one, and a 32-bit `Java 8 / Java 7` generation at 9900/9800, in both
groupings of width and runtime) were replayed and refused at exactly member[10], member[11]
and member[12], so neither arrangement is the recorded one. Padding the lists with branch
names that cannot be grounded is not a fix: a branch the emulator invents makes an impossible
deployment look valid, which is the defect the parent report is about.

Fix: the inventory data in
`services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice/src/state/platform_catalog.rs`
(`MANAGED_PLATFORM_BRANCHES`), plus a `SOLUTION_STACKS` row for any added branch that has NOT
retired, so the platform-version surface keeps agreeing with the branch catalog. The handler
(`src/ops/list_platform_branches.rs`) needs no change: its filter grammar, conjunction,
ordering (`PlatformName`, then `BranchOrder` descending, then `BranchName`) and `NextToken`
paging already match the recording — the five steps that still diverge do so only on which
members the catalog holds.

Traces: elasticbeanstalk-catalog-list-platform-branches#task#6, #task#8, #task#10, #task#11,
#task#12.

## Evidence
Replay after the parent fix: the fixture went from 11 diverging steps to 5, `bound_step`
(task#1) is `not_among_divergences`, and `other_causes` is empty — so the parent fix caused
nothing here and the steps that remain are the ones listed above. The first of them reports:

- task#6 (`PlatformName = Java`), expected HTTP 200, actual HTTP 200 — differs at
  `$.ListPlatformBranchesResponse.ListPlatformBranchesResult.PlatformBranchSummaryList.member[10]`,
  `…member[11]`, `…member[12]` (recorded values withheld), so the recorded list holds at least
  11 members where the emulator holds 9.

The later steps are reported only once the one before them replays clean, which is why they
have no member positions here yet.

## Steps to Reproduce
1. Boot the emulator and point an AWS client at it (region us-west-2).
2. Call `eb.list_platform_branches(Filters=[{'Attribute':'PlatformName','Operator':'=','Values':[<PlatformName>]}])`
   for `<PlatformName>` in {'Java', 'PHP', 'Python', 'Ruby', 'Tomcat'} and count the members:
   9, 19, 13, 16 and 12 against the recorded 13+, 26, 16, 29 and 26.
3. Compare `PlatformBranchSummaryList` member for member against the recorded response:
   `BranchName`, `LifecycleState`, `BranchOrder` and `SupportedTierList`, in order. The leading
   members agree; the list ends before the oldest generations.
4. Re-replay fixture `elasticbeanstalk-catalog-list-platform-branches` steps task#6, task#8,
   task#10, task#11 and task#12.
