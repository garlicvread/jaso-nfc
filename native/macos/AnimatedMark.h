#import <AppKit/AppKit.h>
#import <QuartzCore/QuartzCore.h>
FOUNDATION_EXPORT const CGFloat JasoMarkWidth;
FOUNDATION_EXPORT const CGPoint JasoMarkSeparatedPositions[4];
FOUNDATION_EXPORT CGPathRef JasoMarkCreatePath(NSUInteger piece, BOOL joined) CF_RETURNS_RETAINED;
FOUNDATION_EXPORT CGPathRef JasoMarkCreateInterpolatedPath(NSUInteger piece, CGFloat progress) CF_RETURNS_RETAINED;
FOUNDATION_EXPORT CGPoint JasoMarkPosition(NSUInteger piece, CGFloat progress);
FOUNDATION_EXPORT BOOL JasoMarkStyleAvailable(NSInteger style);
FOUNDATION_EXPORT void JasoMarkSetStyle(NSInteger style);
FOUNDATION_EXPORT CGFloat JasoMarkProgressAtCycleFraction(CGFloat fraction);
FOUNDATION_EXPORT CFTimeInterval JasoMarkCycleDuration(void);
@interface JasoAnimatedMark : NSView
@property(nonatomic) BOOL animationEnabled;
- (void)refreshAnimation;
@end
