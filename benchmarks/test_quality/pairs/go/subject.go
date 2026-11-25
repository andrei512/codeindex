package transform

import (
    "fmt"
    "os"
    "strconv"
)

func transform(value int) int {
    fmt.Printf("CASE=%d\n", value)
    result := value * 2
    if value < 0 {
        result = -value
    }
    if os.Getenv("CX_PAIR_MUTANT") == strconv.Itoa(value) {
        result++
    }
    return result
}
