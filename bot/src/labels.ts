import { parseBotConfig, type BotConfig } from "./config.ts";
import {
  GitHubError,
  type Comparison,
  type GitHubApi,
  type PullRequest,
} from "./github.ts";
import type { Labeler } from "./webhook.ts";

type Action =
  | { kind: "disable_auto_merge" }
  | { kind: "update_branch" }
  | { kind: "close_landing" }
  | { kind: "enable_auto_merge"; label: string }
  | { kind: "skip"; reason: string };

interface Gated {
  ignored: boolean;
  declined?: string;
  updateAll: boolean;
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
  policy: () => Promise<Policy>;
  permission: (login: string) => Promise<string>;
}

// Free plan cap of 50, minus the action sweep's worst case outside the count:
// OIDC 2, app slug, installation, mint, ref 3, revoke.
export const SUBREQUEST_BUDGET = 41;
const WORST_CASE_ALWAYS = 4;
const WORST_CASE_PER_PULL_REQUEST = 5;
const SHARED_READS = 2;
const COMPARE_FILE_CAP = 300;

const CONFIG_TTL_MS = 60_000;
type Parsed = ReturnType<typeof parseBotConfig>;
const configs = new Map<number, { fetchedAt: number; parsed: Parsed }>();

const WRITE_PERMISSIONS: readonly string[] = ["admin", "maintain", "write"];
const CHECK_RULES: readonly string[] = [
  "required_status_checks",
  "merge_queue",
];

export const isLanding = (pr: PullRequest, botLogin?: string) =>
  botLogin !== undefined &&
  pr.user.login === botLogin &&
  pr.head.ref.startsWith("bot/") &&
  pr.head.repo?.full_name === pr.base.repo.full_name;

// The live read carries mergeable; null means GitHub is still computing it.
// A head that moved since the list read is a landing that was just rebuilt.
export const conflicting = (pr: PullRequest, live: PullRequest) =>
  live.mergeable === false && live.head.sha === pr.head.sha;

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
    return { ignored: true, updateAll: false, update: [], merge: [] };
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
    updateAll: config.updateAll && !pr.draft,
    update: present(pr, config.updateLabels),
    merge: mergeable ? applied : [],
  };
}

// update_all is read from the default branch, which only writers can change,
// so it needs no labeler check.
const wantsUpdate = (gate: Gated) => gate.updateAll || gate.update.length > 0;

export function decide(
  pr: PullRequest,
  gate: Gated,
  checked: number,
  behind: boolean,
  conflicts = false,
): Action[] {
  const { ignored, merge } = gate;
  if (ignored)
    return pr.auto_merge === null ? [] : [{ kind: "disable_auto_merge" }];
  if (conflicts) return [{ kind: "close_landing" }];
  const actions: Action[] = [];
  if (behind && wantsUpdate(gate)) actions.push({ kind: "update_branch" });
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

export async function evaluateOpen(source: Run): Promise<void> {
  let spent = 0;
  const run = { ...source, github: counted(source.github, () => spent++) };
  const context = await load(run);
  if (!context) return;
  const open = await run.github.openPullRequests(
    run.repository,
    run.defaultBranch,
    run.token,
  );
  const always = context.config.update === "always";
  const queuedFirst = (pr: PullRequest) =>
    always || pr.auto_merge === null ? 1 : 0;
  const actionable = open
    .filter((pr) => canAct(pr, context.config))
    .sort(
      (a, b) =>
        queuedFirst(a) - queuedFirst(b) ||
        a.updated_at.localeCompare(b.updated_at),
    );
  if (actionable.length === 0) run.log("no pull requests to act on");
  const worst = always ? WORST_CASE_ALWAYS : WORST_CASE_PER_PULL_REQUEST;
  const reserved = always ? 0 : SHARED_READS;
  const quota = { strict: 1 };
  let reached = 0;
  for (const pr of actionable) {
    if (SUBREQUEST_BUDGET - reserved - spent < worst) break;
    reached++;
    await evaluate(context, pr, undefined, quota).catch((e: unknown) =>
      run.log(`#${pr.number}: ${e instanceof Error ? e.message : e}`),
    );
  }
  if (reached < actionable.length)
    run.log(`evaluating ${reached} of ${actionable.length} pull requests`);
}

function counted(github: GitHubApi, spend: () => void): GitHubApi {
  return new Proxy(github, {
    get(target, name) {
      const member = Reflect.get(target, name);
      if (typeof member !== "function") return member;
      return (...args: unknown[]) => {
        spend();
        return member.apply(target, args);
      };
    },
  });
}

function canAct(pr: PullRequest, config: BotConfig): boolean {
  const gate = gated(pr, config);
  return gate.ignored ? pr.auto_merge !== null : !pr.draft && wantsUpdate(gate);
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
    policy: memo(() => branchPolicy(run)),
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

interface Policy {
  checks: number;
  strict: boolean;
}

// Unreadable rules or protection, and enforced classic protection with no
// strict flag, count as strict: an unupdated pull request may never merge.
async function branchPolicy(run: Run): Promise<Policy> {
  const { github, repository, defaultBranch: branch, token } = run;
  const [rules, info] = await Promise.all([
    readable(run, "branch rules", () =>
      github.branchRules(repository, branch, token),
    ),
    readable(run, "branch protection", () =>
      github.branchProtection(repository, branch, token),
    ),
  ]);
  const classic = info?.protection?.required_status_checks;
  const classicChecks =
    (classic?.contexts?.length ?? 0) + (classic?.checks?.length ?? 0);
  return {
    checks:
      (rules ?? []).filter((r) => CHECK_RULES.includes(r.type)).length +
      classicChecks,
    strict:
      rules === undefined ||
      info === undefined ||
      rules.some(
        (r) =>
          r.type === "required_status_checks" &&
          r.parameters?.strict_required_status_checks_policy,
      ) ||
      (classicChecks > 0 &&
        classic?.enforcement_level !== "off" &&
        classic?.strict !== false),
  };
}

async function readable<T>(
  run: Run,
  what: string,
  read: () => Promise<T>,
): Promise<T | undefined> {
  return read().catch((e: unknown) => {
    if (!(e instanceof GitHubError && [403, 404].includes(e.status))) throw e;
    run.log(`${what} unreadable (${e.status}); counting no required checks`);
    return undefined;
  });
}

async function evaluate(
  context: Context,
  pr: PullRequest,
  labeler?: Labeler,
  quota?: Quota,
): Promise<void> {
  const { run, config } = context;
  const initial = gated(pr, config, labeler);
  if (initial.declined)
    run.log(`#${pr.number}: not merging, ${initial.declined}`);
  const gate = await byWriters(context, pr, initial, labeler);
  const checked = gate.merge.length > 0 ? (await context.policy()).checks : 0;
  const reason = wantsUpdate(gate)
    ? await updateReason(context, pr, quota)
    : undefined;
  for (const action of decide(pr, gate, checked, reason !== undefined)) {
    if (action.kind !== "update_branch") await perform(context, pr, action);
    else if ((await updateBranch(context, pr)) && reason === "strict" && quota)
      quota.strict--;
  }
}

interface Quota {
  strict: number;
}

type UpdateReason = "always" | "strict" | "overlap";

async function updateReason(
  { run, config, policy }: Context,
  pr: PullRequest,
  quota?: Quota,
): Promise<UpdateReason | undefined> {
  const ahead = await run.github.compare(
    run.repository,
    run.defaultBranch,
    pr.head.sha,
    run.token,
  );
  if (ahead.status !== "behind" && ahead.status !== "diverged")
    return undefined;
  if (config.update === "always") return "always";
  const skip = (why: string) => {
    run.log(`#${pr.number}: not updating, ${why}`);
    return undefined;
  };
  if ((await policy()).strict)
    return quota && quota.strict <= 0
      ? skip("another pull request is being updated first")
      : "strict";
  return (await overlaps(run, pr, ahead))
    ? "overlap"
    : skip(
        `${run.defaultBranch} does not require it and the incoming commits touch none of its files`,
      );
}

async function overlaps(
  run: Run,
  pr: PullRequest,
  ahead: Comparison,
): Promise<boolean> {
  const incoming = await run.github.compare(
    run.repository,
    pr.head.sha,
    run.defaultBranch,
    run.token,
  );
  if (truncated(ahead) || truncated(incoming)) return true;
  const mine = new Set(paths(ahead));
  return paths(incoming).some((path) => mine.has(path));
}

const truncated = ({ files }: Comparison) =>
  files === undefined || files.length >= COMPARE_FILE_CAP;

const paths = ({ files = [] }: Comparison) =>
  files.flatMap((f) =>
    f.previous_filename ? [f.filename, f.previous_filename] : [f.filename],
  );

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

async function updateBranch(
  { run }: Context,
  pr: PullRequest,
): Promise<boolean> {
  const tag = `#${pr.number}`;
  return run.github
    .updateBranch(run.repository, pr.number, pr.head.sha, run.token)
    .then(() => {
      run.log(`${tag}: updated from ${run.defaultBranch}`);
      return true;
    })
    .catch((e: unknown) => {
      if (!(e instanceof GitHubError)) throw e;
      run.log(`${tag}: could not update (${e.status}: ${e.message})`);
      return false;
    });
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
    case "skip":
      run.log(`${tag}: not merging, ${action.reason}`);
  }
}
