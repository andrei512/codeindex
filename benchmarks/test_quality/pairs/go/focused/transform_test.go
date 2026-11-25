package transform

import "testing"

func TestNegative(t *testing.T) {
    for _, c := range [][2]int{{-2, 2}, {-1, 1}} {
        if got := transform(c[0]); got != c[1] {
            t.Errorf("transform(%d) = %d, want %d", c[0], got, c[1])
        }
    }
}

func TestNonnegative(t *testing.T) {
    for _, c := range [][2]int{{0, 0}, {1, 2}, {2, 4}, {3, 6}} {
        if got := transform(c[0]); got != c[1] {
            t.Errorf("transform(%d) = %d, want %d", c[0], got, c[1])
        }
    }
}
