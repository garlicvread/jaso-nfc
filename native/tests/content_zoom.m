#import "../macos/StatusWindow.h"
#import "../macos/SettingsWindow.h"
#import "../macos/Localization.h"
#import <objc/runtime.h>
#import <math.h>

@interface NSWindowController (ContentZoomTests)
@property (readonly) CGFloat contentZoom;
- (void)zoomIn:(id)sender;
- (void)zoomOut:(id)sender;
- (void)resetZoom:(id)sender;
@end

static NSUserDefaults *TestDefaults;
static NSUInteger Checks;
static id IsolatedDefaults(id self, SEL selector) { (void)self; (void)selector; return TestDefaults; }
static void Require(BOOL value, NSString *message) {
    Checks++;
    if (!value) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSView *Find(NSView *view, NSString *identifier) {
    if ([view.identifier isEqual:identifier]) return view;
    for (NSView *child in view.subviews) { NSView *found = Find(child, identifier); if (found) return found; }
    return nil;
}
static NSScrollView *Scroll(NSView *view) {
    if ([view isKindOfClass:NSScrollView.class]) return (NSScrollView *)view;
    for (NSView *child in view.subviews) { NSScrollView *found = Scroll(child); if (found) return found; }
    return nil;
}
static CGFloat LargestFont(NSView *view) {
    CGFloat size = [view isKindOfClass:NSControl.class] ? [(NSControl *)view font].pointSize : 0;
    for (NSView *child in view.subviews) size = MAX(size, LargestFont(child));
    return size;
}
static void CheckLayout(NSView *view, NSView *document) {
    if ([view isKindOfClass:NSScrollView.class]) return;
    if ([view isKindOfClass:NSTextField.class] || [view isKindOfClass:NSButton.class]) {
        NSRect frame = [document convertRect:view.bounds fromView:view];
        Require(!NSIsEmptyRect(frame) && NSContainsRect(NSInsetRect(document.bounds, -1, -1), frame), [NSString stringWithFormat:@"A zoomed control is outside its container: %@ %@ in %@", [(NSControl *)view stringValue], NSStringFromRect(frame), NSStringFromRect(document.bounds)]);
        NSControl *control = (NSControl *)view;
        NSSize required = [control.cell cellSizeForBounds:NSMakeRect(0, 0, view.bounds.size.width, CGFLOAT_MAX)];
        Require(required.height <= view.bounds.size.height + 1, [NSString stringWithFormat:@"Zoomed text is vertically clipped: %@", control.stringValue]);
        if ([view isKindOfClass:NSButton.class]) {
            Require(control.cell.cellSize.width <= view.bounds.size.width + 1, @"A zoomed button title is horizontally clipped");
            NSRect titleRect = [control.cell titleRectForBounds:control.bounds];
            CGFloat fontHeight = ceil(control.font.ascender - control.font.descender);
            Require(fontHeight <= MIN(titleRect.size.height, control.bounds.size.height) + 1, [NSString stringWithFormat:@"A zoomed button's font is taller than its visible title area: %@ (%g in %g)", [(NSButton *)control title], fontHeight, titleRect.size.height]);
        }
    }
    for (NSView *child in view.subviews) CheckLayout(child, document);
}
static void Render(NSWindowController *controller, NSString *path) {
    NSView *view = controller.window.contentView;
    NSBitmapImageRep *bitmap = [view bitmapImageRepForCachingDisplayInRect:view.bounds];
    [view cacheDisplayInRect:view.bounds toBitmapImageRep:bitmap];
    Require([[bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}] writeToFile:path atomically:YES], @"Could not render zoomed window");
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSString *suite = [@"jaso-nfc.content-zoom-test." stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults = [[NSUserDefaults alloc] initWithSuiteName:suite];
        [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"];
        Method method = class_getClassMethod(NSUserDefaults.class, @selector(standardUserDefaults));
        IMP original = method_setImplementation(method, (IMP)IsolatedDefaults);
        JasoStatusWindowController *status = nil;
        JasoSettingsWindowController *settings = nil;
        int result = 0;
        @try {
            if (argc > 1) Require([NSFileManager.defaultManager createDirectoryAtPath:[NSString stringWithUTF8String:argv[1]] withIntermediateDirectories:YES attributes:nil error:NULL], @"Could not create zoom preview directory");
            status = [JasoStatusWindowController new]; settings = [JasoSettingsWindowController new];
            for (NSWindowController *controller in @[status, settings])
                for (NSString *selector in @[@"zoomIn:", @"zoomOut:", @"resetZoom:", @"contentZoom"])
                    Require([controller respondsToSelector:NSSelectorFromString(selector)], @"Both production window controllers must support content zoom");
            NSDictionary *snapshot = @{@"running":@YES, @"paused":@NO, @"apply":@YES, @"baseline_complete":@YES,
                @"indexed_entries":@1234567, @"pending_jobs":@0, @"deferred_jobs":@0, @"deferred_renames":@0,
                @"errors":@0, @"active_roots":@[@"/Users/example/Documents"], @"unavailable_roots":@{@"/Volumes/Archive":@"Operation timed out (os error 60)"},
                @"catalog_unavailable":@{}, @"pending_recovery":@NO, @"needs_revalidation":@NO};
            [status updateSnapshot:snapshot error:nil updatedAt:NSDate.date];
            // Exercise initial display placement on any runner screen geometry.
            // This is not a zoom action: AppKit must first bring the window on screen.
            for (NSWindowController *controller in @[status, settings]) {
                NSRect screen = NSScreen.mainScreen.visibleFrame;
                [controller.window setFrameOrigin:NSMakePoint(NSMaxX(screen) + 100, NSMaxY(screen) + 100)];
                [controller showWindow:nil];
                [controller.window orderOut:nil];
            }
            CGFloat statusFont = LargestFont(status.window.contentView), settingsFont = LargestFont(settings.window.contentView);
            NSRect statusFrame = status.window.frame, settingsFrame = settings.window.frame;
            __block NSUInteger refreshes = 0, languageChanges = 0;
            status.refreshHandler = ^{ refreshes++; };
            settings.languageChangedHandler = ^{ languageChanges++; };
            [status zoomIn:nil];
            Require(fabs(status.contentZoom - 1.1) < .001 && fabs(settings.contentZoom - 1.1) < .001, @"Zoom updates the shared scale once");
            Require(fabs(LargestFont(status.window.contentView) - statusFont * 1.1) < .1 && fabs(LargestFont(settings.window.contentView) - settingsFont * 1.1) < .1, @"Zoom must resize real native fonts in both windows");
            Require(NSEqualRects(statusFrame, status.window.frame) && NSEqualRects(settingsFrame, settings.window.frame), @"Content zoom must not change either window frame");
            Require(refreshes == 0 && languageChanges == 0, @"Zoom must not invoke refresh or language callbacks");
            Require(fabs([TestDefaults doubleForKey:@"contentZoom"] - 1.1) < .001, @"Zoom preference must persist");
            JasoSettingsWindowController *reopened = [JasoSettingsWindowController new];
            Require(fabs(reopened.contentZoom - 1.1) < .001 && fabs(LargestFont(reopened.window.contentView) - settingsFont * 1.1) < .1, @"A reopened window must restore the saved scale");
            [reopened close];
            for (NSUInteger i=0; i<30; i++) {
                [settings zoomIn:nil];
                Require(NSEqualRects(statusFrame, status.window.frame) && NSEqualRects(settingsFrame, settings.window.frame), @"Every zoom step must preserve window frames");
                Require(fabs(status.window.contentView.frame.size.width - statusFrame.size.width) < 1 && fabs(settings.window.contentView.frame.size.width - settingsFrame.size.width) < 1, @"Reflow must preserve the actual content viewport width");
            }
            Require(fabs(status.contentZoom - 2) < .001 && fabs(settings.contentZoom - 2) < .001, @"Zoom stops at 200 percent");
            for (NSString *language in @[@"en", @"ko"]) {
                JasoSetLanguagePreference(language);
                [settings reloadLocalization];
                [status setRefreshing:YES];
                [status updateSnapshot:snapshot error:nil updatedAt:NSDate.date];
                Require(fabs(LargestFont(status.window.contentView) - statusFont * 2) < .1 && fabs(LargestFont(settings.window.contentView) - settingsFont * 2) < .1, @"Refresh and localization must preserve zoom without compounding it");
                for (NSWindowController *controller in @[status, settings]) {
                    NSRect beforeShow = controller.window.frame;
                    [controller showWindow:nil];
                    Require(NSEqualRects(beforeShow, controller.window.frame), [NSString stringWithFormat:@"Showing an already-placed zoomed window must preserve its frame: %@ -> %@", NSStringFromRect(beforeShow), NSStringFromRect(controller.window.frame)]);
                    [controller.window setFrame:NSMakeRect(controller.window.frame.origin.x, controller.window.frame.origin.y, [controller isKindOfClass:JasoStatusWindowController.class] ? 680 : 560, 520) display:NO];
                    [controller.window.contentView layoutSubtreeIfNeeded];
                    CheckLayout(controller.window.contentView, controller.window.contentView);
                    NSScrollView *scroll = Scroll(controller.window.contentView);
                    Require(scroll != nil && scroll.hasVerticalScroller, @"Both zoomed windows need vertical scrolling");
                    Require(scroll.documentView.frame.size.width <= scroll.contentView.bounds.size.width + 1, @"Content must reflow without horizontal scrolling");
                    Require(scroll.documentView.frame.size.height > scroll.contentView.bounds.size.height, @"Large content remains reachable in a scroll document");
                    CheckLayout(scroll.documentView, scroll.documentView);
                    if (argc > 1) Render(controller, [[NSString stringWithUTF8String:argv[1]] stringByAppendingPathComponent:[NSString stringWithFormat:@"zoom-%@-%@.png", [controller isKindOfClass:JasoStatusWindowController.class] ? @"status" : @"settings", language]]);
                }
            }
            NSPopUpButton *picker = (NSPopUpButton *)Find(settings.window.contentView, @"interface-language-picker");
            [picker selectItemAtIndex:2]; [NSApp sendAction:picker.action to:picker.target from:picker];
            Require(languageChanges == 1 && [JasoLanguagePreference() isEqual:@"en"] && fabs(settings.contentZoom - 2) < .001, @"The zoomed language picker keeps its action and preference");
            for (NSUInteger i=0; i<30; i++) [status zoomOut:nil];
            Require(fabs(status.contentZoom - .8) < .001 && fabs(settings.contentZoom - .8) < .001, @"Zoom stops at 80 percent");
            [settings resetZoom:nil];
            Require(fabs(status.contentZoom - 1) < .001 && fabs(LargestFont(status.window.contentView) - statusFont) < .1, @"Reset restores native font sizes and shared scale");
            [status.window orderOut:nil];
            Require(refreshes == 0 && !status.refreshingAutomatically, @"Zoom must not start hidden-window polling");
            printf("PASS: %lu content zoom assertions\n", (unsigned long)Checks);
        } @catch (NSException *error) { fprintf(stderr, "FAIL: %s\n", error.reason.UTF8String); result = 1; }
        @finally {
            [status close]; [settings close];
            method_setImplementation(method, original);
            [TestDefaults removePersistentDomainForName:suite]; TestDefaults = nil;
        }
        return result;
    }
}
