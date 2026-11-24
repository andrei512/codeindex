package work

// emit() is mentioned in prose.
func emit() {}
func flush() {}
func Run() { emit() }

type other struct{}
func (other) emit() {}
