namespace Example;
static class Demo {
    static int Parse(First text) { return 1; }
    static int Parse(Second text) { return 2; }
    static int Run(string input) { return Parse(input.AsSpan()); }
}
