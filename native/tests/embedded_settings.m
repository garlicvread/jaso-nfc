#import "../macos/SetupWindow.h"
#import "../macos/SettingsWindow.h"
#import "../macos/ContentZoom.h"
#import <objc/runtime.h>

@interface NSWindowController (EmbeddingContract)
- (NSView *)embeddedContentViewForWindow:(NSWindow *)host;
@end

static NSUserDefaults *TestDefaults;
static id IsolatedDefaults(id self, SEL selector) { return TestDefaults; }
static NSWindow *PanelHost;
static void CapturePanel(id panel, SEL command, NSWindow *host, void (^completion)(NSModalResponse)) {
    PanelHost = host;
    completion(NSModalResponseCancel);
}
static void Require(BOOL condition, NSString *message) {
    if (!condition) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSView *Find(NSView *view, NSString *identifier) {
    if ([view.identifier isEqual:identifier]) return view;
    for (NSView *child in view.subviews) { NSView *found = Find(child, identifier); if (found) return found; }
    return nil;
}
static void CheckBackgroundPaint(NSWindowController *controller) {
    NSView *view = [controller embeddedContentViewForWindow:controller.window];
    view.frame = NSMakeRect(0, 0, 80, 80);
    NSBitmapImageRep *bitmap = [[NSBitmapImageRep alloc] initWithBitmapDataPlanes:NULL pixelsWide:240 pixelsHigh:80 bitsPerSample:8 samplesPerPixel:4 hasAlpha:YES isPlanar:NO colorSpaceName:NSDeviceRGBColorSpace bytesPerRow:0 bitsPerPixel:0];
    NSGraphicsContext *context = [NSGraphicsContext graphicsContextWithBitmapImageRep:bitmap];
    [NSGraphicsContext saveGraphicsState];
    [NSGraphicsContext setCurrentContext:context];
    CGContextClearRect(context.CGContext, CGRectMake(0, 0, 240, 80));
    CGContextTranslateCTM(context.CGContext, 80, 0);
    [view drawRect:NSMakeRect(-80, 0, 240, 80)];
    [NSGraphicsContext restoreGraphicsState];
    Require([bitmap colorAtX:120 y:40].alphaComponent > .9, @"An embedded page must paint its own background");
    Require([bitmap colorAtX:40 y:40].alphaComponent < .1 && [bitmap colorAtX:200 y:40].alphaComponent < .1, @"An oversized dirty region must not paint across the sidebar or outside the embedded page");
    [controller close];
}
static void SettleLayout(NSWindow *window) {
    [window.contentView layoutSubtreeIfNeeded];
    [window displayIfNeeded];
    [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.05]];
    [window.contentView layoutSubtreeIfNeeded];
}
static void CheckTopAlignment(NSWindowController *controller, NSString *headingIdentifier) {
    NSWindow *host = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 860, 760) styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
    host.releasedWhenClosed = NO;
    NSView *view = [controller embeddedContentViewForWindow:host]; view.translatesAutoresizingMaskIntoConstraints = NO;
    [host.contentView addSubview:view];
    [NSLayoutConstraint activateConstraints:@[[view.leadingAnchor constraintEqualToAnchor:host.contentView.leadingAnchor constant:180], [view.trailingAnchor constraintEqualToAnchor:host.contentView.trailingAnchor], [view.topAnchor constraintEqualToAnchor:host.contentView.topAnchor], [view.bottomAnchor constraintEqualToAnchor:host.contentView.bottomAnchor]]];
    [host orderFront:nil];
    for (NSNumber *height in @[@760, @860, @660]) {
        [host setContentSize:NSMakeSize(860, height.doubleValue)]; SettleLayout(host);
        NSView *heading = Find(view, headingIdentifier);
        NSRect frame = [heading.superview convertRect:[heading alignmentRectForFrame:heading.frame] toView:view];
        CGFloat top = NSMaxY(view.bounds) - NSMaxY(frame);
        Require(fabs(top - 24) < 1, [NSString stringWithFormat:@"%@ must remain 24 points below the top after resize, got %.1f at height %.0f", headingIdentifier, top, height.doubleValue]);
    }
    [view removeFromSuperview];
    [host close];
    // The screen may cap a real window below the document's height. Exercise
    // both document lengths with the same production view in a sized container.
    NSView *geometry = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 860, 760)];
    [geometry addSubview:view];
    [NSLayoutConstraint activateConstraints:@[[view.leadingAnchor constraintEqualToAnchor:geometry.leadingAnchor constant:180], [view.trailingAnchor constraintEqualToAnchor:geometry.trailingAnchor], [view.topAnchor constraintEqualToAnchor:geometry.topAnchor], [view.bottomAnchor constraintEqualToAnchor:geometry.bottomAnchor]]];
    [geometry layoutSubtreeIfNeeded];
    for (NSNumber *margin in @[@120, @(-120), @80]) {
        NSView *heading = Find(view, headingIdentifier);
        NSScrollView *scroll = heading.enclosingScrollView;
        CGFloat reserved = geometry.bounds.size.height - scroll.contentView.bounds.size.height;
        CGFloat height = scroll.documentView.frame.size.height + reserved + margin.doubleValue;
        [geometry setFrameSize:NSMakeSize(860, height)]; [geometry layoutSubtreeIfNeeded];
        CGFloat difference = scroll.contentView.bounds.size.height - scroll.documentView.frame.size.height;
        Require(margin.doubleValue > 0 ? difference > 0 : difference < 0, [NSString stringWithFormat:@"%@ must exercise the requested short/long document: margin=%.0f document=%.1f viewport=%.1f", headingIdentifier, margin.doubleValue, scroll.documentView.frame.size.height, scroll.contentView.bounds.size.height]);
        NSRect frame = [heading.superview convertRect:[heading alignmentRectForFrame:heading.frame] toView:view];
        CGFloat top = NSMaxY(view.bounds) - NSMaxY(frame);
        Require(fabs(top - 24) < 1, [NSString stringWithFormat:@"%@ must remain top-aligned with short and long documents, got %.1f", headingIdentifier, top]);
    }
}
static void CheckCompactFolders(void) {
    JasoSetupWindowController *setup = JasoSetupWindowController.new;
    NSWindow *host = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 660, 660) styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
    host.releasedWhenClosed = NO;
    NSView *view = [setup embeddedContentViewForWindow:host]; view.translatesAutoresizingMaskIntoConstraints = NO;
    [host.contentView addSubview:view];
    [NSLayoutConstraint activateConstraints:@[[view.leadingAnchor constraintEqualToAnchor:host.contentView.leadingAnchor constant:152], [view.trailingAnchor constraintEqualToAnchor:host.contentView.trailingAnchor], [view.topAnchor constraintEqualToAnchor:host.contentView.topAnchor], [view.bottomAnchor constraintEqualToAnchor:host.contentView.bottomAnchor]]];
    NSString *longName = @"/Volumes/Archive containing long Korean and English project folder names — 연구 개발 문서 백업 및 사진 보관 드라이브";
    NSDictionary *config = @{ @"scope":@"all-user-files", @"roots":@[], @"excludes":@[@"/Users/example/Library/CloudStorage/GoogleDrive-personal/My Drive/Long archived project folder names"], @"apply":@NO, @"drives":@{@"mode":@"selected", @"included":@[], @"excluded":@[]} };
    [setup updateConfiguration:@{@"config":config, @"revision":@"layout", @"drive_inventory":@[@{@"uuid":@"long-drive", @"mount":longName, @"connected":@YES, @"included":@NO}], @"drive_inventory_issues":@[@{@"mount":longName, @"reason":@"Identity unavailable"}], @"drive_inventory_complete":@NO} error:nil];
    Require([[(NSPopUpButton *)Find(view, @"setup-add-drive") lastItem].title isEqual:longName.lastPathComponent], @"Fitting the drive picker must preserve full drive names in its menu");
    [host orderFront:nil];
    for (NSString *language in @[@"en", @"ko"]) {
        [TestDefaults setObject:language forKey:@"interfaceLanguage"]; [setup reloadLocalization];
        for (NSNumber *width in @[@660, @620]) for (NSNumber *zoom in @[@1, @2, @1]) {
            [host setContentSize:NSMakeSize(width.doubleValue, 660)];
            JasoSetContentZoom(zoom.doubleValue); SettleLayout(host);
            [(NSButton *)Find(view, @"setup-exclusions-toggle") performClick:nil];
            NSView *drives = Find(view, @"setup-drives-card"); [drives scrollRectToVisible:drives.bounds]; SettleLayout(host);
            NSString *context = [NSString stringWithFormat:@"%@ %.0fpx %.1fx, actual %.0fpx", language, width.doubleValue, zoom.doubleValue, host.contentView.bounds.size.width];
            Require(fabs(host.contentView.bounds.size.width - width.doubleValue) < 1, [@"Folder configuration and zoom must preserve compact window width: " stringByAppendingString:context]);
            Require(fabs(view.frame.size.width - (width.doubleValue - 152)) < 1, @"Long drive labels must fit the embedded content column");
            NSTextField *warning = (id)Find(view, @"setup-drive-inventory-warning");
            NSSize warningSize = [warning.cell cellSizeForBounds:NSMakeRect(0, 0, warning.bounds.size.width, CGFLOAT_MAX)];
            Require(!warning.hiddenOrHasHiddenAncestor && warningSize.height <= warning.bounds.size.height + 1, @"Degraded inventory warnings must wrap completely within the compact folder page");
            for (NSString *identifier in @[@"setup-add-folder", @"setup-remove-folder", @"setup-add-exclude", @"setup-remove-exclude", @"setup-preview", @"setup-cancel", @"setup-save", @"setup-start"]) {
                NSButton *button = (id)Find(view, identifier);
                if (!button.hiddenOrHasHiddenAncestor) Require(button.cell.cellSize.width <= button.bounds.size.width + 1, [NSString stringWithFormat:@"Compact folder action title must remain visible: %@ (%@)", button.title, context]);
            }
        }
    }
    [host close]; JasoSetContentZoom(1); [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"];
}
static void CheckEmbedding(NSWindowController *controller) {
    Require([controller respondsToSelector:@selector(embeddedContentViewForWindow:)], @"Settings controllers must provide an embedded view");
    NSWindow *original = controller.window;
    NSWindow *host = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 950, 760) styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    host.releasedWhenClosed = NO;
    NSView *view = [controller embeddedContentViewForWindow:host];
    view.translatesAutoresizingMaskIntoConstraints = NO;
    [host.contentView addSubview:view];
    [NSLayoutConstraint activateConstraints:@[
        [view.leadingAnchor constraintEqualToAnchor:host.contentView.leadingAnchor constant:190],
        [view.trailingAnchor constraintEqualToAnchor:host.contentView.trailingAnchor],
        [view.topAnchor constraintEqualToAnchor:host.contentView.topAnchor],
        [view.bottomAnchor constraintEqualToAnchor:host.contentView.bottomAnchor]
    ]];
    [host.contentView layoutSubtreeIfNeeded];
    Require(view.window == host && !original.visible, @"Embedding must use the shared window without opening another window");
    Require(fabs(view.frame.size.width - 760) < 1, @"Embedded settings must fill the content column rather than the former window width");
    Require([controller embeddedContentViewForWindow:host] == view, @"Returning to settings must preserve the existing view and draft");
    if ([controller isKindOfClass:JasoSetupWindowController.class]) {
        JasoSetupWindowController *setup = (id)controller;
        [setup updateConfiguration:@{@"config":@{@"scope":@"configured", @"roots":@[@"/fixture/Documents"], @"excludes":@[], @"apply":@NO}, @"revision":@"fixture"} error:nil];
        Method method = class_getInstanceMethod(NSSavePanel.class, @selector(beginSheetModalForWindow:completionHandler:));
        IMP previous = method_setImplementation(method, (IMP)CapturePanel);
        @try { [(NSButton *)Find(view, @"setup-add-folder") performClick:nil]; }
        @finally { method_setImplementation(method, previous); }
        Require(PanelHost == host, @"Folder selection must attach to the shared window");
        Require(![setup.draft[@"apply"] boolValue], @"Embedding must preserve the saved cleanup mode");
    } else {
        Require(Find(view, @"setup-folders").hiddenOrHasHiddenAncestor, @"Embedded Settings must use sidebar folder navigation without a duplicate folder card");
        NSButton *help = (id)Find(view, @"settings-advanced-toggle");
        Require(help && ![help.accessibilityValue boolValue], @"Advanced settings must start collapsed");
        NSView *guide = Find(view, @"full-disk-access-guide");
        Require(guide.hiddenOrHasHiddenAncestor, @"Permission instructions must sit inside Advanced settings");
        [help performClick:nil];
        Require(!guide.hiddenOrHasHiddenAncestor, @"Advanced settings must open the existing permission controls");
        JasoSettingsWindowController *settings = (id)controller;
        __block NSUInteger storageRequests = 0;
        settings.storageHandler = ^{ storageRequests++; };
        [(NSButton *)Find(view, @"settings-storage") performClick:nil];
        Require(storageRequests == 1, @"Storage usage must open through its host callback");
        NSMutableArray *actions = NSMutableArray.new;
        settings.actionHandler = ^(NSString *action) { [actions addObject:action]; };
        for (NSString *action in @[@"reconcile", @"restart", @"diagnostics"]) {
            NSButton *button = (id)Find(view, action);
            Require(button != nil && !button.hiddenOrHasHiddenAncestor, @"Advanced settings must contain the maintenance action");
            [button performClick:nil];
        }
        Require([actions isEqual:@[@"reconcile", @"restart", @"diagnostics"]], @"Advanced buttons must preserve explicit maintenance callbacks");
    }
    [view removeFromSuperview];
    [host close];
}
static void CheckDriveDetails(void) {
    JasoSetContentZoom(2);
    JasoSetupWindowController *setup = JasoSetupWindowController.new;
    NSWindow *host = [[NSWindow alloc] initWithContentRect:NSMakeRect(0,0,950,760) styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    host.releasedWhenClosed = NO;
    NSView *view = [setup embeddedContentViewForWindow:host]; host.contentView = view;
    NSDictionary *reference = @{@"uuid":@"selected-id", @"mount":@"/Volumes/Work"};
    NSDictionary *inventory = @{@"uuid":@"selected-id", @"mount":@"/Volumes/Work", @"connected":@YES, @"included":@YES};
    NSDictionary *draft = @{@"scope":@"all-user-files", @"roots":@[], @"excludes":@[], @"apply":@NO, @"drives":@{@"mode":@"selected", @"included":@[reference], @"excluded":@[]}};
    [setup updateConfiguration:@{@"config":draft, @"revision":@"drive-revision", @"drive_inventory":@[inventory], @"paused":@YES} error:nil];
    NSTableView *drives = (id)Find(view, @"setup-drives");
    [drives selectRowIndexes:[NSIndexSet indexSetWithIndex:0] byExtendingSelection:NO];
    __block NSString *startedUUID = nil;
    setup.startDriveHandler = ^(NSString *uuid, NSString *revision) { Require([revision isEqual:@"saved-drive-revision"], @"Drive approval uses the loaded configuration revision"); startedUUID = uuid; };
    NSDictionary *before = setup.draft;
    [(NSButton *)Find(view, @"setup-manage-drive") performClick:nil];
    Require(host.attachedSheet != nil && !setup.window.visible, @"Drive details must be a sheet on the shared window");
    NSTextField *title = (id)Find(host.attachedSheet.contentView, @"setup-drive-title");
    Require(title.font.pointSize >= 39, @"Drive details must apply the shared content zoom");
    NSScrollView *scroll = (id)Find(host.attachedSheet.contentView, @"setup-drive-sheet-scroll");
    [host.attachedSheet.contentView layoutSubtreeIfNeeded];
    Require(scroll.hasVerticalScroller && scroll.documentView.frame.size.width <= scroll.contentView.bounds.size.width + 1, @"Drive details must remain readable and scrollable at 200 percent");
    NSButton *removeControl = (id)Find(host.attachedSheet.contentView, @"setup-drive-details-remove");
    Require(removeControl.enabled && removeControl.cell.cellSize.width <= removeControl.bounds.size.width + 1, @"Drive removal must fit the sheet at 200 percent");
    Require(NSIntersectsRect(scroll.documentVisibleRect, [scroll.documentView convertRect:title.bounds fromView:title]), @"Drive details must open at the title when enlarged");
    NSView *saveControl = Find(host.attachedSheet.contentView, @"setup-drive-details-save");
    [saveControl scrollRectToVisible:saveControl.bounds];
    Require(NSIntersectsRect(scroll.documentVisibleRect, [scroll.documentView convertRect:saveControl.bounds fromView:saveControl]), @"The enlarged sheet must allow scrolling to its Save action");
    NSPopUpButton *picker = (id)Find(host.attachedSheet.contentView, @"setup-drive-reconnect");
    [picker selectItemAtIndex:2]; [NSApp sendAction:picker.action to:picker.target from:picker];
    Require(![(NSButton *)Find(host.attachedSheet.contentView, @"setup-start-drive") isEnabled], @"An unsaved mode must not start a drive");
    [(NSButton *)Find(host.attachedSheet.contentView, @"setup-drive-details-cancel") performClick:nil];
    Require([setup.draft isEqual:before] && !startedUUID, @"Cancelling must preserve folder and reconnect choices");
    __block NSDictionary *saved = nil;
    setup.saveHandler = ^(NSDictionary *value, NSString *revision, BOOL start) { Require(!start && [revision isEqual:@"drive-revision"], @"Reconnect preference save must preserve application run state"); saved = value; };
    [(NSButton *)Find(view, @"setup-manage-drive") performClick:nil];
    picker = (id)Find(host.attachedSheet.contentView, @"setup-drive-reconnect");
    [picker selectItemAtIndex:2]; [NSApp sendAction:picker.action to:picker.target from:picker];
    [(NSButton *)Find(host.attachedSheet.contentView, @"setup-drive-details-save") performClick:nil];
    Require([saved[@"drives"][@"reconnect"] isEqual:@[@{@"uuid":@"selected-id", @"mount":@"/Volumes/Work", @"mode":@"manual"}]], @"Saving commits the selected drive preference only");
    Require(![saved[@"apply"] boolValue] && !startedUUID, @"Saving must preserve preview mode until explicit automatic cleanup start");
    [setup updateSave:@{@"config":saved, @"revision":@"saved-drive-revision", @"drive_inventory":@[inventory], @"paused":@YES} error:nil];
    [drives selectRowIndexes:[NSIndexSet indexSetWithIndex:0] byExtendingSelection:NO];
    [(NSButton *)Find(view, @"setup-manage-drive") performClick:nil];
    NSButton *start = (id)Find(host.attachedSheet.contentView, @"setup-start-drive");
    Require(start.enabled, @"A saved manual connected drive offers an explicit start"); [start performClick:nil];
    Require([startedUUID isEqual:@"selected-id"], @"Explicit drive start must authorize its UUID only");
    [setup updateDriveStart:@{@"ready":@YES, @"running":@YES, @"paused":@YES} error:nil];
    Require(![setup.draft[@"apply"] boolValue], @"Drive approval must preserve preview mode");
    [host close];
    JasoSetContentZoom(1);
}
static void CheckDriveRemoval(void) {
    for (NSString *scope in @[@"all-user-files", @"configured"]) {
        for (NSNumber *keepLocal in @[@NO, @YES]) {
            JasoSetupWindowController *setup = JasoSetupWindowController.new;
            NSView *view = setup.window.contentView;
            NSDictionary *archive = @{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive"};
            NSDictionary *preference = @{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive", @"mode":@"manual"};
            NSArray *roots = [scope isEqual:@"configured"] ? (keepLocal.boolValue ? @[@"/Volumes/Archive/Documents", @"/fixture/Documents"] : @[@"/Volumes/Archive/Documents"]) : @[];
            NSDictionary *config = @{@"scope":scope, @"roots":roots, @"excludes":@[], @"apply":@YES, @"drives":@{@"mode":@"automatic", @"included":@[archive], @"excluded":@[], @"reconnect":@[preference]}};
            NSDictionary *inventory = @{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive", @"connected":@NO, @"included":@YES};
            NSDictionary *result = @{@"config":config, @"revision":@"remove-revision", @"drive_inventory":@[inventory], @"paused":@YES};
            [setup updateConfiguration:result error:nil];
            NSTableView *drives = (id)Find(view, @"setup-drives");
            [drives selectRowIndexes:[NSIndexSet indexSetWithIndex:0] byExtendingSelection:NO];
            NSButton *listRemove = (id)Find(view, @"setup-remove-drive");
            Require(listRemove.enabled && !listRemove.hiddenOrHasHiddenAncestor, @"Offline drive removal must be available for automatic and configured folders");
            [(NSButton *)Find(view, @"setup-manage-drive") performClick:nil];
            NSButton *remove = (id)Find(setup.window.attachedSheet.contentView, @"setup-drive-details-remove");
            Require(remove && remove.enabled && !remove.hiddenOrHasHiddenAncestor, @"Offline drive details must offer removal directly");
            __block NSDictionary *saved = nil;
            __block NSUInteger saveCount = 0;
            setup.saveHandler = ^(NSDictionary *value, NSString *revision, BOOL start) {
                Require([revision isEqual:@"remove-revision"] && !start, @"Removing a drive must save without starting cleanup");
                saved = value; saveCount++;
            };
            [remove performClick:nil];
            Require(saved && saveCount == 1 && !setup.window.attachedSheet, @"Detail removal must close and persist immediately");
            Require([saved[@"drives"][@"included"] count] == 0 && [saved[@"drives"][@"reconnect"] count] == 0 && [saved[@"drives"][@"excluded"] containsObject:archive], @"Removal must clear selection and reconnect preference while remembering exclusion");
            NSArray *expectedRoots = [scope isEqual:@"configured"] && keepLocal.boolValue ? @[@"/fixture/Documents"] : @[];
            Require([saved[@"roots"] isEqual:expectedRoots] && [saved[@"scope"] isEqual:scope] && [saved[@"apply"] boolValue], @"Configured drive removal must prune its roots, preserve local folders, and allow an empty idle scope");
            [setup updateSave:nil error:@"Fixture save failure"];
            NSButton *retry = (id)Find(view, @"setup-save");
            Require(retry.enabled && [setup.draft isEqual:saved], @"Failed removal must preserve the reduced scope for retry");
            [retry performClick:nil];
            Require(saveCount == 2, @"A failed drive removal must be retryable");
            [setup updateSave:@{@"config":saved, @"revision":@"removed-revision", @"drive_inventory":@[inventory], @"paused":@YES} error:nil];
            Require(drives.numberOfRows == 0, @"An old inventory response must not restore an excluded drive");
            [setup updateDriveInventory:@{@"drive_inventory":@[inventory], @"drive_inventory_complete":@YES} error:nil];
            Require(drives.numberOfRows == 0, @"Refreshing drives must preserve saved removal");
            [setup close];
        }
    }
}
static void CheckConfiguredReplacement(void) {
    JasoSetupWindowController *setup = JasoSetupWindowController.new;
    NSArray *roots = @[@"/Volumes/Work/Documents"];
    NSDictionary *config = @{@"scope":@"configured", @"roots":roots, @"excludes":@[], @"apply":@NO};
    NSArray *inventory = @[@{@"uuid":@"remembered", @"mount":@"/Volumes/Work", @"connected":@NO, @"included":@YES}, @{@"uuid":@"replacement", @"mount":@"/Volumes/Work", @"connected":@YES, @"included":@NO}, @{@"uuid":@"unrelated", @"mount":@"/Volumes/Other", @"connected":@YES, @"included":@NO}];
    [setup updateConfiguration:@{@"config":config, @"revision":@"configured-revision", @"drive_inventory":inventory} error:nil];
    NSTableView *drives = (id)Find(setup.window.contentView, @"setup-drives");
    Require(drives.numberOfRows == 1, @"A replacement UUID at the same path must wait outside managed drives");
    NSPopUpButton *add = (id)Find(setup.window.contentView, @"setup-add-drive");
    Require(add.enabled && !add.hiddenOrHasHiddenAncestor && add.numberOfItems == 2, @"Configured folders must offer explicit replacement approval without admitting unrelated drives");
    [add selectItemAtIndex:1]; [NSApp sendAction:add.action to:add.target from:add];
    Require([setup.draft[@"roots"] isEqual:roots] && [setup.draft[@"drives"][@"included"] isEqual:@[@{@"uuid":@"replacement", @"mount":@"/Volumes/Work"}]], @"Approving a replacement must preserve the configured folder scope");
    [drives selectRowIndexes:[NSIndexSet indexSetWithIndex:0] byExtendingSelection:NO];
    [(NSButton *)Find(setup.window.contentView, @"setup-remove-drive") performClick:nil];
    Require([setup.draft[@"roots"] isEqual:roots] && drives.numberOfRows == 1, @"Removing an old drive identity must preserve folders used by its selected replacement");
    [setup close];
}
static void CheckDriveRemovalWithOtherEdits(void) {
    JasoSetupWindowController *setup = JasoSetupWindowController.new;
    NSDictionary *config = @{@"scope":@"configured", @"roots":@[@"/Volumes/Archive/Work"], @"excludes":@[], @"apply":@YES};
    NSDictionary *inventory = @{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive", @"connected":@NO, @"included":@YES};
    [setup updateConfiguration:@{@"config":config, @"revision":@"edited-revision", @"drive_inventory":@[inventory]} error:nil];
    [setup addFolderURLs:@[[NSURL fileURLWithPath:@"/fixture/Added documents"]] excluding:NO];
    NSView *view = setup.window.contentView;
    NSTableView *drives = (id)Find(view, @"setup-drives");
    [drives selectRowIndexes:[NSIndexSet indexSetWithIndex:0] byExtendingSelection:NO];
    __block NSDictionary *saved = nil;
    setup.saveHandler = ^(NSDictionary *draft, NSString *revision, BOOL start) { Require([revision isEqual:@"edited-revision"] && !start, @"Pending removal must save through the normal revision transaction"); saved = draft; };
    [(NSButton *)Find(view, @"setup-manage-drive") performClick:nil];
    [(NSButton *)Find(setup.window.attachedSheet.contentView, @"setup-drive-details-remove") performClick:nil];
    NSString *message = [(NSTextField *)Find(view, @"setup-message") stringValue];
    Require(!saved && [message containsString:@"Save settings"], @"Removal with other unreviewed changes must explicitly explain the remaining save step");
    Require([setup.draft[@"roots"] isEqual:@[@"/fixture/Added documents"]] && [setup.draft[@"drives"][@"excluded"] count] == 1, @"Pending removal must preserve unrelated edits");
    setup.previewHandler = ^(NSDictionary *draft) {};
    [(NSButton *)Find(view, @"setup-preview") performClick:nil];
    [setup updatePreview:@{@"candidates":@[], @"errors":@[], @"revision":@"edited-revision", @"complete":@YES} error:nil];
    [(NSButton *)Find(view, @"setup-save") performClick:nil];
    Require(saved != nil, @"Preview then Save must persist the pending removal and other edits");
    [setup close];
}
int main(void) {
    @autoreleasepool {
        [NSApplication sharedApplication]; [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSString *suite = [@"jaso-nfc.embedded-settings-test." stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults = [[NSUserDefaults alloc] initWithSuiteName:suite];
        [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"];
        Method method = class_getClassMethod(NSUserDefaults.class, @selector(standardUserDefaults));
        IMP original = method_setImplementation(method, (IMP)IsolatedDefaults);
        @try {
            CheckBackgroundPaint([JasoSetupWindowController new]);
            CheckBackgroundPaint([JasoSettingsWindowController new]);
            CheckTopAlignment(JasoSettingsWindowController.new, @"settings-heading");
            CheckTopAlignment(JasoSetupWindowController.new, @"setup-heading");
            CheckCompactFolders();
            CheckEmbedding([JasoSetupWindowController new]);
            CheckEmbedding([JasoSettingsWindowController new]);
            CheckDriveDetails();
            CheckDriveRemoval();
            CheckConfiguredReplacement();
            CheckDriveRemovalWithOtherEdits();
            puts("PASS embedded settings, folder panel host and permission help");
            return 0;
        } @catch (NSException *error) {
            fprintf(stderr, "FAIL %s\n", error.reason.UTF8String); return 1;
        } @finally {
            method_setImplementation(method, original);
            [TestDefaults removePersistentDomainForName:suite]; TestDefaults = nil;
        }
    }
}
