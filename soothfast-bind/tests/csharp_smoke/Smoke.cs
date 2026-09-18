using Acme.Core;

var counter = new Counter(10);
Console.WriteLine($"value={counter.Value}");
Console.WriteLine($"bump={counter.Bump(5)}");
Console.WriteLine($"at(Low)={counter.At(Level.Low)}");
Console.WriteLine($"at(High)={counter.At(Level.High)}");
Console.WriteLine($"bumpAll={counter.BumpAll(new long[] { 1, 2, 3 })}");
try
{
    counter.Bump(long.MaxValue);
    throw new Exception("expected CoreException");
}
catch (CoreException e)
{
    Console.WriteLine($"caught: {e.Message}");
}
counter.Dispose();

Console.WriteLine($"digest=[{string.Join(", ", Core.Digest(new byte[] { 1, 2, 3 }))}]");
Console.WriteLine($"normalize=[{string.Join(", ", Core.Normalize(new double[] { 1.0, 2.0, 3.0 }, 2.0))}]");
Console.WriteLine($"stamp={Core.Stamp(7, 3.0, new byte[] { 9, 9 })}");

var scaled = new double[] { 0, 0, 0 };
Core.ScaleInto(new double[] { 1.0, 2.0, 3.0 }, 2.0, scaled);
Console.WriteLine($"scaleInto=[{string.Join(", ", scaled)}]");

Console.WriteLine($"peakLevel={Core.PeakLevel(new double[] { 0.1, 0.9, 0.3 })}");
var found = Core.FindCounter(5);
Console.WriteLine($"findCounter={found.Value}");
Console.WriteLine($"findCounter(missing)={Core.FindCounter(-1)?.Value.ToString() ?? "null"}");

Console.WriteLine($"describe={Core.Describe("world")}");
Console.WriteLine($"describe(none)={Core.Describe(null) ?? "null"}");

Console.WriteLine($"describeOwned={Core.DescribeOwned("world")}");
Console.WriteLine($"describeOwned(none)={Core.DescribeOwned(null) ?? "null"}");

try
{
    Core.Fail("bad byte");
    throw new Exception("expected CoreException");
}
catch (CoreException e)
{
    Console.WriteLine($"fail caught: {e.Message}");
}

var lo = new double[3];
var hi = new double[3];
Core.Split(new double[] { 1.0, 2.0, 3.0 }, lo, hi);
Console.WriteLine($"split.lo=[{string.Join(", ", lo)}]");
Console.WriteLine($"split.hi=[{string.Join(", ", hi)}]");

var closedCounter = new Counter(1);
closedCounter.Dispose();
try
{
    _ = closedCounter.Value;
    throw new Exception("expected ObjectDisposedException");
}
catch (ObjectDisposedException e)
{
    Console.WriteLine($"use after close: {e.ObjectName}");
}
