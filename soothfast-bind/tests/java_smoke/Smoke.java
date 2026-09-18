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

        double[] scaled = new double[] {0, 0, 0};
        Core.scaleInto(new double[] {1.0, 2.0, 3.0}, 2.0, scaled);
        System.out.println("scaleInto=" + Arrays.toString(scaled));

        System.out.println("peakLevel=" + Core.peakLevel(new double[] {0.1, 0.9, 0.3}));
        try (Counter found = Core.findCounter(5)) {
            System.out.println("findCounter=" + found.value());
        }
        System.out.println("findCounter(missing)=" + Core.findCounter(-1));

        System.out.println("describe=" + Core.describe("world"));
        System.out.println("describe(none)=" + Core.describe(null));

        System.out.println("describeOwned=" + Core.describeOwned("world"));
        System.out.println("describeOwned(none)=" + Core.describeOwned(null));

        double[] lo = new double[3];
        double[] hi = new double[3];
        Core.split(new double[] {1.0, 2.0, 3.0}, lo, hi);
        System.out.println("split.lo=" + Arrays.toString(lo));
        System.out.println("split.hi=" + Arrays.toString(hi));
    }
}
