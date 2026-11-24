struct Meter { Meter(bool) {} int value() const { return 1; } };
int run() { Meter m{true}; return m.value(); }
