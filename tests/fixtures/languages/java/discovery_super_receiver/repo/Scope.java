class Parent {
    void emit() {}
}
final class Scope extends Parent {
    @Override void emit() {}
    void invokeParent() { super.emit(); }
    void invokeChild() { this.emit(); }
}
