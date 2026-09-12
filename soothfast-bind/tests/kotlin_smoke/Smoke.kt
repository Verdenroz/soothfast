import acme.core.Core
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

    println("digest=${Core.digest(byteArrayOf(1, 2, 3)).contentToString()}")
    println("normalize=${Core.normalize(doubleArrayOf(1.0, 2.0, 3.0), 2.0).contentToString()}")
    println("stamp=${Core.stamp(7, 3.0, byteArrayOf(9, 9))}")
}
