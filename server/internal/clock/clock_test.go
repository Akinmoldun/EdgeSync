package clock

import (
	"testing"

	"github.com/akinmoladun/edgesync/server/internal/models"
)

func TestVectorClockCompare(t *testing.T) {
	tests := []struct {
		name     string
		a        models.VectorClock
		b        models.VectorClock
		expected Relation
	}{
		{
			name:     "both empty are equal",
			a:        models.VectorClock{},
			b:        models.VectorClock{},
			expected: Equal,
		},
		{
			name:     "a dominates b single key",
			a:        models.VectorClock{"dev1": 2},
			b:        models.VectorClock{"dev1": 1},
			expected: Dominates,
		},
		{
			name:     "b dominates a single key",
			a:        models.VectorClock{"dev1": 1},
			b:        models.VectorClock{"dev1": 2},
			expected: Dominated,
		},
		{
			name:     "a dominates b with additional key",
			a:        models.VectorClock{"dev1": 1, "dev2": 1},
			b:        models.VectorClock{"dev1": 1},
			expected: Dominates,
		},
		{
			name:     "b dominates a with additional key",
			a:        models.VectorClock{"dev1": 1},
			b:        models.VectorClock{"dev1": 1, "dev2": 1},
			expected: Dominated,
		},
		{
			name:     "concurrent updates",
			a:        models.VectorClock{"dev1": 2, "dev2": 1},
			b:        models.VectorClock{"dev1": 1, "dev2": 2},
			expected: Concurrent,
		},
		{
			name:     "concurrent disjoint keys",
			a:        models.VectorClock{"dev1": 1},
			b:        models.VectorClock{"dev2": 1},
			expected: Concurrent,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			rel := Compare(tc.a, tc.b)
			if rel != tc.expected {
				t.Fatalf("expected relation %v, got %v for clocks a=%v, b=%v", tc.expected, rel, tc.a, tc.b)
			}
		})
	}
}

func TestVectorClockMerge(t *testing.T) {
	a := models.VectorClock{"dev1": 3, "dev2": 1}
	b := models.VectorClock{"dev1": 2, "dev2": 4, "dev3": 5}

	merged := Merge(a, b)

	if merged["dev1"] != 3 {
		t.Fatalf("expected dev1=3, got %d", merged["dev1"])
	}
	if merged["dev2"] != 4 {
		t.Fatalf("expected dev2=4, got %d", merged["dev2"])
	}
	if merged["dev3"] != 5 {
		t.Fatalf("expected dev3=5, got %d", merged["dev3"])
	}
}
