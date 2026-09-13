local acme = require("acme.core")

local counter = acme.Counter.new(10)
assert(counter:value() == 10, "value")
assert(counter:at("low") == 10, "at low")
assert(counter:at("high") == 20, "at high")
assert(counter:bump(5) == 15, "bump")
assert(counter:bump_all({ 1, 2, 3 }) == 16, "bump_all")

local ok, err = pcall(function()
	return acme.Counter.new(9223372036854775807LL):bump(1)
end)
assert(not ok, "expected an error past MaxInt64")
assert(tostring(err):find("overflow"), "error message: " .. tostring(err))

counter:close()
counter:close()

local digest = acme.digest({ 1, 2, 3 })
assert(#digest == 3, "digest length")
assert(digest[1] == 2 and digest[2] == 3 and digest[3] == 4, "digest")
assert(#acme.digest({}) == 0, "empty digest")

local digest_table = digest:totable()
assert(digest_table[1] == 2 and digest_table[2] == 3 and digest_table[3] == 4, "totable")

for _, bad in ipairs({ 0, -1, 4 }) do
	local ok, err = pcall(function()
		return digest[bad]
	end)
	assert(not ok, "expected an error at index " .. bad)
	assert(tostring(err):find("out of range"), "error message: " .. tostring(err))
end

local norm = acme.normalize({ 1.0, 2.0, 3.0 }, 2.0)
assert(norm[1] == 2.0 and norm[2] == 4.0 and norm[3] == 6.0, "normalize")

-- A returned array passes back into another call with no copy.
local renorm = acme.normalize(norm, 1.0)
assert(renorm[1] == 2.0 and renorm[2] == 4.0 and renorm[3] == 6.0, "normalize a returned array")

assert(acme.greet("world") == "hello, world", "greet")

-- A caller already holding a matching FFI array passes it straight through;
-- writing into it needs no copy back.
local ffi = require("ffi")
local out = ffi.new("double[?]", 3)
acme.scale_into({ 1.0, 2.0, 3.0 }, 2.0, out)
assert(out[0] == 2.0 and out[1] == 4.0 and out[2] == 6.0, "scale_into cdata out")

-- A returned array works as a scale_into input too.
local out_from_norm = ffi.new("double[?]", 3)
acme.scale_into(norm, 3.0, out_from_norm)
assert(
	out_from_norm[0] == 6.0 and out_from_norm[1] == 12.0 and out_from_norm[2] == 18.0,
	"scale_into from a returned array"
)

-- A plain table is copied in and the mutation copied back out.
local out_table = { 0, 0, 0 }
acme.scale_into({ 1.0, 2.0, 3.0 }, 3.0, out_table)
assert(out_table[1] == 3.0 and out_table[2] == 6.0 and out_table[3] == 9.0, "scale_into table out")

assert(acme.peak_level({ 0.1, 0.9, 0.3 }) == "high", "peak_level high")
assert(acme.peak_level({ 0.1, 0.2 }) == "low", "peak_level low")

local found = acme.find_counter(5)
assert(found ~= nil, "find_counter present")
assert(found:value() == 5, "find_counter value")
found:close()
assert(acme.find_counter(-1) == nil, "find_counter absent")

print("ok")
