import acme.core.*
import acme.core.CoreException
import acme.core.Counter
import acme.core.Level

fun main() {
    Counter(10).use { c ->
        println("value=${c.value}")
        println("bump=${c.bump(5)}")
        println("at(Low)=${c.at(Level.Low)}")
        println("at(High)=${c.at(Level.High)}")
        println("bumpAll=${c.bumpAll(longArrayOf(1, 2, 3))}")
        try {
            c.bump(Long.MAX_VALUE)
            throw AssertionError("expected CoreException")
        } catch (e: CoreException) {
            println("caught: ${e.message}")
        }
    }

    println("digest=${digest(byteArrayOf(1, 2, 3)).contentToString()}")
    println("normalize=${normalize(doubleArrayOf(1.0, 2.0, 3.0), 2.0).contentToString()}")
    println("stamp=${stamp(7, 3.0, byteArrayOf(9, 9))}")

    val scaled = doubleArrayOf(0.0, 0.0, 0.0)
    scaleInto(doubleArrayOf(1.0, 2.0, 3.0), 2.0, scaled)
    println("scaleInto=${scaled.contentToString()}")

    println("peakLevel=${peakLevel(doubleArrayOf(0.1, 0.9, 0.3))}")
    findCounter(5)!!.use { found -> println("findCounter=${found.value}") }
    println("findCounter(missing)=${findCounter(-1)}")

    println("describe=${describe("world")}")
    println("describe(none)=${describe(null)}")

    println("describeOwned=${describeOwned("world")}")
    println("describeOwned(none)=${describeOwned(null)}")

    val lo = DoubleArray(3)
    val hi = DoubleArray(3)
    split(doubleArrayOf(1.0, 2.0, 3.0), lo, hi)
    println("split.lo=${lo.contentToString()}")
    println("split.hi=${hi.contentToString()}")

    val closed = Counter(1)
    closed.close()
    try {
        closed.value
        throw AssertionError("expected IllegalStateException")
    } catch (e: IllegalStateException) {
        println("use after close: ${e.message}")
    }
}
