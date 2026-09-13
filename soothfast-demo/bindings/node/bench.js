"use strict";

// bind bench harness: soothfast-stats-native vs. the same arithmetic in
// plain JavaScript over a typed array. Same LCG as every other language's
// bench script, so the ratio here is measured against identical data.

const { Summary } = require("./index");

const N = 100000;
const K = 9;

const LCG_A = 6364136223846793005n;
const LCG_C = 1442695040888963407n;
const LCG_MASK = (1n << 64n) - 1n;
const TWO_POW_53 = 9007199254740992;

function samples(n) {
  let x = 7n;
  const out = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    x = (x * LCG_A + LCG_C) & LCG_MASK;
    out[i] = Number(x >> 11n) / TWO_POW_53;
  }
  return out;
}

function medianNs(fn) {
  const times = [];
  let last;
  for (let i = 0; i < K; i++) {
    const t0 = process.hrtime.bigint();
    last = fn();
    const t1 = process.hrtime.bigint();
    times.push(Number(t1 - t0));
  }
  times.sort((a, b) => a - b);
  return [times[(K - 1) / 2], last];
}

function hostMedianMad(values) {
  const ordered = values.slice().sort();
  const n = ordered.length;
  const median = n % 2 ? ordered[(n - 1) / 2] : (ordered[n / 2 - 1] + ordered[n / 2]) / 2;
  const absDevs = Float64Array.from(values, (v) => Math.abs(v - median)).sort();
  const mad = n % 2 ? absDevs[(n - 1) / 2] : (absDevs[n / 2 - 1] + absDevs[n / 2]) / 2;
  return [median, mad];
}

function dev(value, median, mad) {
  if (mad === 0) return value === median ? 0 : Infinity;
  return Math.abs(value - median) / mad;
}

function emit(shape, bindingNs, hostNs, n) {
  console.log(JSON.stringify({ shape, binding_ns: bindingNs, host_ns: hostNs, n }));
}

const values = samples(N);
let checksum = 0;

const [buildBindingNs, s] = medianNs(() => new Summary(values));
const [buildHostNs, [median, mad]] = medianNs(() => hostMedianMad(values));
checksum += s.median;
emit("build_summary", buildBindingNs, buildHostNs, N);

const summary = new Summary(values);

const [batchBindingNs, batchBinding] = medianNs(() => summary.deviationsAll(values));
const [batchHostNs, batchHost] = medianNs(() => {
  const out = new Float64Array(N);
  for (let i = 0; i < N; i++) out[i] = dev(values[i], median, mad);
  return out;
});
checksum += batchBinding[0] + batchHost[0];
emit("batch_buffer", batchBindingNs, batchHostNs, N);

const outBuf = new Float64Array(N);
const hostOut = new Float64Array(N);
const [intoBindingNs] = medianNs(() => {
  summary.deviationsInto(values, outBuf);
  return outBuf;
});
const [intoHostNs] = medianNs(() => {
  for (let i = 0; i < N; i++) hostOut[i] = dev(values[i], median, mad);
  return hostOut;
});
checksum += outBuf[0] + hostOut[0];
emit("batch_into", intoBindingNs, intoHostNs, N);

const [perBindingNs, perBinding] = medianNs(() => {
  let total = 0;
  for (let i = 0; i < N; i++) total += summary.deviations(values[i]);
  return total;
});
const [perHostNs, perHost] = medianNs(() => {
  let total = 0;
  for (let i = 0; i < N; i++) total += dev(values[i], median, mad);
  return total;
});
checksum += perBinding + perHost;
emit("per_element", perBindingNs, perHostNs, N);

console.error(`checksum: ${checksum}`);
