package transform

import "testing"

func TestTransform(t *testing.T) {
    cases := [][2]int{
            {-2, 2},
            {-1, 1},
            {0, 0},
            {1, 2},
            {2, 4},
            {3, 6},
        }
    for _, c := range cases {
        if got := transform(c[0]); got != c[1] {
            t.Errorf("transform(%d) = %d, want %d", c[0], got, c[1])
        }
    }
}
