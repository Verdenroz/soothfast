import acme.core.Core;
import acme.core.CoreException;
import acme.core.Counter;
import acme.core.Level;

import java.util.Arrays;

public class Smoke {
    public static void main(String[] args) {
        try (Counter c = new Counter(10)) {
            System.out.println("value=" + c.value());
            System.out.println("bump=" + c.bump(5));
            System.out.println("at(Low)=" + c.at(Level.Low));
            System.out.println("at(High)=" + c.at(Level.High));
            System.out.println("bumpAll=" + c.bumpAll(new long[] {1, 2, 3}));
            try {
                c.bump(Long.MAX_VALUE);
                throw new AssertionError("expected CoreException");
            } catch (CoreException e) {
                System.out.println("caught: " + e.getMessage());
            }
        }

        System.out.println("digest=" + Arrays.toString(Core.digest(new byte[] {1, 2, 3})));
        System.out.println(
                "normalize=" + Arrays.toString(Core.normalize(new double[] {1.0, 2.0, 3.0}, 2.0)));
        System.out.println("stamp=" + Core.stamp(7, 3.0, new byte[] {9, 9}));
    }
}
