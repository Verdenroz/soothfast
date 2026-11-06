export type MergeMethod = "squash" | "merge" | "rebase";

export interface BotConfig {
  labels: boolean;
  mergeLabels: string[];
  updateLabels: string[];
  ignoreLabels: string[];
  mergeMethod: MergeMethod;
}

export type ConfigResult =
  { ok: true; config: BotConfig } | { ok: false; reason: string };

type Value = boolean | string | string[];

class ConfigError extends Error {}

export const DEFAULTS: BotConfig = {
  labels: false,
  mergeLabels: ["soothfast:merge"],
  updateLabels: ["soothfast:update"],
  ignoreLabels: ["do-not-merge"],
  mergeMethod: "squash",
};

// Reads only the [bot] table of soothfast.toml. Every other table is
// skipped, including strings and arrays that span lines.
export function parseBotConfig(toml: string): ConfigResult {
  try {
    return { ok: true, config: build(botEntries(statements(toml))) };
  } catch (e) {
    if (e instanceof ConfigError) return { ok: false, reason: e.message };
    throw e;
  }
}

function statements(toml: string): string[] {
  const out: string[] = [];
  let current = "";
  let depth = 0;
  for (let i = 0; i < toml.length; i++) {
    const c = toml[i];
    if (c === '"' || c === "'") {
      const end = stringEnd(toml, i);
      current += toml.slice(i, end);
      i = end - 1;
    } else if (c === "#") {
      while (i + 1 < toml.length && toml[i + 1] !== "\n") i++;
    } else if (c === "\n" && depth <= 0) {
      if (current.trim()) out.push(current.trim());
      current = "";
      depth = 0;
    } else {
      if (c === "[" || c === "{") depth++;
      if (c === "]" || c === "}") depth--;
      current += c;
    }
  }
  if (current.trim()) out.push(current.trim());
  return out;
}

function stringEnd(toml: string, start: number): number {
  const quote = toml[start];
  const long = toml.startsWith(quote.repeat(3), start);
  const delimiter = long ? quote.repeat(3) : quote;
  let i = start + delimiter.length;
  while (i < toml.length) {
    if (quote === '"' && toml[i] === "\\") i += 2;
    else if (toml.startsWith(delimiter, i)) return i + delimiter.length;
    else if (!long && toml[i] === "\n") return i;
    else i++;
  }
  return toml.length;
}

function botEntries(statementList: string[]): Map<string, Value> {
  const entries = new Map<string, Value>();
  let inBot = false;
  let seen = false;
  let root = true;
  for (const statement of statementList) {
    if (root && /^bot\s*[.=]/.test(statement))
      throw new ConfigError("write [bot] as a table header");
    if (statement.startsWith("[")) {
      root = false;
      const isBot = /^\[\s*bot\s*\]$/.test(statement);
      if (isBot && seen) throw new ConfigError("[bot] is defined twice");
      inBot = isBot;
      seen ||= isBot;
      continue;
    }
    if (!inBot) continue;
    const eq = statement.indexOf("=");
    const key = eq < 0 ? "" : statement.slice(0, eq).trim();
    if (!/^[A-Za-z0-9_-]+$/.test(key))
      throw new ConfigError(`[bot] has an invalid line: ${statement}`);
    if (entries.has(key)) throw new ConfigError(`[bot] repeats ${key}`);
    entries.set(key, parseValue(key, statement.slice(eq + 1).trim()));
  }
  return entries;
}

function build(entries: Map<string, Value>): BotConfig {
  const unknown = [...entries.keys()].filter((k) => !KEYS.includes(k));
  if (unknown.length > 0)
    throw new ConfigError(`[bot] has unknown keys: ${unknown.join(", ")}`);
  return {
    labels: flag(entries, "labels", DEFAULTS.labels),
    mergeLabels: list(entries, "merge_labels", DEFAULTS.mergeLabels),
    updateLabels: list(entries, "update_labels", DEFAULTS.updateLabels),
    ignoreLabels: list(entries, "ignore_labels", DEFAULTS.ignoreLabels),
    mergeMethod: method(entries, "merge_method", DEFAULTS.mergeMethod),
  };
}

const KEYS = [
  "labels",
  "merge_labels",
  "update_labels",
  "ignore_labels",
  "merge_method",
];

function flag(entries: Map<string, Value>, key: string, fallback: boolean) {
  const value = entries.get(key) ?? fallback;
  if (typeof value !== "boolean")
    throw new ConfigError(`[bot] ${key} must be true or false`);
  return value;
}

function list(entries: Map<string, Value>, key: string, fallback: string[]) {
  const value = entries.get(key) ?? fallback;
  if (!Array.isArray(value))
    throw new ConfigError(`[bot] ${key} must be an array of strings`);
  return value;
}

function method(
  entries: Map<string, Value>,
  key: string,
  fallback: MergeMethod,
): MergeMethod {
  const value = entries.get(key) ?? fallback;
  if (value !== "squash" && value !== "merge" && value !== "rebase")
    throw new ConfigError(`[bot] ${key} must be squash, merge or rebase`);
  return value;
}

function parseValue(key: string, raw: string): Value {
  if (raw === "true") return true;
  if (raw === "false") return false;
  const bad = () => new ConfigError(`[bot] ${key} has an unreadable value`);
  if (raw.startsWith("[")) {
    if (!raw.endsWith("]")) throw bad();
    return arrayItems(raw.slice(1, -1), bad);
  }
  const scalar = quoted(raw);
  if (scalar === undefined) throw bad();
  return scalar;
}

function arrayItems(inner: string, bad: () => ConfigError): string[] {
  const items: string[] = [];
  let rest = inner.trim();
  while (rest) {
    const match = /^("(?:[^"\\]|\\.)*"|'[^']*')\s*(,|$)/.exec(rest);
    if (!match) throw bad();
    const item = quoted(match[1]);
    if (item === undefined) throw bad();
    items.push(item);
    rest = rest.slice(match[0].length).trim();
  }
  return items;
}

function quoted(raw: string): string | undefined {
  if (/^'[^']*'$/.test(raw)) return raw.slice(1, -1);
  if (!/^"(?:[^"\\]|\\.)*"$/.test(raw)) return undefined;
  try {
    return JSON.parse(raw);
  } catch {
    return undefined;
  }
}
