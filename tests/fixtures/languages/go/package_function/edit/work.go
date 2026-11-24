package work

// emit() is mentioned in prose.
func emit() {}
func flush() {}
func Run() { flush() }

type other struct{}
func (other) emit() {}
