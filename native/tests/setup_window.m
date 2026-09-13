#import "../macos/SetupWindow.h"
#import "../macos/Localization.h"
#import "../macos/ContentZoom.h"
#import <objc/runtime.h>

static NSUserDefaults *TestDefaults;
static id IsolatedDefaults(id self, SEL selector) { (void)self; (void)selector; return TestDefaults; }
static void Require(BOOL value, NSString *message) {
    if (!value) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSView *Find(NSView *view, NSString *identifier) {
    if ([view.identifier isEqual:identifier]) return view;
    for (NSView *child in view.subviews) { NSView *found = Find(child, identifier); if (found) return found; }
    return nil;
}
static NSButton *Button(JasoSetupWindowController *controller, NSString *identifier) {
    NSView *view = Find(controller.window.contentView, identifier);
    Require([view isKindOfClass:NSButton.class], [@"Missing button: " stringByAppendingString:identifier]);
    return (NSButton *)view;
}
static void Choose(NSPopUpButton *picker, NSString *value) {
    [picker.menu update];
    for (NSMenuItem *item in picker.itemArray) if ([item.representedObject isEqual:value]) {
        Require(picker.enabled && item.enabled, [NSString stringWithFormat:@"Setup option is disabled after menu validation: %@ / %@", picker.identifier, item.title]);
        [picker.menu performActionForItemAtIndex:[picker indexOfItem:item]]; return;
    }
    Require(NO, @"Requested setup option is missing");
}
static NSDictionary *Configuration(NSString *scope, BOOL apply) {
    return @{@"config":@{@"scope":scope, @"roots":[scope isEqual:@"all-user-files"] ? @[] : @[@"/fixture/Downloads"], @"excludes":@[@"/fixture/Downloads/Private"], @"apply":@(apply)}, @"revision":@"fixture-revision", @"running":@NO, @"paused":@YES};
}
static NSDictionary *Preview(void) {
    return @{@"candidates":@[@{@"path":@"/fixture/Downloads/보고서.txt", @"before":@"보고서.txt", @"after":@"보고서.txt"}], @"entries":@24, @"errors":@[], @"truncated":@NO, @"complete":@YES, @"revision":@"fixture-revision"};
}
static void CheckLayout(NSView *view, NSView *container);
static void Render(JasoSetupWindowController *controller, NSString *path);
static void CheckDegradedDriveInventory(void) {
    JasoSetupWindowController *controller = JasoSetupWindowController.new;
    NSDictionary *reference = @{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive"};
    NSDictionary *config = @{@"scope":@"configured", @"roots":@[@"/fixture/Documents", @"/Volumes/Archive/Work", @"/Volumes/Unknown/Work"], @"excludes":@[], @"apply":@NO, @"drives":@{@"mode":@"selected", @"included":@[reference], @"excluded":@[], @"reconnect":@[@{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive", @"mode":@"manual"}]}};
    NSArray *inventory = @[@{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive", @"included":@YES, @"connected":@NO, @"availability":@"unavailable"}, @{@"uuid":@"unknown-id", @"mount":@"/Volumes/Unknown", @"included":@NO, @"connected":@YES, @"availability":@"unavailable"}];
    NSArray *issues = @[@{@"mount":@"/Volumes/Archive", @"reason":@"Cannot read mounted drive identity: Operation canceled (os error 89)"}, @{@"mount":@"/Volumes/Unknown", @"reason":@"Cannot read mounted drive identity"}];
    [controller updateConfiguration:@{@"config":config, @"revision":@"degraded-revision", @"drive_inventory":inventory, @"drive_inventory_issues":issues, @"drive_inventory_complete":@NO} error:nil];
    NSTextField *warning = (id)Find(controller.window.contentView, @"setup-drive-inventory-warning");
    Require(warning && !warning.hiddenOrHasHiddenAncestor && [warning isDescendantOf:Find(controller.window.contentView, @"setup-drives-card")], @"Partial drive failures must be shown inside Drives instead of blocking all folder settings");
    Require(Button(controller, @"setup-add-folder").enabled && Button(controller, @"setup-preview").enabled && Button(controller, @"setup-save").enabled && [controller.revision isEqual:@"degraded-revision"], @"A degraded inventory must preserve editable folders and the configuration revision");
    Require(!Button(controller, @"setup-add-drive").enabled, @"An unavailable identity must not be offered for inclusion even if connected is inconsistent");
    NSTableView *table = (id)Find(controller.window.contentView, @"setup-drives");
    Require(table.numberOfRows == 1, @"Unknown issue paths must not create managed UUID rows");
    __block NSUInteger starts = 0; controller.startDriveHandler = ^(NSString *uuid, NSString *revision) { starts++; };
    for (NSString *language in @[@"en", @"ko"]) {
        [TestDefaults setObject:language forKey:@"interfaceLanguage"]; [controller reloadLocalization];
        Require([warning.stringValue containsString:@"/Volumes/Archive"] && [warning.stringValue containsString:@"/Volumes/Unknown"] && [warning.stringValue containsString:[language isEqual:@"en"] ? @"Refresh drives" : @"드라이브 새로고침"], @"Drive warnings must include affected paths and a localized refresh action");
        Require(![warning.stringValue containsString:@"os error 89"] && ![[(NSTextField *)Find(controller.window.contentView, @"setup-message") stringValue] containsString:@"/Volumes/Archive"], @"Raw identity diagnostics must stay outside the main setup message");
        NSTextField *row = (id)[controller tableView:table viewForTableColumn:table.tableColumns.firstObject row:0];
        Require([row.stringValue containsString:[language isEqual:@"en"] ? @"Unavailable" : @"확인할 수 없음"], @"Unavailable drives must be distinguished from disconnected drives");
        [table selectRowIndexes:[NSIndexSet indexSetWithIndex:0] byExtendingSelection:NO];
        [Button(controller, @"setup-manage-drive") performClick:nil];
        NSView *sheet = controller.window.attachedSheet.contentView;
        NSTextField *status = (id)Find(sheet, @"setup-drive-status");
        Require([status.stringValue containsString:[language isEqual:@"en"] ? @"Unavailable" : @"확인할 수 없음"], @"Drive details must explain that the identity check is unavailable");
        NSButton *start = (id)Find(sheet, @"setup-start-drive"); Require(!start.enabled, @"An unavailable drive must not be started");
        start.enabled = YES; [start performClick:nil]; Require(starts == 0, @"The drive start action must also validate current availability");
        [(NSButton *)Find(sheet, @"setup-drive-details-cancel") performClick:nil];
    }
    [controller addFolderURLs:@[[NSURL fileURLWithPath:@"/fixture/New folder"]] excluding:NO];
    NSDictionary *draft = controller.draft;
    [controller updateDriveInventory:@{@"drive_inventory":@[], @"drive_inventory_issues":@[], @"drive_inventory_complete":@YES} error:nil];
    Require(warning.hiddenOrHasHiddenAncestor && [controller.draft isEqual:draft], @"A successful refresh must clear drive warnings and preserve the unsaved folder draft");
    NSMutableArray *manyIssues = NSMutableArray.new;
    for (NSUInteger i=0; i<20; i++) [manyIssues addObject:@{@"mount":[NSString stringWithFormat:@"/Volumes/%lu/%@", (unsigned long)i, [@"Long name " stringByPaddingToLength:400 withString:@"Long name " startingAtIndex:0]], @"reason":@"Unavailable"}];
    [controller updateDriveInventory:@{@"drive_inventory":@[], @"drive_inventory_issues":manyIssues, @"drive_inventory_complete":@NO} error:nil];
    Require(warning.stringValue.length < 900 && [warning.stringValue containsString:@"17"], @"The drive warning must bound long mount lists and show the remaining count");
    [controller updateConfiguration:@{@"config":@{@"scope":@"configured", @"roots":@[@"/fixture/Documents"], @"excludes":@[], @"apply":@NO}, @"revision":@"local", @"drive_inventory":@[], @"drive_inventory_issues":issues, @"drive_inventory_complete":@NO} error:nil];
    Require(!warning.hiddenOrHasHiddenAncestor && Button(controller, @"setup-add-folder").enabled, @"A failed unknown drive must keep its warning visible while ordinary local folders remain editable");
    [controller close]; [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"];
}
static void CheckDriveManagement(NSString *output) {
    NSDictionary *archive = @{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive"};
    NSDictionary *work = @{@"uuid":@"work-id", @"mount":@"/Volumes/Work"};
    NSDictionary *newDrive = @{@"uuid":@"new-id", @"mount":@"/Volumes/New"};
    NSMutableDictionary *configuration = [Configuration(@"all-user-files", YES) mutableCopy];
    NSMutableDictionary *config = [configuration[@"config"] mutableCopy];
    config[@"drives"] = @{@"mode":@"automatic", @"included":@[], @"excluded":@[newDrive]};
    configuration[@"config"] = config;
    configuration[@"drive_inventory"] = @[
        @{@"uuid":@"archive-id", @"mount":@"/Volumes/Archive", @"name":@"Archive", @"connected":@NO, @"included":@YES},
        @{@"uuid":@"work-id", @"mount":@"/Volumes/Work", @"name":@"Work", @"connected":@YES, @"included":@YES},
        @{@"uuid":@"new-id", @"mount":@"/Volumes/New", @"name":@"New", @"connected":@YES, @"included":@NO}];
    JasoSetupWindowController *controller = [JasoSetupWindowController new];
    [controller updateConfiguration:configuration error:nil];
    NSPopUpButton *mode = (NSPopUpButton *)Find(controller.window.contentView, @"setup-drive-mode");
    NSTableView *drives = (NSTableView *)Find(controller.window.contentView, @"setup-drives");
    Require(mode && drives, @"Drive management must be available for automatic folder discovery");
    Require(drives.numberOfRows == 2, @"Managed drives include a disconnected remembered drive");
    for (NSString *language in @[@"en", @"ko"]) {
        [TestDefaults setObject:language forKey:@"interfaceLanguage"]; [controller reloadLocalization];
        for (NSNumber *zoom in @[@1.0, @2.0]) {
            JasoSetContentZoom(zoom.doubleValue); [controller.window setContentSize:NSMakeSize(560, 640)];
            [controller.window.contentView layoutSubtreeIfNeeded]; CheckLayout(controller.window.contentView, controller.window.contentView);
            if (output) {
                [NSFileManager.defaultManager createDirectoryAtPath:output withIntermediateDirectories:YES attributes:nil error:NULL];
                [Find(controller.window.contentView, @"setup-drives-card") scrollRectToVisible:Find(controller.window.contentView, @"setup-drives-card").bounds];
                Render(controller, [output stringByAppendingPathComponent:[NSString stringWithFormat:@"drives-%@-%.1f.png", language, zoom.doubleValue]]);
            }
        }
    }
    JasoSetContentZoom(1); [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"]; [controller reloadLocalization];
    [drives selectRowIndexes:[NSIndexSet indexSetWithIndex:0] byExtendingSelection:NO];
    [Button(controller, @"setup-remove-drive") performClick:nil];
    Require(drives.numberOfRows == 1 && [controller.draft[@"drives"][@"excluded"] containsObject:archive], @"Removing a disconnected drive must suppress its future automatic inclusion");
    Require(Button(controller, @"setup-save").enabled, @"Drive choices can be saved directly for the current cleanup mode");
    Choose(mode, @"selected");
    Require([controller.draft[@"drives"][@"mode"] isEqual:@"selected"] && [controller.draft[@"drives"][@"included"] isEqual:@[work]], @"Manual mode begins with the currently managed drives");
    NSPopUpButton *add = (NSPopUpButton *)Find(controller.window.contentView, @"setup-add-drive");
    Choose(add, @"new-id");
    Require(drives.numberOfRows == 2 && [controller.draft[@"drives"][@"included"] containsObject:newDrive] && ![controller.draft[@"drives"][@"excluded"] containsObject:newDrive], @"Adding a connected drive restores its inclusion");
    Require(Button(controller, @"setup-refresh-drives").enabled, @"The drive list can refresh without reloading the folder draft");
    __block BOOL refreshed = NO;
    controller.refreshDrivesHandler = ^{ refreshed = YES; };
    NSDictionary *beforeRefresh = controller.draft;
    [Button(controller, @"setup-refresh-drives") performClick:nil];
    Require(refreshed && !mode.enabled, @"Refresh dispatches inventory loading and freezes controls");
    NSMutableDictionary *refreshedConfiguration = [configuration mutableCopy];
    refreshedConfiguration[@"drive_inventory"] = [configuration[@"drive_inventory"] subarrayWithRange:NSMakeRange(0, 2)];
    [controller updateDriveInventory:refreshedConfiguration error:nil];
    Require([controller.draft isEqual:beforeRefresh] && mode.enabled, @"Refreshing connected drives preserves unsaved removal and manual selection");
    Require(drives.numberOfRows == 2, @"A newly selected drive remains manageable if it disconnects before saving");
    [controller updateDriveInventory:nil error:@"Temporary drive refresh failure"];
    [controller updateDriveInventory:refreshedConfiguration error:nil];
    Require(![[(NSTextField *)Find(controller.window.contentView, @"setup-message") stringValue] containsString:@"Temporary drive refresh failure"] && [controller.draft isEqual:beforeRefresh], @"A successful drive refresh clears its previous error and keeps the draft");
    __block NSDictionary *saved = nil;
    controller.saveHandler = ^(NSDictionary *draft, NSString *revision, BOOL start) {
        Require([revision isEqual:@"fixture-revision"] && !start, @"Drive save uses the existing revision and lifecycle transaction"); saved = draft;
    };
    [Button(controller, @"setup-save") performClick:nil];
    Require(saved && !mode.enabled && !add.enabled && !Button(controller, @"setup-remove-drive").enabled, @"Drive save carries preferences and freezes all controls");
    [controller updateSave:nil error:@"Retry the save"];
    Require([controller.draft isEqual:saved] && mode.enabled, @"A failed drive save preserves the selection for retry");
    configuration[@"config"] = saved;
    [controller updateConfiguration:configuration error:nil];
    Require([controller.draft isEqual:saved], @"Reload restores manual mode and saved drive choices");
    [TestDefaults setObject:@"ko" forKey:@"interfaceLanguage"]; [controller reloadLocalization];
    Require([mode.selectedItem.title isEqual:@"직접 선택"] && [Button(controller, @"setup-remove-drive").title containsString:@"제거"], @"Drive controls are localized in Korean");
    [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"]; [controller reloadLocalization];
    Choose((NSPopUpButton *)Find(controller.window.contentView, @"setup-scope"), @"configured");
    Require(mode.hiddenOrHasHiddenAncestor, @"Explicit folder selection keeps its own drive roots");
    [controller close];
}
static void CheckLayout(NSView *view, NSView *container) {
    if (view.hidden) return;
    if ([view isKindOfClass:NSScrollView.class]) {
        NSScrollView *scroll = (NSScrollView *)view;
        Require(scroll.documentView.frame.size.width <= scroll.contentView.bounds.size.width + 1, [NSString stringWithFormat:@"Setup document %@ is wider than its viewport: %.1f > %.1f", scroll.documentView.identifier, scroll.documentView.frame.size.width, scroll.contentView.bounds.size.width]);
        if (![scroll.documentView isKindOfClass:NSTableView.class]) CheckLayout(scroll.documentView, scroll.documentView);
        return;
    }
    if ([view isKindOfClass:NSTextField.class] || [view isKindOfClass:NSButton.class]) {
        NSRect rect = [container convertRect:view.bounds fromView:view];
        Require(!NSIsEmptyRect(rect) && NSContainsRect(NSInsetRect(container.bounds, -1, -1), rect), [NSString stringWithFormat:@"Setup control is outside its container: %@", view.identifier]);
        if ([view isKindOfClass:NSTextField.class]) {
            NSTextField *field = (NSTextField *)view;
            NSSize needed = [field.cell cellSizeForBounds:NSMakeRect(0, 0, field.bounds.size.width, CGFLOAT_MAX)];
            Require(needed.height <= field.bounds.size.height + 1, [NSString stringWithFormat:@"Setup label is vertically clipped: %@", field.stringValue]);
        } else {
            Require([(NSButton *)view cell].cellSize.width <= view.bounds.size.width + 1, [NSString stringWithFormat:@"Setup button title is clipped: %@", [(NSButton *)view title]]);
        }
    }
    for (NSView *child in view.subviews) CheckLayout(child, container);
}
static void Render(JasoSetupWindowController *controller, NSString *path) {
    NSView *view = controller.window.contentView;
    [view layoutSubtreeIfNeeded];
    [controller.window display];
    NSBitmapImageRep *bitmap = [view bitmapImageRepForCachingDisplayInRect:view.bounds];
    [view cacheDisplayInRect:view.bounds toBitmapImageRep:bitmap];
    NSData *png = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
    Require(png.length > 1000 && [png writeToFile:path atomically:YES], @"Could not render setup window");
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        [NSApplication sharedApplication]; [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSString *suite = [@"jaso-nfc.setup-window-test." stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults = [[NSUserDefaults alloc] initWithSuiteName:suite];
        [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"];
        Method method = class_getClassMethod(NSUserDefaults.class, @selector(standardUserDefaults));
        IMP original = method_setImplementation(method, (IMP)IsolatedDefaults);
        NSString *output = argc > 1 ? [NSString stringWithUTF8String:argv[1]] : nil;
        int passed = 0, failures = 0;
        @try {
            CheckDegradedDriveInventory(); passed++;
            CheckDriveManagement(output); passed++;
            Class controllerClass = NSClassFromString(@"JasoSetupWindowController");
            Require(controllerClass != Nil, @"The native folder setup controller is not implemented");
            JasoSetupWindowController *controller = [[controllerClass alloc] init];
            if (output) Require([NSFileManager.defaultManager createDirectoryAtPath:output withIntermediateDirectories:YES attributes:nil error:NULL], @"Could not create setup rendering directory");
            Require(!Button(controller, @"setup-preview").enabled && !Button(controller, @"setup-save").enabled && !Button(controller, @"setup-start").enabled, @"Setup actions must wait for confirmed configuration");
            Require(Button(controller, @"setup-preview").enclosingScrollView == nil, @"The preview action must stay visible when the folder list scrolls");
            Require(![Button(controller, @"setup-more-settings").accessibilityValue boolValue], @"The optional saved mode must begin inside collapsed additional settings");
            __block NSUInteger reloads = 0, previews = 0, cancels = 0, saves = 0, closes = 0;
            __block NSDictionary *requestedDraft = nil;
            __block NSString *requestedRevision = nil;
            __block BOOL requestedStart = NO;
            controller.reloadHandler = ^{ reloads++; };
            controller.previewHandler = ^(NSDictionary *draft) { previews++; requestedDraft = draft; };
            controller.saveHandler = ^(NSDictionary *draft, NSString *revision, BOOL start) { saves++; requestedDraft = draft; requestedRevision = revision; requestedStart = start; };
            controller.cancelHandler = ^{ cancels++; };
            controller.closeHandler = ^{ closes++; };
            [controller updateConfiguration:nil error:@"Could not read /fixture/config.json"];
            Require([[(NSTextField *)Find(controller.window.contentView, @"setup-message") stringValue] containsString:@"/fixture/config.json"], @"Configuration error must expose the concrete path");
            [Button(controller, @"setup-reload") performClick:nil];
            Require(reloads == 1, @"Reload must ask the owner to read configuration");
            passed++;

            [controller updateConfiguration:Configuration(@"all-user-files", YES) error:nil];
            Require([controller.draft[@"scope"] isEqual:@"all-user-files"] && [controller.draft[@"excludes"] isEqual:@[@"/fixture/Downloads/Private"]] && [controller.draft[@"apply"] boolValue], @"Setup must preserve the existing broad scope, exclusions and automatic mode");
            Require(![Button(controller, @"setup-exclusions-toggle").accessibilityValue boolValue] && [Button(controller, @"setup-exclusions-toggle").title containsString:@"1"], @"Saved exclusions must begin collapsed with their count visible");
            Require(!Button(controller, @"setup-add-folder").enabled && !Button(controller, @"setup-start").enabled, @"Broad mode should use its saved scope; Start requires a preview");
            NSPopUpButton *scope = (NSPopUpButton *)Find(controller.window.contentView, @"setup-scope");
            NSPopUpButton *mode = (NSPopUpButton *)Find(controller.window.contentView, @"setup-mode");
            [scope.menu update]; [mode.menu update];
            NSArray *scopeEnabled = [scope.itemArray valueForKey:@"enabled"], *modeEnabled = [mode.itemArray valueForKey:@"enabled"];
            Require([scopeEnabled isEqual:@[@YES, @YES]] && [modeEnabled isEqual:@[@YES, @YES]], [NSString stringWithFormat:@"Setup choices must remain available after menu validation: scope=%@, mode=%@", scopeEnabled, modeEnabled]);
            Choose(scope, @"configured");
            Require([controller.draft[@"roots"] count] == 0 && Button(controller, @"setup-add-folder").enabled, @"A broad scope must let the user choose a focused folder list");
            [controller addFolderURLs:@[[NSURL fileURLWithPath:@"/fixture/Downloads" isDirectory:YES]] excluding:NO];
            [controller addFolderURLs:@[[NSURL fileURLWithPath:@"/fixture/Projects" isDirectory:YES], [NSURL fileURLWithPath:@"/fixture/Projects" isDirectory:YES]] excluding:NO];
            Require([controller.draft[@"roots"] isEqual:@[@"/fixture/Downloads", @"/fixture/Projects"]], @"Folder selection must add each directory once");
            Choose(scope, @"all-user-files");
            Require([controller.draft[@"roots"] count] == 0, @"The all-user-files payload must omit the cached selected roots required only by configured scope");
            Choose(scope, @"configured");
            Require([controller.draft[@"roots"] isEqual:@[@"/fixture/Downloads", @"/fixture/Projects"]], @"Returning to selected folders must restore the user's cached list");
            [controller addFolderURLs:@[[NSURL fileURLWithPath:@"/fixture/Projects/Archive" isDirectory:YES]] excluding:YES];
            Require([controller.draft[@"excludes"] containsObject:@"/fixture/Projects/Archive"], @"Exclusion selection must update the draft");
            passed++;

            [Button(controller, @"setup-preview") performClick:nil];
            Require(previews == 1 && [requestedDraft isEqual:controller.draft], @"Preview must submit the full editable draft");
            [scope.menu update]; [mode.menu update];
            Require(Button(controller, @"setup-cancel").enabled && !Button(controller, @"setup-save").enabled && !scope.enabled && !mode.enabled, @"Preview must expose cancellation and freeze editing after menu validation");
            [controller updatePreview:Preview() error:nil];
            Require(Button(controller, @"setup-start").enabled, @"A reviewed automatic draft must enable Start");
            NSTextField *before = (NSTextField *)Find(controller.window.contentView, @"setup-before-0");
            NSTextField *after = (NSTextField *)Find(controller.window.contentView, @"setup-after-0");
            Require(before.selectable && after.selectable && [before.stringValue isEqual:@"보고서.txt"] && [after.stringValue isEqual:@"보고서.txt"], @"Preview must show selectable original and normalized filenames");
            if (output) Render(controller, [output stringByAppendingPathComponent:@"setup-preview-result.png"]);
            NSClipView *previewViewport = before.enclosingScrollView.contentView;
            NSRect beforeRect = [previewViewport convertRect:before.bounds fromView:before], afterRect = [previewViewport convertRect:after.bounds fromView:after];
            Require(NSContainsRect(previewViewport.bounds, beforeRect) && NSContainsRect(previewViewport.bounds, afterRect), [NSString stringWithFormat:@"Completed preview must scroll names into viewport %@; document %@ (%@), before %@, after %@", NSStringFromRect(previewViewport.bounds), NSStringFromRect(previewViewport.documentView.frame), NSStringFromSize(previewViewport.documentView.fittingSize), NSStringFromRect(beforeRect), NSStringFromRect(afterRect)]);
            [Button(controller, @"setup-more-settings") performClick:nil];
            Require(!mode.hiddenOrHasHiddenAncestor, @"Opening additional settings must expose the saved mode without changing the draft");
            Choose(mode, @"preview");
            Require(Button(controller, @"setup-start").enabled && Find(controller.window.contentView, @"setup-before-0") && previews == 1, @"Changing only the saved mode must retain a read-only preview and allow Start");
            Choose(mode, @"automatic");
            passed++;

            [controller addFolderURLs:@[[NSURL fileURLWithPath:@"/fixture/Photos" isDirectory:YES]] excluding:NO];
            Require(!Button(controller, @"setup-start").enabled && !Find(controller.window.contentView, @"setup-before-0"), @"Editing folders must discard stale preview rows and disable Start");
            Require(!Button(controller, @"setup-save").enabled, @"Automatic Save must not bypass review for a changed draft");
            Choose(mode, @"preview");
            Require(Button(controller, @"setup-save").enabled && !Button(controller, @"setup-start").enabled, @"Preview mode can be saved without starting automatic work");
            [Button(controller, @"setup-save") performClick:nil];
            Require(saves == 1 && !requestedStart && [requestedRevision isEqual:@"fixture-revision"] && ![requestedDraft[@"apply"] boolValue] && !scope.enabled && !mode.enabled, @"Save must carry its revision, preserve the owner's lifecycle choice, and freeze editing");
            NSDictionary *draftAtFailure = controller.draft;
            [controller updateSave:nil error:@"Settings changed at /fixture/config.json. Reload settings."];
            Require([controller.draft isEqual:draftAtFailure] && Button(controller, @"setup-save").enabled, @"Failed save must preserve the draft and allow recovery");
            passed++;

            [Button(controller, @"setup-preview") performClick:nil];
            [Button(controller, @"setup-cancel") performClick:nil];
            Require(cancels == 1 && !Button(controller, @"setup-preview").enabled && !Button(controller, @"setup-save").enabled && !Button(controller, @"setup-reload").enabled && !scope.enabled && !mode.enabled, @"Cancel must wait for process acknowledgement before allowing conflicting work");
            Require([controller windowShouldClose:controller.window], @"The window can close while preview cancellation is pending");
            [controller setBusy:NO cancellable:NO];
            Require(Button(controller, @"setup-preview").enabled && Button(controller, @"setup-save").enabled && Button(controller, @"setup-reload").enabled && scope.enabled && mode.enabled, @"Cancellation acknowledgement must unlock the preserved draft");
            [controller updatePreview:Preview() error:nil];
            Require(!Find(controller.window.contentView, @"setup-before-0"), @"A cancelled preview response must not repopulate results");
            Choose(mode, @"automatic");
            [Button(controller, @"setup-preview") performClick:nil];
            NSMutableDictionary *partial = [Preview() mutableCopy];
            partial[@"errors"] = @[@{@"path":@"/fixture/Offline", @"error":@"Volume unavailable"}];
            partial[@"complete"] = @NO;
            [controller updatePreview:partial error:nil];
            Require(Button(controller, @"setup-start").enabled, @"A valid partial preview must allow available folders to start while preserving existing offline roots");
            NSTextField *previewError = (NSTextField *)Find(controller.window.contentView, @"setup-preview-error-0");
            Require(previewError.selectable && [previewError.stringValue containsString:@"/fixture/Offline"] && [previewError.stringValue containsString:@"Volume unavailable"], @"Preview errors must show a selectable path and cause");
            Require([previewError.stringValue containsString:@"Finder"] && ![previewError.stringValue containsString:@"Reconnect"], @"Actual preview errors must point to Finder and the reported cause rather than assume a disconnected folder");
            Require(![[(NSTextField *)Find(controller.window.contentView, @"setup-message") stringValue] containsString:@"Reconnect"], @"The partial-preview action guidance must not assume every error needs reconnection");
            passed++;

            [Button(controller, @"setup-preview") performClick:nil];
            NSMutableDictionary *timeLimited = [Preview() mutableCopy];
            timeLimited[@"complete"] = @NO; timeLimited[@"truncated"] = @YES; timeLimited[@"stop_reason"] = @"time_limit";
            timeLimited[@"interrupted_checks"] = @[@{@"path":@"/fixture/Desktop", @"error":@"Operation canceled (os error 89)"}];
            [controller updatePreview:timeLimited error:nil];
            for (NSString *language in @[@"en", @"ko"]) {
                JasoSetLanguagePreference(language); [controller reloadLocalization];
                NSString *summary = [(NSTextField *)Find(controller.window.contentView, @"setup-preview-summary") stringValue];
                Require([summary containsString:[language isEqual:@"ko"] ? @"미리보기 시간" : @"preview time limit"] && [summary containsString:[language isEqual:@"ko"] ? @"특정 폴더" : @"specific folder"], @"A preview deadline must explain the normal sampling limit and how to inspect a narrower folder");
                Require(Button(controller, @"setup-start").enabled && Find(controller.window.contentView, @"setup-before-0") && !Find(controller.window.contentView, @"setup-preview-error-0"), @"A time-limited preview must keep its sample and Start action without manufacturing a folder error");
                NSTextField *interrupted = (NSTextField *)Find(controller.window.contentView, @"setup-interrupted-check-0");
                NSString *interruptedHeading = [(NSTextField *)Find(controller.window.contentView, @"setup-interrupted-heading") stringValue];
                Require(interrupted.selectable && [interrupted.stringValue containsString:@"/fixture/Desktop"] && [interrupted.stringValue containsString:@"Operation canceled (os error 89)"] && ![interrupted.stringValue containsString:@"Reconnect"] && [interrupted.stringValue containsString:[language isEqual:@"ko"] ? @"특정 폴더를 선택해 미리보세요" : @"preview a specific folder"] && [interruptedHeading isEqual:[language isEqual:@"ko"] ? @"확인이 끝나지 않은 위치" : @"Locations not fully checked"], @"Interrupted checks must preserve a selectable path and raw diagnostic under a neutral localized heading and guidance");
                CheckLayout(controller.window.contentView, controller.window.contentView);
            }
            JasoSetLanguagePreference(@"en"); [controller reloadLocalization];
            passed++;

            [Button(controller, @"setup-preview") performClick:nil];
            NSMutableDictionary *sample = [Preview() mutableCopy]; sample[@"complete"] = @NO; sample[@"truncated"] = @YES;
            [controller updatePreview:sample error:nil];
            Require(Button(controller, @"setup-start").enabled && [[(NSTextField *)Find(controller.window.contentView, @"setup-preview-summary") stringValue].lowercaseString containsString:@"sample"], @"A bounded sample must be labeled and may enable Start when accessible");
            [Button(controller, @"setup-start") performClick:nil];
            Require(saves == 2 && requestedStart && [requestedDraft[@"apply"] boolValue] && !Button(controller, @"setup-cancel").enabled, @"Start must request automatic work once and keep save non-cancellable");
            [controller updateSave:@{@"config":requestedDraft, @"revision":@"next-revision", @"started":@YES, @"paused":@NO} error:nil];
            Require([controller.revision isEqual:@"next-revision"] && [controller.draft isEqual:requestedDraft], @"Successful save must use the confirmed draft and new revision");
            JasoSetLanguagePreference(@"ko"); [controller reloadLocalization];
            Require([[(NSTextField *)Find(controller.window.contentView, @"setup-message") stringValue] containsString:@"저장"], @"A saved-state notice must relocalize with the rest of the window");
            JasoSetLanguagePreference(@"en"); [controller reloadLocalization];
            [controller updateSave:@{@"config":requestedDraft, @"revision":@"next-revision", @"started":@YES, @"paused":@YES} error:nil];
            NSString *pausedNotice = [(NSTextField *)Find(controller.window.contentView, @"setup-message") stringValue];
            Require([pausedNotice containsString:@"Resume"] && ![pausedNotice containsString:@"Automatic cleanup has started"], @"A loaded but paused worker must be presented as paused");
            NSMutableDictionary *previewMode = [requestedDraft mutableCopy]; previewMode[@"apply"] = @NO;
            [controller updateSave:@{@"config":previewMode, @"revision":@"next-revision", @"started":@YES, @"paused":@NO} error:nil];
            Require(![[(NSTextField *)Find(controller.window.contentView, @"setup-message") stringValue] containsString:@"Automatic cleanup has started"], @"A running preview-mode worker must not be described as automatic cleanup");
            passed++;

            NSTableView *roots = (NSTableView *)Find(controller.window.contentView, @"setup-folders");
            [roots selectRowIndexes:[NSIndexSet indexSetWithIndex:1] byExtendingSelection:NO];
            [Button(controller, @"setup-remove-folder") performClick:nil];
            Require(![controller.draft[@"roots"] containsObject:@"/fixture/Projects"] && !Button(controller, @"setup-start").enabled, @"Removing the selected folder must invalidate review");
            [controller updateConfiguration:Configuration(@"configured", NO) error:nil];
            [Button(controller, @"setup-preview") performClick:nil]; [controller updatePreview:Preview() error:nil];
            [Button(controller, @"setup-start") performClick:nil];
            Require(requestedStart && [requestedDraft[@"apply"] boolValue] && [controller.draft[@"apply"] boolValue], @"Preview mode must support folders → preview → Start, with Start enabling automatic cleanup itself");
            [controller updateSave:@{@"config":requestedDraft, @"revision":@"fixture-revision", @"started":@YES, @"paused":@NO} error:nil];
            [controller updateConfiguration:Configuration(@"configured", NO) error:nil];
            [roots selectAll:nil]; [Button(controller, @"setup-remove-folder") performClick:nil];
            Require([controller.draft[@"roots"] count] == 0 && !Button(controller, @"setup-preview").enabled && Button(controller, @"setup-save").enabled, @"An empty selected-folder scope can be saved as idle without previewing");
            passed++;

            [controller updateConfiguration:Configuration(@"configured", YES) error:nil];
            [Button(controller, @"setup-preview") performClick:nil]; [controller updatePreview:Preview() error:nil];
            for (NSString *language in @[@"en", @"ko"]) {
                JasoSetLanguagePreference(language); [controller reloadLocalization];
                for (NSNumber *zoom in @[@0.8, @1.0, @2.0]) {
                    JasoSetContentZoom(zoom.doubleValue);
                    for (NSString *appearance in @[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) {
                        controller.window.appearance = [NSAppearance appearanceNamed:appearance];
                        [controller.window setContentSize:NSMakeSize(560, 640)];
                        [controller.window.contentView layoutSubtreeIfNeeded];
                        if (output) Render(controller, [output stringByAppendingPathComponent:[NSString stringWithFormat:@"setup-%@-%@-%.1f.png", language, [appearance isEqual:NSAppearanceNameAqua] ? @"light" : @"dark", zoom.doubleValue]]);
                        CheckLayout(controller.window.contentView, controller.window.contentView);
                        if (output && zoom.doubleValue == 1) {
                            JasoSetupWindowController *snapshot = [[controllerClass alloc] init];
                            snapshot.window.appearance = [NSAppearance appearanceNamed:appearance];
                            [snapshot.window setContentSize:NSMakeSize(560, 640)];
                            snapshot.previewHandler = ^(NSDictionary *draft) { (void)draft; };
                            [snapshot updateConfiguration:Configuration(@"configured", NO) error:nil]; [snapshot showWindow:nil];
                            NSString *suffix = [NSString stringWithFormat:@"%@-%@.png", language, [appearance isEqual:NSAppearanceNameAqua] ? @"light" : @"dark"];
                            Render(snapshot, [output stringByAppendingPathComponent:[@"setup-initial-" stringByAppendingString:suffix]]);
                            [Button(snapshot, @"setup-preview") performClick:nil]; [snapshot updatePreview:Preview() error:nil];
                            Render(snapshot, [output stringByAppendingPathComponent:[@"setup-review-" stringByAppendingString:suffix]]);
                            [snapshot close];
                        }
                        passed++;
                    }
                }
            }
            JasoSetContentZoom(1);
            [controller showWindow:nil]; [Button(controller, @"setup-preview") performClick:nil]; [controller close];
            Require(closes == 1 && cancels == 2, @"Closing during preview must cancel read-only work and notify the owner");
            passed++;
            __weak JasoSetupWindowController *released;
            @autoreleasepool { JasoSetupWindowController *temporary = [[controllerClass alloc] init]; released = temporary; [temporary close]; }
            Require(released == nil, @"Closed setup controllers must be releasable");
            passed++;
        } @catch (NSException *error) {
            fprintf(stderr, "FAIL: %s\n", error.reason.UTF8String); failures++;
        } @finally {
            method_setImplementation(method, original); [TestDefaults removePersistentDomainForName:suite]; TestDefaults = nil;
        }
        printf("%d setup window cases, %d failures\n", passed, failures);
        return failures ? 1 : 0;
    }
}
