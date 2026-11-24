#include "types.h"
int OWNER value() const { return 1; }
int OTHER value() const { return 2; }
int first(Left& left) { return left.value(); }
int second(Right& right) { return right.value(); }
#undef OWNER
#define OWNER
int OWNER free_step() { return 3; }
int third() { return free_step(); }
