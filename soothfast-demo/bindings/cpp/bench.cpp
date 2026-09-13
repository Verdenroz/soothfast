// bind bench harness: soothfast::stats vs. the same arithmetic in plain
// C++ over a std::vector. Same LCG as every other language's bench script,
// so the ratio here is measured against identical data. A compiled host
// calling the C ABI directly, with no marshalling layer, so its ratio is
// the floor: the cost of the wrapper itself.
#include "stats.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <iterator>
#include <limits>
#include <optional>
#include <utility>
#include <vector>

namespace {

constexpr int N = 100000;
constexpr int K = 9;
constexpr std::uint64_t LCG_A = 6364136223846793005ULL;
constexpr std::uint64_t LCG_C = 1442695040888963407ULL;

std::vector<double> samples(int n) {
    std::vector<double> out(n);
    std::uint64_t x = 7;
    for (int i = 0; i < n; ++i) {
        x = x * LCG_A + LCG_C;
        out[i] = static_cast<double>(x >> 11) / static_cast<double>(1ULL << 53);
    }
    return out;
}

template <typename Fn>
long long median_ns(Fn fn) {
    long long times[K];
    for (int i = 0; i < K; ++i) {
        auto t0 = std::chrono::steady_clock::now();
        fn();
        auto t1 = std::chrono::steady_clock::now();
        times[i] = std::chrono::duration_cast<std::chrono::nanoseconds>(t1 - t0).count();
    }
    std::sort(std::begin(times), std::end(times));
    return times[K / 2];
}

double middle(const std::vector<double> &sorted) {
    std::size_t n = sorted.size();
    if (n % 2 == 1) {
        return sorted[n / 2];
    }
    return (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0;
}

std::pair<double, double> host_median_mad(const std::vector<double> &values) {
    std::vector<double> ordered(values);
    std::sort(ordered.begin(), ordered.end());
    double median = middle(ordered);
    std::vector<double> abs_devs(values.size());
    for (std::size_t i = 0; i < values.size(); ++i) {
        abs_devs[i] = std::fabs(values[i] - median);
    }
    std::sort(abs_devs.begin(), abs_devs.end());
    return {median, middle(abs_devs)};
}

double dev(double value, double median, double mad) {
    if (mad == 0.0) {
        return value == median ? 0.0 : std::numeric_limits<double>::infinity();
    }
    return std::fabs(value - median) / mad;
}

void emit(const char *shape, double binding_ns, double host_ns, int n) {
    std::printf(
        "{\"shape\": \"%s\", \"binding_ns\": %.17g, \"host_ns\": %.17g, \"n\": %d}\n",
        shape, binding_ns, host_ns, n);
}

} // namespace

int main() {
    using soothfast::stats::Summary;

    std::vector<double> values = samples(N);
    double checksum = 0.0;

    std::optional<Summary> summary;
    long long build_binding_ns = median_ns([&] { summary.emplace(values); });
    double median = 0.0;
    double mad = 0.0;
    long long build_host_ns = median_ns([&] {
        auto result = host_median_mad(values);
        median = result.first;
        mad = result.second;
    });
    checksum += summary->median();
    emit("build_summary", build_binding_ns, build_host_ns, N);

    std::vector<double> batch_binding;
    std::vector<double> batch_host(N);
    long long batch_binding_ns = median_ns([&] { batch_binding = summary->deviations_all(values); });
    long long batch_host_ns = median_ns([&] {
        for (int i = 0; i < N; ++i) {
            batch_host[i] = dev(values[i], median, mad);
        }
    });
    checksum += batch_binding[0] + batch_host[0];
    emit("batch_buffer", batch_binding_ns, batch_host_ns, N);

    std::vector<double> out_buf(N, 0.0);
    std::vector<double> host_out(N, 0.0);
    long long into_binding_ns = median_ns([&] { summary->deviations_into(values, out_buf); });
    long long into_host_ns = median_ns([&] {
        for (int i = 0; i < N; ++i) {
            host_out[i] = dev(values[i], median, mad);
        }
    });
    checksum += out_buf[0] + host_out[0];
    emit("batch_into", into_binding_ns, into_host_ns, N);

    double per_binding_total = 0.0;
    double per_host_total = 0.0;
    long long per_binding_ns = median_ns([&] {
        per_binding_total = 0.0;
        for (double v : values) {
            per_binding_total += summary->deviations(v);
        }
    });
    long long per_host_ns = median_ns([&] {
        per_host_total = 0.0;
        for (double v : values) {
            per_host_total += dev(v, median, mad);
        }
    });
    checksum += per_binding_total + per_host_total;
    emit("per_element", per_binding_ns, per_host_ns, N);

    std::fprintf(stderr, "checksum: %.17g\n", checksum);
    return 0;
}
