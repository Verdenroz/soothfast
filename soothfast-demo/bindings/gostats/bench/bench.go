// bind bench harness: gostats vs. the same arithmetic in plain Go. Same LCG
// as every other language's bench script, so the ratio here is measured
// against identical data. Lives outside the gostats package itself since a
// directory can hold only one package and gostats.go isn't `package main`.
package main

import (
	"encoding/json"
	"fmt"
	"math"
	"os"
	"sort"
	"time"

	gostats "github.com/Verdenroz/soothfast/soothfast-demo/bindings/gostats"
)

const (
	n = 100000
	k = 9
)

func samples(n int) []float64 {
	var x uint64 = 7
	out := make([]float64, n)
	for i := 0; i < n; i++ {
		x = x*6364136223846793005 + 1442695040888963407
		out[i] = float64(x>>11) / float64(uint64(1)<<53)
	}
	return out
}

func medianNs(fn func()) int64 {
	times := make([]int64, k)
	for i := 0; i < k; i++ {
		t0 := time.Now()
		fn()
		times[i] = time.Since(t0).Nanoseconds()
	}
	sort.Slice(times, func(a, b int) bool { return times[a] < times[b] })
	return times[k/2]
}

func hostMedianMad(values []float64) (float64, float64) {
	ordered := append([]float64(nil), values...)
	sort.Float64s(ordered)
	median := middle(ordered)
	absDevs := make([]float64, len(values))
	for i, v := range values {
		absDevs[i] = math.Abs(v - median)
	}
	sort.Float64s(absDevs)
	return median, middle(absDevs)
}

// middle returns a sorted slice's median.
func middle(sorted []float64) float64 {
	n := len(sorted)
	if n%2 == 1 {
		return sorted[n/2]
	}
	return (sorted[n/2-1] + sorted[n/2]) / 2
}

func dev(value, median, mad float64) float64 {
	if mad == 0 {
		if value == median {
			return 0
		}
		return math.Inf(1)
	}
	return math.Abs(value-median) / mad
}

type record struct {
	Shape     string `json:"shape"`
	BindingNs int64  `json:"binding_ns"`
	HostNs    int64  `json:"host_ns"`
	N         int    `json:"n"`
}

func emit(shape string, bindingNs, hostNs int64, n int) {
	b, err := json.Marshal(record{shape, bindingNs, hostNs, n})
	if err != nil {
		panic(err)
	}
	fmt.Println(string(b))
}

func main() {
	values := samples(n)
	checksum := 0.0

	var s *gostats.Summary
	buildBindingNs := medianNs(func() {
		built, err := gostats.NewSummary(values)
		if err != nil {
			panic(err)
		}
		s = built
	})
	var median, mad float64
	buildHostNs := medianNs(func() {
		median, mad = hostMedianMad(values)
	})
	checksum += s.Median()
	emit("build_summary", buildBindingNs, buildHostNs, n)

	var batchResult []float64
	batchBindingNs := medianNs(func() {
		batchResult = s.DeviationsAll(values)
	})
	hostBatch := make([]float64, n)
	batchHostNs := medianNs(func() {
		for i, v := range values {
			hostBatch[i] = dev(v, median, mad)
		}
	})
	checksum += batchResult[0] + hostBatch[0]
	emit("batch_buffer", batchBindingNs, batchHostNs, n)

	outBuf := make([]float64, n)
	intoBindingNs := medianNs(func() {
		s.DeviationsInto(values, outBuf)
	})
	hostOut := make([]float64, n)
	intoHostNs := medianNs(func() {
		for i, v := range values {
			hostOut[i] = dev(v, median, mad)
		}
	})
	checksum += outBuf[0] + hostOut[0]
	emit("batch_into", intoBindingNs, intoHostNs, n)

	var total float64
	perBindingNs := medianNs(func() {
		total = 0
		for _, v := range values {
			total += s.Deviations(v)
		}
	})
	checksum += total
	var hostTotal float64
	perHostNs := medianNs(func() {
		hostTotal = 0
		for _, v := range values {
			hostTotal += dev(v, median, mad)
		}
	})
	checksum += hostTotal
	emit("per_element", perBindingNs, perHostNs, n)

	fmt.Fprintf(os.Stderr, "checksum: %v\n", checksum)
}
