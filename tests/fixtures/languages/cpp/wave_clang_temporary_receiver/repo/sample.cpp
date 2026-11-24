struct Meter { int value() const { return 1; } };
int run() { return Meter().value(); }
