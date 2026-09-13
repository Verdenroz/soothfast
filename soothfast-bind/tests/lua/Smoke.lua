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
assert(digest[1] == 2 and digest[2] == 3 and digest[3] == 4, "digest")
assert(#acme.digest({}) == 0, "empty digest")

local norm = acme.normalize({ 1.0, 2.0, 3.0 }, 2.0)
assert(norm[1] == 2.0 and norm[2] == 4.0 and norm[3] == 6.0, "normalize")

assert(acme.greet("world") == "hello, world", "greet")

-- A caller already holding a matching FFI array passes it straight through;
-- writing into it needs no copy back.
local ffi = require("ffi")
local out = ffi.new("double[?]", 3)
acme.scale_into({ 1.0, 2.0, 3.0 }, 2.0, out)
assert(out[0] == 2.0 and out[1] == 4.0 and out[2] == 6.0, "scale_into cdata out")

-- A plain table is copied in and the mutation copied back out.
local out_table = { 0, 0, 0 }
acme.scale_into({ 1.0, 2.0, 3.0 }, 3.0, out_table)
assert(out_table[1] == 3.0 and out_table[2] == 6.0 and out_table[3] == 9.0, "scale_into table out")

print("ok")
