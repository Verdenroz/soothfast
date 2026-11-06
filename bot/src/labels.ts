import { parseBotConfig, type BotConfig } from "./config.ts";
import { GitHubError, type GitHubApi, type PullRequest } from "./github.ts";
import type { Labeler } from "./webhook.ts";

type Action =
  | { kind: "disable_auto_merge" }
  | { kind: "update_branch" }
  | { kind: "enable_auto_merge"; label: string }
  | { kind: "skip"; reason: string };

interface Gated {
  ignored: boolean;
  declined?: string;
  update: string[];
  merge: string[];
}

export interface Run {
  github: GitHubApi;
  token: string;
  repositoryId: number;
  repository: string;
  defaultBranch: string;
  fresh: boolean;
  log: (message: string) => void;
}

interface Context {
  run: Run;
  config: BotConfig;
  checked: () => Promise<number>;
  permission: (login: string) => Promise<string>;
}

// Each pull request costs up to four subrequests; with the fixed overhead of
// a delivery, six stay under the free plan's cap of 50.
export const MAX_EVALUATIONS = 6;

const CONFIG_TTL_MS = 60_000;
type Parsed = ReturnType<typeof parseBotConfig>;
const configs = new Map<number, { fetchedAt: number; parsed: Parsed }>();

const WRITE_PERMISSIONS: readonly string[] = ["admin", "maintain", "write"];
const CHECK_RULES: readonly string[] = [
  "required_status_checks",
  "merge_queue",
];

const present = (pr: PullRequest, wanted: string[]) =>
  pr.labels.map((l) => l.name).filter((name) => wanted.includes(name));

// GitHub drops auto-merge when someone without write access pushes to the
// head, so only a writer applying the label may enable it, never a later event.
export function gated(
  pr: PullRequest,
  config: BotConfig,
  labeler?: Labeler,
): Gated {
  if (present(pr, config.ignoreLabels).length > 0)
    return { ignored: true, update: [], merge: [] };
  const mergeable = !pr.draft && pr.auto_merge === null;
  const applied = present(pr, config.mergeLabels).filter(
    (label) => label === labeler?.label,
  );
  const declined =
    applied.length > 0 && !mergeable
      ? pr.draft
        ? "it is a draft"
        : "auto-merge is already enabled"
      : undefined;
  return {
    ignored: false,
    declined,
    update: present(pr, config.updateLabels),
    merge: mergeable ? applied : [],
  };
}

export function decide(
  pr: PullRequest,
  { ignored, update, merge }: Gated,
  checked: number,
  behind: boolean,
): Action[] {
  if (ignored)
    return pr.auto_merge === null ? [] : [{ kind: "disable_auto_merge" }];
  const actions: Action[] = [];
  if (behind && update.length > 0) actions.push({ kind: "update_branch" });
  if (merge.length > 0) {
    actions.push(
      checked === 0
        ? {
            kind: "skip",
            reason: "the default branch requires no status check",
          }
        : { kind: "enable_auto_merge", label: merge[0] },
    );
  }
  return actions;
}

export async function evaluatePullRequest(
  run: Run,
  pr: PullRequest,
  labeler?: Labeler,
): Promise<void> {
  const context = await load(run);
  if (context) await evaluate(context, pr, labeler);
}

export async function evaluateOpen(run: Run): Promise<void> {
  const context = await load(run);
  if (!context) return;
  const open = await run.github.openPullRequests(
    run.repository,
    run.defaultBranch,
    run.token,
  );
  const actionable = open.filter((pr) => canAct(pr, context.config));
  if (actionable.length === 0) run.log("no pull requests to act on");
  if (actionable.length > MAX_EVALUATIONS)
    run.log(
      `evaluating ${MAX_EVALUATIONS} of ${actionable.length} pull requests`,
    );
  for (const pr of actionable.slice(0, MAX_EVALUATIONS)) {
    await evaluate(context, pr).catch((e: unknown) =>
      run.log(`#${pr.number}: ${e instanceof Error ? e.message : e}`),
    );
  }
}

function canAct(pr: PullRequest, config: BotConfig): boolean {
  const { ignored, update } = gated(pr, config);
  return ignored ? pr.auto_merge !== null : !pr.draft && update.length > 0;
}

async function load(run: Run): Promise<Context | undefined> {
  const parsed = await botConfig(run);
  if (!parsed.ok || !parsed.config.labels) {
    run.log(offNotice(parsed, run.defaultBranch));
    return undefined;
  }
  return {
    run,
    config: parsed.config,
    checked: memo(() => requiredChecks(run)),
    permission: memoBy((login) =>
      run.github.collaboratorPermission(run.repository, login, run.token),
    ),
  };
}

const offNotice = (parsed: Parsed, defaultBranch: string) =>
  parsed.ok
    ? `labels are off: set [bot] labels = true in soothfast.toml on ${defaultBranch}`
    : `ignoring labels: ${parsed.reason}`;

function cachedConfig(repositoryId: number): Parsed | undefined {
  const hit = configs.get(repositoryId);
  if (!hit) return undefined;
  if (Date.now() - hit.fetchedAt < CONFIG_TTL_MS) return hit.parsed;
  configs.delete(repositoryId);
  return undefined;
}

export function cachedOff(
  repositoryId: number,
  defaultBranch: string,
): string | undefined {
  const parsed = cachedConfig(repositoryId);
  const off = parsed && (!parsed.ok || !parsed.config.labels);
  return off ? offNotice(parsed, defaultBranch) : undefined;
}

async function botConfig(run: Run): Promise<Parsed> {
  const cached = run.fresh ? undefined : cachedConfig(run.repositoryId);
  if (cached) return cached;
  const toml = await run.github.fileContents(
    run.repository,
    "soothfast.toml",
    run.defaultBranch,
    run.token,
  );
  const parsed = parseBotConfig(toml ?? "");
  configs.set(run.repositoryId, { fetchedAt: Date.now(), parsed });
  return parsed;
}

function memoBy<T>(compute: (key: string) => Promise<T>) {
  const cache = new Map<string, Promise<T>>();
  return (key: string) => {
    let hit = cache.get(key);
    if (!hit) {
      hit = compute(key).catch((e: unknown) => {
        cache.delete(key);
        throw e;
      });
      cache.set(key, hit);
    }
    return hit;
  };
}

function memo<T>(compute: () => Promise<T>): () => Promise<T> {
  const once = memoBy(compute);
  return () => once("");
}

async function requiredChecks(run: Run): Promise<number> {
  const { github, repository, defaultBranch: branch, token } = run;
  const [rules, info] = await Promise.all([
    readable(run, "branch rules", [], () =>
      github.branchRules(repository, branch, token),
    ),
    readable(run, "branch protection", {}, () =>
      github.branchProtection(repository, branch, token),
    ),
  ]);
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
  const initial = gated(pr, config, labeler);
  if (initial.declined)
    run.log(`#${pr.number}: not merging, ${initial.declined}`);
  const gate = await byWriters(context, pr, initial, labeler);
  const checked = gate.merge.length > 0 ? await context.checked() : 0;
  const behind = gate.update.length > 0 && (await isBehind(run, pr));
  for (const action of decide(pr, gate, checked, behind))
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

async function byWriters(
  { run, permission }: Context,
  pr: PullRequest,
  gate: Gated,
  labeler?: Labeler,
): Promise<Gated> {
  const appliedBy = (label: string) =>
    labeler?.label === label
      ? labeler.login
      : run.github.lastLabeler(run.repository, pr.number, label, run.token);
  const writes = async (label: string) => {
    const login = await appliedBy(label);
    if (!login) return false;
    const level = await permission(login);
    if (WRITE_PERMISSIONS.includes(level)) return true;
    run.log(
      `#${pr.number}: ignoring ${label}, applied by ${login} (${level}), who cannot write here`,
    );
    return false;
  };
  const keep = async (labels: string[]) => {
    const kept: string[] = [];
    for (const label of labels) if (await writes(label)) kept.push(label);
    return kept;
  };
  return {
    ...gate,
    update: await keep(gate.update),
    merge: await keep(gate.merge),
  };
}

// A delivery can be late, replayed or overtaken by a label removal, and its
// payload is only a snapshot: enabling needs the live state to still match it.
function unchanged(current: PullRequest, seen: PullRequest, label: string) {
  return (
    current.labels.some((l) => l.name === label) &&
    current.auto_merge === null &&
    !current.draft &&
    current.head.sha === seen.head.sha
  );
}

const isAlreadyMergeable = (e: unknown) =>
  e instanceof GitHubError &&
  /(clean|unstable|has_hooks) status/i.test(e.message);

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
    case "enable_auto_merge": {
      const current = await github.pullRequest(repository, pr.number, token);
      if (!unchanged(current, pr, action.label)) {
        run.log(
          `${tag}: not merging, it changed since ${action.label} was applied`,
        );
        return;
      }
      try {
        await github.enableAutoMerge(
          pr.node_id,
          config.mergeMethod,
          pr.head.sha,
          token,
        );
        run.log(`${tag}: auto-merge enabled`);
        return;
      } catch (e) {
        if (!isAlreadyMergeable(e)) throw e;
      }
      try {
        await github.mergePullRequest(
          repository,
          pr.number,
          pr.head.sha,
          config.mergeMethod,
          token,
        );
        run.log(`${tag}: merged (checks had already passed)`);
      } catch (e) {
        if (!(e instanceof GitHubError)) throw e;
        run.log(`${tag}: could not merge (${e.status}: ${e.message})`);
      }
      return;
    }
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
