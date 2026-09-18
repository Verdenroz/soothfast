#include "core.hpp"

#include <cstdint>
#include <cstdio>
#include <limits>
#include <vector>

using namespace acme::core;

int main() {
    Counter c(10);
    std::printf("value=%lld\n", static_cast<long long>(c.value()));
    std::printf("at(Low)=%lld\n", static_cast<long long>(c.at(Level::Low)));
    std::printf("at(High)=%lld\n", static_cast<long long>(c.at(Level::High)));
    std::printf("bump=%lld\n", static_cast<long long>(c.bump(5)));

    std::vector<int64_t> by{1, 2, 3};
    std::printf("bumpAll=%lld\n", static_cast<long long>(c.bump_all(by)));

    Counter overflowing(std::numeric_limits<int64_t>::max());
    try {
        overflowing.bump(1);
        std::printf("no exception thrown\n");
    } catch (const Error &e) {
        std::printf("caught: %s\n", e.what());
    }

    std::vector<uint8_t> data{1, 2, 3};
    auto digested = digest(data);
    std::printf("digest=[%d, %d, %d]\n", digested[0], digested[1], digested[2]);

    std::vector<double> input{1.0, 2.0, 3.0};
    auto normalized = normalize(input, 2.0);
    std::printf("normalize=[%g, %g, %g]\n", normalized[0], normalized[1], normalized[2]);

    std::printf("greet=%s\n", greet("C++").c_str());

    std::vector<double> out(3);
    scale_into(input, 2.0, out);
    std::printf("scale_into=[%g, %g, %g]\n", out[0], out[1], out[2]);

    std::printf("stamp=%llu\n", static_cast<unsigned long long>(stamp(1, 2.0, data)));

    std::vector<double> peaked{0.1, 0.9, 0.3};
    std::printf("peak_level=%s\n", peak_level(peaked) == Level::High ? "High" : "Low");

    auto found = find_counter(5);
    std::printf("find_counter=%lld\n", static_cast<long long>(found->value()));
    std::printf("find_counter(missing)=%s\n", find_counter(-1).has_value() ? "present" : "none");

    std::printf("describe=%s\n", describe("world").value().c_str());
    std::printf("describe(none)=%s\n", describe(std::nullopt).has_value() ? "present" : "none");

    std::printf("describe_owned=%s\n", describe_owned("world").value().c_str());
    std::printf("describe_owned(none)=%s\n", describe_owned(std::nullopt).has_value() ? "present" : "none");

    try {
        fail("bad byte");
        std::printf("fail: no exception thrown\n");
    } catch (const Error &e) {
        std::printf("fail caught: %s\n", e.what());
    }

    std::vector<double> src{1.0, 2.0, 3.0};
    std::vector<double> lo(3);
    std::vector<double> hi(3);
    split(src, lo, hi);
    std::printf("split.lo=[%g, %g, %g]\n", lo[0], lo[1], lo[2]);
    std::printf("split.hi=[%g, %g, %g]\n", hi[0], hi[1], hi[2]);

    return 0;
}
