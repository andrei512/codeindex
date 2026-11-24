int save(const char *) { return 1; }
int save(int) { return 2; }
int run() { return save(10); }
