/* The only change is the compiler definition. */
#if ENABLED
static void before(void) {}
#else
static void after(void) {}
#endif
