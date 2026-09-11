#import <AppKit/AppKit.h>

// The owner performs CLI work. This controller only edits and presents a draft.
@interface JasoSetupWindowController : NSWindowController <NSWindowDelegate, NSTableViewDataSource, NSTableViewDelegate>
@property (copy) void (^previewHandler)(NSDictionary *draft);
@property (copy) void (^saveHandler)(NSDictionary *draft, NSString *revision, BOOL start);
@property (copy) void (^reloadHandler)(void);
// A cancelled preview stays busy until its owner acknowledges process exit.
// Call setBusy:NO cancellable:NO and discard obsolete request responses.
@property (copy) void (^cancelHandler)(void);
@property (copy) void (^closeHandler)(void);
@property (readonly, copy) NSDictionary *draft;
@property (readonly, copy) NSString *revision;
@property (readonly) CGFloat contentZoom;
- (void)updateConfiguration:(NSDictionary *)result error:(NSString *)error;
- (void)updatePreview:(NSDictionary *)result error:(NSString *)error;
- (void)updateSave:(NSDictionary *)result error:(NSString *)error;
- (void)setBusy:(BOOL)busy cancellable:(BOOL)cancellable;
- (void)addFolderURLs:(NSArray<NSURL *> *)urls excluding:(BOOL)excluding;
- (void)reloadLocalization;
- (void)zoomIn:(id)sender;
- (void)zoomOut:(id)sender;
- (void)resetZoom:(id)sender;
@end
