// linkme registrations reach this binary only if the linker keeps the
// library, which it does only when something names it.
use soothfast_demo as _;

soothfast::bench_main!();
