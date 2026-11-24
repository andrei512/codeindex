class Scope {
    static void marker() {}
    static void shadow(System.Action marker) { marker(); }
    static void positive() { marker(); }
}
