#!/usr/bin/env Rscript

# bind bench harness: soothfast.stats vs. the same arithmetic in plain R.
# R has no native 64-bit integer, so the LCG state is four 16-bit limbs
# (little-endian) and every step multiplies and adds them by hand, mod
# 2^64 — the same generator every other language's bench script uses.
# extendr gaps `deviations_into` for R, so there is no batch_into shape.

library(soothfast.stats)

n <- 100000L
k <- 9L

lcg_a <- c(32557, 19605, 62509, 22609)
lcg_c <- c(33103, 63335, 31614, 5125)

lcg_next <- function(state) {
  products <- outer(state, lcg_a)
  limb <- numeric(4)
  carry <- 0
  for (kk in 0:3) {
    s <- carry
    for (i in 0:3) {
      j <- kk - i
      if (j >= 0 && j <= 3) s <- s + products[i + 1, j + 1]
    }
    limb[kk + 1] <- s %% 65536
    carry <- s %/% 65536
  }
  carry2 <- 0
  out <- numeric(4)
  for (kk in 1:4) {
    s <- limb[kk] + lcg_c[kk] + carry2
    out[kk] <- s %% 65536
    carry2 <- s %/% 65536
  }
  out
}

# The top 53 bits of a 64-bit limb state, read off without ever forming the
# full 64-bit value: 65536 == 2048 * 32, so state[1]'s own top bits plus
# every higher limb shifted by 5 gives exactly `state >> 11`.
lcg_uniform <- function(state) {
  (state[1] %/% 2048) + 32 * state[2] + 2097152 * state[3] + 137438953472 * state[4]
}

samples <- function(count) {
  state <- c(7, 0, 0, 0)
  out <- numeric(count)
  for (i in seq_len(count)) {
    state <- lcg_next(state)
    out[i] <- lcg_uniform(state) / 9007199254740992
  }
  out
}

median_ns <- function(fn) {
  times <- numeric(k)
  last <- NULL
  for (i in seq_len(k)) {
    t0 <- Sys.time()
    last <- fn()
    t1 <- Sys.time()
    times[i] <- as.numeric(t1 - t0, units = "secs") * 1e9
  }
  list(ns = sort(times)[(k + 1) %/% 2], last = last)
}

host_median_mad <- function(values) {
  ordered <- sort(values)
  m <- length(ordered)
  if (m %% 2 == 1) {
    median <- ordered[(m + 1) %/% 2]
  } else {
    median <- (ordered[m %/% 2] + ordered[m %/% 2 + 1]) / 2
  }
  abs_devs <- sort(abs(values - median))
  if (m %% 2 == 1) {
    mad <- abs_devs[(m + 1) %/% 2]
  } else {
    mad <- (abs_devs[m %/% 2] + abs_devs[m %/% 2 + 1]) / 2
  }
  c(median, mad)
}

dev <- function(value, median, mad) {
  if (mad == 0) {
    if (value == median) 0 else Inf
  } else {
    abs(value - median) / mad
  }
}

emit <- function(shape, binding_ns, host_ns, count) {
  cat(sprintf(
    '{"shape": "%s", "binding_ns": %.1f, "host_ns": %.1f, "n": %d}\n',
    shape, binding_ns, host_ns, count
  ))
}

values <- samples(n)
checksum <- 0

build <- median_ns(function() Summary(values))
host_build <- median_ns(function() host_median_mad(values))
summary <- build$last
stats <- host_build$last
median <- stats[1]
mad <- stats[2]
checksum <- checksum + summary$median()
emit("build_summary", build$ns, host_build$ns, n)

batch <- median_ns(function() summary$deviations_all(values))
host_batch <- median_ns(function() sapply(values, dev, median = median, mad = mad))
checksum <- checksum + batch$last[1] + host_batch$last[1]
emit("batch_buffer", batch$ns, host_batch$ns, n)

per_binding <- median_ns(function() {
  total <- 0
  for (v in values) total <- total + summary$deviations(v)
  total
})
per_host <- median_ns(function() {
  total <- 0
  for (v in values) total <- total + dev(v, median, mad)
  total
})
checksum <- checksum + per_binding$last + per_host$last
emit("per_element", per_binding$ns, per_host$ns, n)

cat(sprintf("checksum: %f\n", checksum), file = stderr())
