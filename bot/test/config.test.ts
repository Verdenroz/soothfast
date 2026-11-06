import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULTS, parseBotConfig } from "../src/config.ts";

const parsed = (toml: string) => {
  const result = parseBotConfig(toml);
  assert.ok(result.ok, result.ok ? "" : result.reason);
  return result.config;
};

const rejected = (toml: string) => {
  const result = parseBotConfig(toml);
  assert.ok(!result.ok);
  return result.reason;
};

test("a document without [bot] yields the defaults with labels off", () => {
  assert.deepEqual(parsed('[site]\nname = "x"\n'), DEFAULTS);
  assert.equal(parsed("").labels, false);
});

test("labels = true opts in and keeps the other defaults", () => {
  assert.deepEqual(parsed("[bot]\nlabels = true\n"), {
    ...DEFAULTS,
    labels: true,
  });
});

test("every key can be overridden", () => {
  const config = parsed(`[bot]
labels = true
merge_labels = ["ship", 'land']
update_labels = []
ignore_labels = ["hold"] # trailing comment
merge_method = "rebase"
`);
  assert.deepEqual(config, {
    labels: true,
    mergeLabels: ["ship", "land"],
    updateLabels: [],
    ignoreLabels: ["hold"],
    mergeMethod: "rebase",
  });
});

test("arrays can span lines and carry escapes", () => {
  const config = parsed(`[bot]
merge_labels = [
  "a\\"b",  # note
  "c,d",
]
`);
  assert.deepEqual(config.mergeLabels, ['a"b', "c,d"]);
});

test("other tables are skipped, including a fake [bot] in a string", () => {
  const config = parsed(`[site]
about = """
[bot]
labels = true
"""
raw = '''
[bot]
merge_method = "bogus"
'''
nav = [
  "[bot]",
  ["x"],
]

[bot]
labels = true

[[other]]
labels = "not a bool"
`);
  assert.deepEqual(config, { ...DEFAULTS, labels: true });
});

test("a [bot] header inside a comment is ignored", () => {
  assert.equal(parsed("# [bot]\n[site]\nlabels = 1\n").labels, false);
});

test("subtables and array tables named bot are not the bot table", () => {
  assert.equal(parsed("[bot.extra]\nlabels = true\n").labels, false);
  assert.equal(parsed("[[bot]]\nlabels = true\n").labels, false);
});

const malformed = [
  ["unknown key", "[bot]\nmerge_label = []\n", /unknown keys: merge_label/],
  ["non-boolean labels", '[bot]\nlabels = "yes"\n', /labels must be/],
  ["scalar label list", '[bot]\nmerge_labels = "a"\n', /array of strings/],
  ["non-string item", "[bot]\nmerge_labels = [1]\n", /unreadable/],
  ["unterminated array", '[bot]\nmerge_labels = ["a"\n', /unreadable/],
  [
    "bad merge method",
    '[bot]\nmerge_method = "ff"\n',
    /squash, merge or rebase/,
  ],
  ["repeated key", "[bot]\nlabels = true\nlabels = false\n", /repeats labels/],
  ["repeated table", "[bot]\n[site]\n[bot]\n", /defined twice/],
  ["line without =", "[bot]\nlabels\n", /invalid line/],
] as const;

for (const [name, toml, reason] of malformed) {
  test(`malformed [bot] is rejected: ${name}`, () => {
    assert.match(rejected(toml), reason);
  });
}

test("a root-level dotted key for bot is rejected", () => {
  assert.match(rejected("bot.labels = true\n"), /table header/);
});

test("a root-level inline table for bot is rejected", () => {
  assert.match(rejected("bot = { labels = true }\n"), /table header/);
});
