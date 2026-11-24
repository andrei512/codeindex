#define UNRELATED 1
#define FEATURE 1
#if 0
#include "inactive-and-missing.h"
#endif
#if __has_attribute(availability)
#include "override.h"
#endif
#if FEATURE
int unproved_feature(void) { return 1; }
#endif
#if UNRELATED
int unaffected_feature(void) { return 1; }
#endif
int unconditional_site(void) { return 1; }
