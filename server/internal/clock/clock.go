package clock

import "github.com/akinmoladun/edgesync/server/internal/models"

// Relation represents the causal relationship between two vector clocks.
type Relation int

const (
	Equal Relation = iota
	Dominates
	Dominated
	Concurrent
)

// Compare determines whether clock A dominates, is dominated by, equals, or is concurrent with clock B.
// A dominates B if for all k, A[k] >= B[k] and exists k where A[k] > B[k].
// B dominates A if for all k, B[k] >= A[k] and exists k where B[k] > A[k].
func Compare(a, b models.VectorClock) Relation {
	if a == nil {
		a = make(models.VectorClock)
	}
	if b == nil {
		b = make(models.VectorClock)
	}

	aGreater := false
	bGreater := false

	// Gather all keys
	allKeys := make(map[string]struct{})
	for k := range a {
		allKeys[k] = struct{}{}
	}
	for k := range b {
		allKeys[k] = struct{}{}
	}

	for k := range allKeys {
		valA := a[k]
		valB := b[k]

		if valA > valB {
			aGreater = true
		} else if valB > valA {
			bGreater = true
		}
	}

	if aGreater && !bGreater {
		return Dominates
	}
	if bGreater && !aGreater {
		return Dominated
	}
	if !aGreater && !bGreater {
		return Equal
	}
	return Concurrent
}

// Merge combines two vector clocks by taking the pairwise maximum of each counter.
func Merge(a, b models.VectorClock) models.VectorClock {
	result := make(models.VectorClock)
	for k, v := range a {
		result[k] = v
	}
	for k, v := range b {
		if existing, ok := result[k]; !ok || v > existing {
			result[k] = v
		}
	}
	return result
}
