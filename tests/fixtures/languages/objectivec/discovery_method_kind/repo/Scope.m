__attribute__((objc_root_class))
@interface Scope
@end
@implementation Scope
+ (void)mark:(int)value __attribute__((objc_direct)) {}
- (void)mark:(int)value __attribute__((objc_direct)) {}

- (void)fromInstance { [self mark:2]; }
@end
