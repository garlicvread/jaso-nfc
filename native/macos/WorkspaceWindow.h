#import <AppKit/AppKit.h>

FOUNDATION_EXPORT NSDictionary *JasoWorkspaceStatus(NSDictionary *snapshot, NSString *error, BOOL korean);
typedef void (^JasoWorkspaceReply)(NSDictionary *result, NSString *error);
@interface JasoWorkspaceWindowController : NSWindowController <NSWindowDelegate, NSTableViewDataSource, NSTableViewDelegate, NSSearchFieldDelegate>
@property (copy) void (^refreshHandler)(void);
@property (copy) void (^actionHandler)(NSString *action);
@property (copy) void (^pathHandler)(NSString *path);
@property (copy) void (^diagnosticsHandler)(void);
@property (copy) void (^closeHandler)(void);
@property (copy) void (^requestHandler)(NSString *request, NSDictionary *parameters, JasoWorkspaceReply reply);
@property (readonly, copy) NSString *selectedSection;
@property (readonly) BOOL refreshingAutomatically;
@property (readonly) CGFloat contentZoom;
- (void)showStorage:(id)sender;
- (void)selectSection:(NSString *)section;
- (void)embedView:(NSView *)view inSection:(NSString *)section;
- (void)updateSnapshot:(NSDictionary *)snapshot error:(NSString *)error updatedAt:(NSDate *)date;
- (void)updateActivity:(NSDictionary *)response error:(NSString *)error;
- (void)updateHistory:(NSDictionary *)response error:(NSString *)error;
- (void)setRefreshing:(BOOL)refreshing;
- (void)reloadLocalization;
- (void)zoomIn:(id)sender;
- (void)zoomOut:(id)sender;
- (void)resetZoom:(id)sender;
@end
