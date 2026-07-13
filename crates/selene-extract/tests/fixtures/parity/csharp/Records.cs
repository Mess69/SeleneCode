
namespace Fixture;

public record SimplePositional(int A);
public record WithBody(int A) { public int DoubleIt() => A * 2; }
public record class ExplicitClassRec(string Name);
public record struct ValueRec(int X);
public readonly record struct ReadonlyRec(int X, int Y);
public record DerivedRec(int A, string B) : SimplePositional(A);
public record GenericRec<T>(T Value);
public partial record PartialRec(int A);
