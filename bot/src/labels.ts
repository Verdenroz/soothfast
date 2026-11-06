import { parseBotConfig, type BotConfig } from "./config.ts";
import {
  GitHubError,
  type GitHubApi,
  type IssueEvent,
  type PullRequest,
} from "./github.ts";
import type { Labeler } from "./webhook.ts";

export type Action =
  | { kind: "disable_auto_merge" }
  | { kind: "update_branch" }
  | { kind: "enable_auto_merge" }
  | { kind: "skip"; reason: string };

export interface Gated {
  update: string[];
  merge: string[];
}

export interface Run {
  github: GitHubApi;
  token: string;
  repository: string;
  defaultBranch: string;
  log: (message: string) => void;
}

interface Context {
  run: Run;
  config: BotConfig;
  checked: () => Promise<number>;
  permission: (login: string) => Promise<string>;
}

// Stays under the Workers subrequest cap on the free plan.
export const MAX_EVALUATIONS = 8;

const WRITE_PERMISSIONS: readonly string[] = ["admin", "maintain", "write"];
const CHECK_RULES: readonly string[] = [
  "required_status_checks",
  "merge_queue",
];

const present = (pr: PullRequest, wanted: string[]) =>
  pr.labels.map((l) => l.name).filter((name) => wanted.includes(name));

export function gated(pr: PullRequest, config: BotConfig): Gated {
  if (present(pr, config.ignoreLabels).length > 0)
    return { update: [], merge: [] };
  const mergeable = !pr.draft && pr.auto_merge === null;
  return {
    update: present(pr, config.updateLabels),
    merge: mergeable ? present(pr, config.mergeLabels) : [],
  };
}

export function decide(
  pr: PullRequest,
  config: BotConfig,
  checked: number,
  behind: boolean,
  writers: ReadonlySet<string>,
): Action[] {
  if (present(pr, config.ignoreLabels).length > 0)
    return pr.auto_merge === null ? [] : [{ kind: "disable_auto_merge" }];
  const { update, merge } = gated(pr, config);
  const actions: Action[] = [];
  if (behind && update.some((l) => writers.has(l)))
    actions.push({ kind: "update_branch" });
  if (merge.some((l) => writers.has(l))) {
    actions.push(
      checked === 0
        ? {
            kind: "skip",
            reason: "the default branch requires no status check",
          }
        : { kind: "enable_auto_merge" },
    );
  }
  return actions;
}

export function lastLabeler(
  events: IssueEvent[],
  label: string,
): string | undefined {
  return [...events]
    .reverse()
    .find((e) => e.event === "labeled" && e.label?.name === label)?.actor
    ?.login;
}

export async function evaluatePullRequest(
  run: Run,
  number: number,
  labeler?: Labeler,
): Promise<void> {
  const context = await load(run);
  if (!context) return;
  const pr = await run.github.pullRequest(run.repository, number, run.token);
  await evaluate(context, pr, labeler);
}

export async function evaluateOpen(
  run: Run,
  scope: "update" | "any",
): Promise<void> {
  const context = await load(run);
  if (!context) return;
  const { config } = context;
  const wanted =
    scope === "update"
      ? config.updateLabels
      : [...config.mergeLabels, ...config.updateLabels, ...config.ignoreLabels];
  const open = await run.github.openPullRequests(
    run.repository,
    run.defaultBranch,
    run.token,
  );
  const matching = open.filter((pr) => present(pr, wanted).length > 0);
  if (matching.length > MAX_EVALUATIONS)
    run.log(
      `evaluating ${MAX_EVALUATIONS} of ${matching.length} pull requests`,
    );
  for (const pr of matching.slice(0, MAX_EVALUATIONS)) {
    await evaluate(context, pr).catch((e: unknown) =>
      run.log(`#${pr.number}: ${e instanceof Error ? e.message : e}`),
    );
  }
}

async function load(run: Run): Promise<Context | undefined> {
  const toml = await run.github.fileContents(
    run.repository,
    "soothfast.toml",
    run.defaultBranch,
    run.token,
  );
  const parsed = parseBotConfig(toml ?? "");
  if (!parsed.ok) {
    run.log(`ignoring labels: ${parsed.reason}`);
    return undefined;
  }
  if (!parsed.config.labels) return undefined;
  return {
    run,
    config: parsed.config,
    checked: memo(() => requiredChecks(run)),
    permission: memoBy((login) =>
      run.github.collaboratorPermission(run.repository, login, run.token),
    ),
  };
}

function memo<T>(compute: () => Promise<T>): () => Promise<T> {
  let cached: Promise<T> | undefined;
  return () => (cached ??= compute());
}

function memoBy<T>(compute: (key: string) => Promise<T>) {
  const cache = new Map<string, Promise<T>>();
  return (key: string) => {
    const hit = cache.get(key) ?? compute(key);
    cache.set(key, hit);
    return hit;
  };
}

async function requiredChecks(run: Run): Promise<number> {
  const { github, repository, defaultBranch: branch, token } = run;
  const rules = await readable(run, "branch rules", [], () =>
    github.branchRules(repository, branch, token),
  );
  const info = await readable(run, "branch protection", {}, () =>
    github.branchProtection(repository, branch, token),
  );
  const classic = info.protection?.required_status_checks;
  return (
    rules.filter((r) => CHECK_RULES.includes(r.type)).length +
    (classic?.contexts?.length ?? 0) +
    (classic?.checks?.length ?? 0)
  );
}

async function readable<T>(
  run: Run,
  what: string,
  empty: T,
  read: () => Promise<T>,
): Promise<T> {
  return read().catch((e: unknown) => {
    if (!(e instanceof GitHubError && [403, 404].includes(e.status))) throw e;
    run.log(`${what} unreadable (${e.status}); counting no required checks`);
    return empty;
  });
}

async function evaluate(
  context: Context,
  pr: PullRequest,
  labeler?: Labeler,
): Promise<void> {
  const { run, config } = context;
  const { update, merge } = gated(pr, config);
  const writers = await writersOf(context, pr, [...update, ...merge], labeler);
  const needsChecks = merge.some((l) => writers.has(l));
  const checked = needsChecks ? await context.checked() : 0;
  const behind =
    update.some((l) => writers.has(l)) && (await isBehind(run, pr));
  for (const action of decide(pr, config, checked, behind, writers))
    await perform(context, pr, action);
}

async function isBehind(run: Run, pr: PullRequest): Promise<boolean> {
  const { status } = await run.github.compare(
    run.repository,
    run.defaultBranch,
    pr.head.sha,
    run.token,
  );
  return status === "behind" || status === "diverged";
}

async function writersOf(
  context: Context,
  pr: PullRequest,
  labels: string[],
  labeler?: Labeler,
): Promise<Set<string>> {
  const { run } = context;
  const events = memo(() =>
    run.github.issueEvents(run.repository, pr.number, run.token),
  );
  const writers = new Set<string>();
  for (const label of labels) {
    const login =
      labeler?.label === label
        ? labeler.login
        : lastLabeler(await events(), label);
    if (!login) continue;
    const permission = await context.permission(login);
    if (WRITE_PERMISSIONS.includes(permission)) writers.add(label);
    else
      run.log(
        `#${pr.number}: ignoring ${label}, applied by ${login} (${permission}), who cannot write here`,
      );
  }
  return writers;
}

async function perform(
  { run, config }: Context,
  pr: PullRequest,
  action: Action,
): Promise<void> {
  const { github, repository, token } = run;
  const tag = `#${pr.number}`;
  switch (action.kind) {
    case "disable_auto_merge":
      await github.disableAutoMerge(pr.node_id, token);
      run.log(`${tag}: auto-merge cancelled`);
      return;
    case "enable_auto_merge":
      await github.enableAutoMerge(pr.node_id, config.mergeMethod, token);
      run.log(`${tag}: auto-merge enabled`);
      return;
    case "update_branch":
      await github
        .updateBranch(repository, pr.number, pr.head.sha, token)
        .then(() => run.log(`${tag}: updated from ${run.defaultBranch}`))
        .catch((e: unknown) => {
          if (!(e instanceof GitHubError)) throw e;
          run.log(`${tag}: could not update (${e.status}: ${e.message})`);
        });
      return;
    case "skip":
      run.log(`${tag}: not merging, ${action.reason}`);
  }
}
