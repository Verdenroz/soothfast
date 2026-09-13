-- bind bench harness: soothfast.stats vs. the same arithmetic in plain Lua
-- over a table. Same LCG as every other language's bench script, so the
-- ratio here is measured against identical data. os.clock is too coarse for
-- the sub-millisecond shapes, so timing goes through clock_gettime instead.

local ffi = require("ffi")
local stats = require("soothfast.stats")

ffi.cdef[[
typedef long time_t;
typedef struct timespec {
	time_t tv_sec;
	long tv_nsec;
} timespec;
int clock_gettime(int clk_id, timespec *tp);
]]

local CLOCK_MONOTONIC = 1

local N = 100000
local K = 9

local LCG_A = 6364136223846793005ULL
local LCG_C = 1442695040888963407ULL

local function samples(n)
	local x = 7ULL
	local out = {}
	for i = 1, n do
		x = x * LCG_A + LCG_C
		out[i] = tonumber(x >> 11) / 9007199254740992.0
	end
	return out
end

-- Elapsed time as sec/nsec deltas rather than an absolute nanosecond count:
-- an absolute count built from CLOCK_MONOTONIC's own epoch (system uptime)
-- can exceed 2^53 and lose precision as a Lua number.
local function median_ns(fn)
	local times = {}
	local t0, t1 = ffi.new("timespec"), ffi.new("timespec")
	for i = 1, K do
		ffi.C.clock_gettime(CLOCK_MONOTONIC, t0)
		fn()
		ffi.C.clock_gettime(CLOCK_MONOTONIC, t1)
		times[i] = tonumber(t1.tv_sec - t0.tv_sec) * 1e9 + tonumber(t1.tv_nsec - t0.tv_nsec)
	end
	table.sort(times)
	return times[(K + 1) / 2]
end

local function middle(sorted, n)
	if n % 2 == 1 then
		return sorted[(n + 1) / 2]
	end
	return (sorted[n / 2] + sorted[n / 2 + 1]) / 2
end

local function host_median_mad(values, n)
	local ordered = {}
	for i = 1, n do
		ordered[i] = values[i]
	end
	table.sort(ordered)
	local median = middle(ordered, n)
	local abs_devs = {}
	for i = 1, n do
		abs_devs[i] = math.abs(values[i] - median)
	end
	table.sort(abs_devs)
	return median, middle(abs_devs, n)
end

local function dev(value, median, mad)
	if mad == 0.0 then
		return value == median and 0.0 or math.huge
	end
	return math.abs(value - median) / mad
end

local function emit(shape, binding_ns, host_ns, n)
	io.write(string.format(
		'{"shape": "%s", "binding_ns": %.17g, "host_ns": %.17g, "n": %d}\n',
		shape, binding_ns, host_ns, n
	))
end

local values = samples(N)
local checksum = 0.0

local summary
local build_binding_ns = median_ns(function()
	summary = stats.Summary.new(values)
end)
local median, mad
local build_host_ns = median_ns(function()
	median, mad = host_median_mad(values, N)
end)
checksum = checksum + summary:median()
emit("build_summary", build_binding_ns, build_host_ns, N)

-- The fast path: a cdata `double[?]` built once, outside the timed region,
-- so the binding call crosses with no table-to-array copy.
local values_cdata = ffi.new("double[?]", N)
for i = 1, N do
	values_cdata[i - 1] = values[i]
end

local batch_binding
local batch_binding_ns = median_ns(function()
	batch_binding = summary:deviations_all(values_cdata)
end)
local batch_host = {}
local batch_host_ns = median_ns(function()
	for i = 1, N do
		batch_host[i] = dev(values[i], median, mad)
	end
end)
checksum = checksum + batch_binding[1] + batch_host[1]
emit("batch_buffer", batch_binding_ns, batch_host_ns, N)

-- The slow path: the same call over a plain table, which the module copies
-- into a scratch cdata array on the way in and back out on the way out.
local batch_binding_table
local batch_binding_table_ns = median_ns(function()
	batch_binding_table = summary:deviations_all(values)
end)
local batch_host_table = {}
local batch_host_table_ns = median_ns(function()
	for i = 1, N do
		batch_host_table[i] = dev(values[i], median, mad)
	end
end)
emit("batch_buffer_table", batch_binding_table_ns, batch_host_table_ns, N)

-- `#out` sizes the scratch cdata array `deviations_into` writes through, so
-- the table must already hold N elements before the first call.
local out_buf, host_out = {}, {}
for i = 1, N do
	out_buf[i] = 0.0
	host_out[i] = 0.0
end
local into_binding_ns = median_ns(function()
	summary:deviations_into(values, out_buf)
end)
local into_host_ns = median_ns(function()
	for i = 1, N do
		host_out[i] = dev(values[i], median, mad)
	end
end)
checksum = checksum + out_buf[1] + host_out[1]
emit("batch_into", into_binding_ns, into_host_ns, N)

local per_binding_total, per_host_total = 0.0, 0.0
local per_binding_ns = median_ns(function()
	per_binding_total = 0.0
	for i = 1, N do
		per_binding_total = per_binding_total + summary:deviations(values[i])
	end
end)
local per_host_ns = median_ns(function()
	per_host_total = 0.0
	for i = 1, N do
		per_host_total = per_host_total + dev(values[i], median, mad)
	end
end)
checksum = checksum + per_binding_total + per_host_total
emit("per_element", per_binding_ns, per_host_ns, N)

io.stderr:write(string.format("checksum: %.17g\n", checksum))
