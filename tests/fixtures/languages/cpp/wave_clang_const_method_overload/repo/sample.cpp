struct Meter { int read(int n) { return n; } int read(int n) const { return n; } };
int run(Meter &m) { return m.read(1); }
