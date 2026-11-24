final class Worker {
    static void emit() {}
    static void flush() {}
    static void run() {
        flush();
    }
}
final class Other {
    static void emit() {}
}
