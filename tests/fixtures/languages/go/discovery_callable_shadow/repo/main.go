package example
func marker() {}
func shadow(marker func()) { marker() }
func positive() { marker() }
func scoped() { if true { marker := 1; _ = marker }; marker() }
