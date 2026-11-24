#include <stdint.h>
#include <limits.h>
#if -1 < 1U
int bad_unsigned(void) { return 0; }
#else
int promoted_unsigned(void) { return 1; }
#endif
#if 'A' == 65 && CHAR_BIT == 8 && INT_MAX > 0 && UINT8_MAX == 255
int native_limits(void) { return 1; }
#endif
#if -1 < UINT8_MAX && -1 < UCHAR_MAX
int small_promotions(void) { return 1; }
#endif
#if CHAR_MIN == 0
int unsigned_char(void) { return 1; }
#endif
