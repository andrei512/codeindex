#define checkreturn __attribute__((warn_unused_result))
static int checkreturn marked(void) { return 1; }
