#import <AppKit/AppKit.h>

// The view owns no worker process and polls only while its window is visible.
@interface JasoStatusWindowController : NSWindowController <NSWindowDelegate>
@property (copy) void (^refreshHandler)(void);
@property (copy) void (^actionHandler)(NSString *action);
@property (copy) void (^pathHandler)(NSString *path);
@property (copy) void (^diagnosticsHandler)(void);
@property (copy) void (^closeHandler)(void);
@property (readonly) BOOL refreshingAutomatically;
@property (readonly) CGFloat contentZoom;
- (void)zoomIn:(id)sender;
- (void)zoomOut:(id)sender;
- (void)resetZoom:(id)sender;
- (void)reloadLocalization;
- (void)updateSnapshot:(NSDictionary *)snapshot error:(NSString *)error updatedAt:(NSDate *)date;
- (void)setRefreshing:(BOOL)refreshing;
@end
