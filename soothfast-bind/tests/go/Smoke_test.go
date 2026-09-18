package core

import (
	"math"
	"testing"
)

func TestCounterLifecycle(t *testing.T) {
	c := NewCounter(10)
	defer c.Close()

	if got := c.Value(); got != 10 {
		t.Fatalf("Value() = %d, want 10", got)
	}
	if got := c.At(LevelHigh); got != 20 {
		t.Fatalf("At(LevelHigh) = %d, want 20", got)
	}
	got, err := c.Bump(5)
	if err != nil {
		t.Fatalf("Bump(5) returned error: %v", err)
	}
	if got != 15 {
		t.Fatalf("Bump(5) = %d, want 15", got)
	}
	if got := c.BumpAll([]int64{1, 2, 3}); got != 16 {
		t.Fatalf("BumpAll = %d, want 16", got)
	}
}

func TestBumpErrorPath(t *testing.T) {
	c := NewCounter(math.MaxInt64)
	defer c.Close()

	if _, err := c.Bump(1); err == nil {
		t.Fatal("Bump(1) past MaxInt64 did not return an error")
	}
}

func TestCloseIsIdempotent(t *testing.T) {
	c := NewCounter(1)
	if err := c.Close(); err != nil {
		t.Fatalf("Close() returned error: %v", err)
	}
	if err := c.Close(); err != nil {
		t.Fatalf("second Close() returned error: %v", err)
	}
}

func TestFreeFunctions(t *testing.T) {
	got := Digest([]byte{1, 2, 3})
	want := []byte{2, 3, 4}
	if len(got) != len(want) {
		t.Fatalf("Digest length = %d, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("Digest = %v, want %v", got, want)
		}
	}

	out := Normalize([]float64{1, 2, 3}, 2)
	if len(out) != 3 || out[0] != 2 || out[1] != 4 || out[2] != 6 {
		t.Fatalf("Normalize = %v, want [2 4 6]", out)
	}

	if got := Stamp(1, 2, []byte{1, 2, 3}); got != 6 {
		t.Fatalf("Stamp = %d, want 6", got)
	}

	scaled := make([]float64, 3)
	ScaleInto([]float64{1, 2, 3}, 2, scaled)
	if scaled[0] != 2 || scaled[1] != 4 || scaled[2] != 6 {
		t.Fatalf("ScaleInto = %v, want [2 4 6]", scaled)
	}
}

func TestEmptyBuffer(t *testing.T) {
	if got := Digest(nil); len(got) != 0 {
		t.Fatalf("Digest(nil) = %v, want empty", got)
	}
}

func TestPeakLevelAndFindCounter(t *testing.T) {
	if got := PeakLevel([]float64{0.1, 0.9, 0.3}); got != LevelHigh {
		t.Fatalf("PeakLevel = %v, want LevelHigh", got)
	}
	if got := PeakLevel([]float64{0.1, 0.2}); got != LevelLow {
		t.Fatalf("PeakLevel = %v, want LevelLow", got)
	}

	found := FindCounter(5)
	if found == nil {
		t.Fatal("FindCounter(5) = nil, want a counter")
	}
	defer found.Close()
	if got := found.Value(); got != 5 {
		t.Fatalf("FindCounter(5).Value() = %d, want 5", got)
	}

	if got := FindCounter(-1); got != nil {
		t.Fatal("FindCounter(-1) = non-nil, want nil")
	}
}

func TestDescribe(t *testing.T) {
	label := "world"
	if got := Describe(&label); got == nil || *got != "label=world" {
		t.Fatalf("Describe(&label) = %v, want label=world", got)
	}
	if got := Describe(nil); got != nil {
		t.Fatalf("Describe(nil) = %v, want nil", got)
	}
}

func TestDescribeOwned(t *testing.T) {
	label := "world"
	if got := DescribeOwned(&label); got == nil || *got != "owned=world" {
		t.Fatalf("DescribeOwned(&label) = %v, want owned=world", got)
	}
	if got := DescribeOwned(nil); got != nil {
		t.Fatalf("DescribeOwned(nil) = %v, want nil", got)
	}
}
