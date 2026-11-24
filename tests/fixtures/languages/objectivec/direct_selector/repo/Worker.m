__attribute__((objc_root_class))
@interface Worker
@end
@implementation Worker
- (void)emit __attribute__((objc_direct)) {}
- (void)flush __attribute__((objc_direct)) {}
- (void)run {
    [self emit];
}
@end
__attribute__((objc_root_class))
@interface Other
@end
@implementation Other
- (void)emit __attribute__((objc_direct)) {}
@end
