#import <AppKit/AppKit.h>

FOUNDATION_EXPORT NSNotificationName const JasoContentZoomDidChangeNotification;
FOUNDATION_EXPORT CGFloat JasoContentZoom(void);
FOUNDATION_EXPORT void JasoSetContentZoom(CGFloat scale);
FOUNDATION_EXPORT void JasoApplyContentZoom(NSView *view);
FOUNDATION_EXPORT BOOL JasoValidateContentZoomAction(NSWindow *window, SEL action);
