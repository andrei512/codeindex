fun marker() {}
fun shadow(action: () -> Unit) { marker() }
fun positive() { marker() }
fun scoped() { if (true) { val marker = 1 }; marker() }
