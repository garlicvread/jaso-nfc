#import <AppKit/AppKit.h>

@interface JasoSettingsWindowController : NSWindowController <NSWindowDelegate>
@property (copy) void (^languageChangedHandler)(void);
@property (copy) void (^appearanceChangedHandler)(void);
@property (copy) void (^startupChangedHandler)(BOOL enabled);
@property (copy) void (^storageHandler)(void);
@property (nonatomic, copy) NSArray<NSNumber *> *availableStyles;
@property (copy) void (^actionHandler)(NSString *action);
@property (copy) void (^closeHandler)(void);
@property (readonly) CGFloat contentZoom;
- (NSView *)embeddedContentViewForWindow:(NSWindow *)host;
- (void)zoomIn:(id)sender;
- (void)zoomOut:(id)sender;
- (void)resetZoom:(id)sender;
- (void)reloadLocalization;
- (void)updateStartupEnabled:(NSNumber *)enabled error:(NSString *)error busy:(BOOL)busy;
@end
