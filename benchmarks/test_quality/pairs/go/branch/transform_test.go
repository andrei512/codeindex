package transform

import "testing"

func TestTransform(t *testing.T) {
    for _, value := range []int{-2, -1, 0, 1, 2, 3} {
        if value < 0 {
            if got := transform(value); got != -value {
                t.Errorf("transform(%d) = %d, want %d", value, got, -value)
            }
        } else {
            if got := transform(value); got != value * 2 {
                t.Errorf("transform(%d) = %d, want %d", value, got, value * 2)
            }
        }
    }
}
