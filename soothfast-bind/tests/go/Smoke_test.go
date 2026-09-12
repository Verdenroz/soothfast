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
}

func TestEmptyBuffer(t *testing.T) {
	if got := Digest(nil); len(got) != 0 {
		t.Fatalf("Digest(nil) = %v, want empty", got)
	}
}
