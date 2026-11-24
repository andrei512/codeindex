class Scope {
    static void marker() {}
    static void shadow(System.Action action) { marker(); }
    static void positive() { marker(); }
}
