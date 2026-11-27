#if SELECT
inline int choose() { return 1; }
#else
inline int choose() { return 2; }
#endif
inline int dispatch() { return choose(); }
