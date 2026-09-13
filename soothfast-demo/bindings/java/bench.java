import io.soothfast.stats.Summary;

import java.util.Arrays;

// bind bench harness: soothfast-stats vs. the same arithmetic in plain
// Java. Same LCG as every other language's bench script, so the ratio here
// is measured against identical data. Lowercase class name so the filename
// (`bench.java`) matches it, which single-file source launch requires.
public class bench {
    static final int N = 100_000;
    static final int K = 9;

    static double[] samples(int n) {
        long x = 7L;
        double[] out = new double[n];
        for (int i = 0; i < n; i++) {
            x = x * 6364136223846793005L + 1442695040888963407L;
            out[i] = (x >>> 11) / (double) (1L << 53);
        }
        return out;
    }

    interface Op {
        void run();
    }

    static long medianNs(Op op) {
        long[] times = new long[K];
        for (int i = 0; i < K; i++) {
            long t0 = System.nanoTime();
            op.run();
            times[i] = System.nanoTime() - t0;
        }
        Arrays.sort(times);
        return times[K / 2];
    }

    static double[] hostMedianMad(double[] values) {
        double[] ordered = values.clone();
        Arrays.sort(ordered);
        double median = middle(ordered);
        double[] absDevs = new double[values.length];
        for (int i = 0; i < values.length; i++) {
            absDevs[i] = Math.abs(values[i] - median);
        }
        Arrays.sort(absDevs);
        return new double[] { median, middle(absDevs) };
    }

    static double middle(double[] sorted) {
        int n = sorted.length;
        if (n % 2 == 1) {
            return sorted[n / 2];
        }
        return (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0;
    }

    static double dev(double value, double median, double mad) {
        if (mad == 0.0) {
            return value == median ? 0.0 : Double.POSITIVE_INFINITY;
        }
        return Math.abs(value - median) / mad;
    }

    static void emit(String shape, long bindingNs, long hostNs, int n) {
        System.out.printf(
                "{\"shape\": \"%s\", \"binding_ns\": %d, \"host_ns\": %d, \"n\": %d}%n",
                shape, bindingNs, hostNs, n);
    }

    public static void main(String[] args) {
        double[] values = samples(N);
        double checksum = 0.0;

        Summary[] summaryHolder = new Summary[1];
        long buildBindingNs = medianNs(() -> summaryHolder[0] = new Summary(values));
        double[][] statsHolder = new double[1][];
        long buildHostNs = medianNs(() -> statsHolder[0] = hostMedianMad(values));
        Summary summary = summaryHolder[0];
        double median = statsHolder[0][0];
        double mad = statsHolder[0][1];
        checksum += summary.median();
        emit("build_summary", buildBindingNs, buildHostNs, N);

        double[][] batchHolder = new double[1][];
        long batchBindingNs = medianNs(() -> batchHolder[0] = summary.deviationsAll(values));
        double[] hostBatch = new double[N];
        long batchHostNs = medianNs(() -> {
            for (int i = 0; i < N; i++) {
                hostBatch[i] = dev(values[i], median, mad);
            }
        });
        checksum += batchHolder[0][0] + hostBatch[0];
        emit("batch_buffer", batchBindingNs, batchHostNs, N);

        double[] outBuf = new double[N];
        long intoBindingNs = medianNs(() -> summary.deviationsInto(values, outBuf));
        double[] hostOut = new double[N];
        long intoHostNs = medianNs(() -> {
            for (int i = 0; i < N; i++) {
                hostOut[i] = dev(values[i], median, mad);
            }
        });
        checksum += outBuf[0] + hostOut[0];
        emit("batch_into", intoBindingNs, intoHostNs, N);

        double[] totalHolder = new double[1];
        long perBindingNs = medianNs(() -> {
            double total = 0.0;
            for (double v : values) {
                total += summary.deviations(v);
            }
            totalHolder[0] = total;
        });
        checksum += totalHolder[0];
        double[] hostTotalHolder = new double[1];
        long perHostNs = medianNs(() -> {
            double total = 0.0;
            for (double v : values) {
                total += dev(v, median, mad);
            }
            hostTotalHolder[0] = total;
        });
        checksum += hostTotalHolder[0];
        emit("per_element", perBindingNs, perHostNs, N);

        summary.close();
        System.err.println("checksum: " + checksum);
    }
}
