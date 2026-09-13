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
