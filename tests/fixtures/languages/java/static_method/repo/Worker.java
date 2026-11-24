final class Worker {
    static void emit() {}
    static void flush() {}
    static void run() {
        emit();
    }
}
final class Other {
    static void emit() {}
}
