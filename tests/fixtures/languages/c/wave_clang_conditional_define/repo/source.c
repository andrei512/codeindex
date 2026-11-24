#define PRESENT 1
#if PRESENT
static void target(void) {}
#endif
void use(void){ target(); }
