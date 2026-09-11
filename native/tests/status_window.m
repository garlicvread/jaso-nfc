#import "../macos/StatusWindow.h"

static void Require(BOOL value, NSString *message) {
    if (!value) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static void Pump(void) { [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.15]]; }
static void Texts(NSView *view, NSMutableArray *texts) {
    if ([view isKindOfClass:NSTextField.class]) [texts addObject:[(NSTextField *)view stringValue]];
    for (NSView *child in view.subviews) Texts(child, texts);
}
static void IdentifiedViews(NSView *view, NSString *identifier, NSMutableArray *matches) {
    if ([view.identifier isEqual:identifier]) [matches addObject:view];
    for (NSView *child in view.subviews) IdentifiedViews(child, identifier, matches);
}
static NSArray *Views(NSView *view, NSString *identifier) {
    NSMutableArray *matches = [NSMutableArray array];
    IdentifiedViews(view, identifier, matches);
    return matches;
}
static void Render(JasoStatusWindowController *controller, NSString *path) {
    [controller.window.contentView layoutSubtreeIfNeeded];
    NSView *view = controller.window.contentView;
    NSBitmapImageRep *bitmap = [view bitmapImageRepForCachingDisplayInRect:view.bounds];
    [view cacheDisplayInRect:view.bounds toBitmapImageRep:bitmap];
    NSData *data = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
    Require(data.length > 1000 && [data writeToFile:path atomically:YES], @"Could not render the production status view");
}
int main(int argc, const char **argv) {
    @autoreleasepool {
        [NSApplication sharedApplication]; [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory]; [NSApp finishLaunching];
        NSString *output = argc > 1 ? [NSString stringWithUTF8String:argv[1]] : nil;
        if (output) [NSFileManager.defaultManager createDirectoryAtPath:output withIntermediateDirectories:YES attributes:nil error:NULL];
        NSDictionary *watching = @{@"running":@YES, @"paused":@NO, @"apply":@YES, @"baseline_complete":@YES,
            @"indexed_entries":@128450, @"indexed_directories":@18400, @"pending_jobs":@0, @"deferred_jobs":@0,
            @"deferred_renames":@0, @"errors":@0, @"renamed":@256,
            @"active_roots":@[@"/Users/example", @"/Users/Shared", @"/Volumes/Archive"],
            @"unavailable_roots":@{}, @"catalog_unavailable":@{}, @"pending_recovery":@NO, @"needs_revalidation":@NO};
        NSMutableDictionary *indexing = watching.mutableCopy;
        indexing[@"baseline_complete"] = @NO; indexing[@"pending_jobs"] = @2480;
        indexing[@"pending_baseline_roots"] = @[@"/Volumes/Archive"];
        NSMutableDictionary *paused = indexing.mutableCopy; paused[@"paused"] = @YES;
        NSMutableDictionary *warning = watching.mutableCopy;
        warning[@"deferred_renames"] = @3; warning[@"errors"] = @184;
        warning[@"unavailable_roots"] = @{@"/Users/another-account/Documents/An unusually long folder name used to check that status rows wrap without hiding the useful information":@"Permission denied (os error 13)"};
        warning[@"catalog_unavailable"] = @{@"volumes":@"Permission denied"};
        NSString *renamePath = @"/Users/example/Documents/A long project directory used to verify the full selectable file path wraps without hiding the affected filename/Permission review.txt";
        NSString *directoryPath = @"/Users/example/Library/CloudStorage/ExampleDrive/Syncing folder";
        NSNumber *futureRetry = @([NSDate.date timeIntervalSince1970] + 600);
        NSMutableDictionary *actionable = watching.mutableCopy;
        actionable[@"deferred_renames"] = @1;
        actionable[@"pending_jobs"] = @1;
        actionable[@"next_retry"] = futureRetry;
        actionable[@"directory_retry_count"] = @1;
        actionable[@"rename_retry_items"] = @[@{@"path":renamePath, @"reason":@"13", @"attempts":@2, @"next_retry":futureRetry, @"last_failure":@([NSDate.date timeIntervalSince1970] - 60), @"locked":@NO}];
        actionable[@"directory_retry_items"] = @[@{@"path":directoryPath, @"reason":@"Resource deadlock avoided (os error 11)", @"attempts":@2, @"next_retry":futureRetry}];
        NSString *authenticationPath = @"/Users/example/Library/CloudStorage/Nextcloud-example-account/Shared project folder with a long name to check that the account guidance and path remain readable/Authentication check";
        NSString *timeoutPath = @"/Users/example/Library/CloudStorage/GoogleDrive-personal-example/Personal project archive with a long name to check that the affected account is distinguishable/Response timeout";
        NSString *unknownPath = @"/Users/example/Documents/Provider returned no specific cause for this location/Unknown failure";
        NSArray *providerPaths = @[authenticationPath, timeoutPath, unknownPath];
        NSMutableDictionary *providerActions = watching.mutableCopy;
        providerActions[@"pending_jobs"] = @3;
        providerActions[@"directory_retry_count"] = @3;
        providerActions[@"next_retry"] = futureRetry;
        providerActions[@"directory_retry_items"] = @[
            @{@"path":authenticationPath, @"reason":@"Need authenticator (os error 81)", @"attempts":@7, @"next_retry":futureRetry},
            @{@"path":timeoutPath, @"reason":@"Operation timed out (os error 60)", @"attempts":@3, @"next_retry":futureRetry},
            @{@"path":unknownPath, @"reason":@"Provider returned an unspecified failure", @"attempts":@3, @"next_retry":futureRetry}];
        NSArray *fixtures = @[@{@"name":@"watching", @"snapshot":watching}, @{@"name":@"indexing", @"snapshot":indexing}, @{@"name":@"paused", @"snapshot":paused}, @{@"name":@"attention", @"snapshot":warning}, @{@"name":@"file-actions", @"snapshot":actionable}, @{@"name":@"provider-actions", @"snapshot":providerActions}];
        int passed = 0;
        @try {
            for (NSString *language in @[@"en", @"ko"]) {
                [NSUserDefaults.standardUserDefaults setVolatileDomain:@{@"interfaceLanguage":language, @"contentZoom":@1} forName:NSArgumentDomain];
            for (NSString *appearance in @[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) {
                for (NSDictionary *fixture in fixtures) {
                    JasoStatusWindowController *controller = [JasoStatusWindowController new];
                    controller.window.appearance = [NSAppearance appearanceNamed:appearance];
                    if ([fixture[@"name"] isEqual:@"provider-actions"]) {
                        NSRect frame = controller.window.frame;
                        frame.size.width = controller.window.minSize.width;
                        [controller.window setFrame:frame display:NO];
                    }
                    [controller updateSnapshot:fixture[@"snapshot"] error:nil updatedAt:NSDate.date];
                    NSMutableArray *texts = [NSMutableArray array]; Texts(controller.window.contentView, texts);
                    NSString *visible = [texts componentsJoinedByString:@"\n"];
                    Require(![visible containsString:@"baseline_complete"] && ![visible containsString:@"active_roots"] && ![visible containsString:@"{\n"], @"Technical JSON leaked into the status view");
                    Require([visible containsString:@"128,450"], @"Indexed count is missing from the actual window");
                    Require(!controller.refreshingAutomatically, @"A never-opened window must not poll");
                    if ([fixture[@"name"] isEqual:@"file-actions"]) {
                        Require([visible containsString:renamePath] && [visible containsString:directoryPath], @"Affected files and folders are absent from the status view");
                        Require([texts containsObject:renamePath.lastPathComponent] && [texts containsObject:directoryPath.lastPathComponent], @"Issue cards must identify the affected filenames separately from their full paths");
                        NSArray *paths = Views(controller.window.contentView, @"issue-path");
                        Require(paths.count == 2, @"Each issue must display a full selectable path");
                        for (NSTextField *path in paths) {
                            Require(path.selectable, @"An issue path cannot be selected or copied");
                            NSSize needed = [path.cell cellSizeForBounds:NSMakeRect(0, 0, path.bounds.size.width, CGFLOAT_MAX)];
                            Require(needed.height <= path.bounds.size.height + 1, @"A long affected path is clipped");
                        }
                        NSArray *summaries = Views(controller.window.contentView, @"issue-summary");
                        Require(summaries.count == 1 && [(NSTextField *)summaries.firstObject stringValue].length > 0, @"The issue section does not explain the next step");
                        NSMutableArray *revealed = [NSMutableArray array];
                        controller.pathHandler = ^(NSString *path) { [revealed addObject:path]; };
                        NSArray *buttons = Views(controller.window.contentView, @"issue-reveal");
                        Require(buttons.count == 2, @"Each revealable issue needs a Finder action");
                        for (NSButton *button in buttons) {
                            Require(button.title.length > 0 && button.enabled, @"A Finder action is disabled or unlabeled");
                            [button performClick:nil];
                        }
                        Require([revealed containsObject:renamePath] && [revealed containsObject:directoryPath] && revealed.count == 2, @"Finder actions lost or changed their affected paths");
                        NSUInteger pathIndex = [texts indexOfObject:renamePath];
                        NSUInteger locationsIndex = [texts indexOfObject:[language isEqual:@"ko"] ? @"감시 위치" : @"Watched locations"];
                        Require(pathIndex < locationsIndex, @"Actionable issues appear after the generic location list");
                    }
                    if ([fixture[@"name"] isEqual:@"provider-actions"]) {
                        BOOL korean = [language isEqual:@"ko"];
                        NSArray *expectedTitles = korean
                            ? @[@"계정 인증 확인 필요", @"실패 반복 · 마지막 확인 시간 초과", @"실패 반복 · 해당 위치 확인"]
                            : @[@"Check account authentication", @"Repeated failures — last check timed out", @"Repeated failure — check this location"];
                        for (NSString *title in expectedTitles) Require([texts containsObject:title], @"Provider failures must retain their distinct localized causes in the actual window");
                        Require(![visible containsString:@"Full Disk Access"] && ![visible containsString:@"전체 디스크 접근"], @"Provider failures cannot invent a privacy-permission diagnosis");
                        Require(controller.contentZoom == 1 && fabs(controller.window.frame.size.width - controller.window.minSize.width) < 1, @"Provider guidance must be checked at normal zoom and the minimum window width");
                        NSArray *paths = Views(controller.window.contentView, @"issue-path");
                        Require(paths.count == providerPaths.count, @"Every provider failure needs its own affected path");
                        for (NSTextField *path in paths) {
                            NSUInteger index = [providerPaths indexOfObject:path.stringValue];
                            Require(index != NSNotFound, @"Provider guidance changed the affected account or location path");
                            NSMutableArray *rowTexts = [NSMutableArray array]; Texts(path.superview, rowTexts);
                            NSString *rowText = [rowTexts componentsJoinedByString:@"\n"];
                            Require([rowTexts containsObject:expectedTitles[index]], @"Cause-specific guidance is attached to the wrong location");
                            if (index < 2) {
                                Require([rowText containsString:korean ? @"계정" : @"account"] && [rowText containsString:korean ? @"동기화 앱" : @"sync app"], @"Cloud failures need affected-account and sync-app guidance");
                            }
                            if (index == 1) {
                                BOOL uncertain = korean ? ([rowText containsString:@"알 수 없"] || [rowText containsString:@"단정"] || [rowText containsString:@"의미하지 않"] || [rowText containsString:@"확정"])
                                    : ([rowText containsString:@"does not identify"] || [rowText containsString:@"does not prove"] || [rowText containsString:@"does not confirm"] || [rowText containsString:@"unknown"]);
                                Require(uncertain, @"A Google Drive timeout must leave the account's current login state uncertain");
                            }
                            BOOL warningTitle = NO;
                            for (NSView *view in path.superview.subviews) {
                                if (![view isKindOfClass:NSTextField.class]) continue;
                                NSTextField *field = (NSTextField *)view;
                                if ([field.stringValue isEqual:expectedTitles[index]]) warningTitle = [field.textColor isEqual:NSColor.systemOrangeColor];
                                NSSize needed = [field.cell cellSizeForBounds:NSMakeRect(0, 0, field.bounds.size.width, CGFLOAT_MAX)];
                                Require(field.bounds.size.width > 0 && needed.height <= field.bounds.size.height + 1, @"Provider cause, path, or next-step guidance is clipped at minimum width");
                            }
                            Require(path.selectable && warningTitle, @"Repeated provider failures need readable selectable paths and a warning title");
                        }
                        NSMutableArray *revealed = [NSMutableArray array];
                        controller.pathHandler = ^(NSString *path) { [revealed addObject:path]; };
                        NSArray *buttons = Views(controller.window.contentView, @"issue-reveal");
                        Require(buttons.count == providerPaths.count, @"Each provider failure needs a Finder action");
                        for (NSButton *button in buttons) {
                            Require(button.enabled && [button.title containsString:@"Finder"], @"Provider Finder actions must remain available and clearly labeled");
                            [button performClick:nil];
                        }
                        Require([[NSSet setWithArray:revealed] isEqual:[NSSet setWithArray:providerPaths]], @"Provider Finder actions must keep the exact affected account paths");
                    }
                    if (output) Render(controller, [output stringByAppendingPathComponent:[NSString stringWithFormat:@"%@-%@-%@.png", language, fixture[@"name"], [appearance isEqual:NSAppearanceNameAqua] ? @"light" : @"dark"]]);
                    [controller close]; passed++;
                }
            }
            }
            NSMutableDictionary *invalidPaths = watching.mutableCopy;
            invalidPaths[@"deferred_renames"] = @2;
            invalidPaths[@"rename_retry_items"] = @[@{@"path":@"relative/file.txt", @"reason":@"13", @"attempts":@1, @"next_retry":futureRetry, @"locked":@NO},
                @{@"path":[NSString stringWithFormat:@"/tmp/file%C.txt", (unichar)0], @"reason":@"13", @"attempts":@1, @"next_retry":futureRetry, @"locked":@NO}];
            JasoStatusWindowController *invalidController = [JasoStatusWindowController new];
            [invalidController updateSnapshot:invalidPaths error:nil updatedAt:NSDate.date];
            Require(Views(invalidController.window.contentView, @"issue-reveal").count == 0, @"Invalid filesystem paths must not expose Finder actions");
            [invalidController close]; passed++;
            JasoStatusWindowController *controller = [JasoStatusWindowController new];
            __block int refreshes = 0;
            controller.refreshHandler = ^{ refreshes++; };
            [controller updateSnapshot:watching error:nil updatedAt:NSDate.date];
            NSMutableArray *maintenance = [NSMutableArray array];
            controller.actionHandler = ^(NSString *action) { [maintenance addObject:action]; };
            for (NSString *identifier in @[@"restart", @"stop", @"reconcile", @"history"]) {
                NSArray *buttons = Views(controller.window.contentView, identifier);
                Require(buttons.count == 1 && [(NSButton *)buttons.firstObject isEnabled], @"Status must expose worker maintenance and recovery actions");
                [(NSButton *)buttons.firstObject performClick:nil];
            }
            Require([maintenance isEqual:@[@"restart", @"stop", @"reconcile", @"history"]], @"Maintenance actions routed incorrectly");
            [controller setRefreshing:YES];
            Require(![(NSButton *)Views(controller.window.contentView, @"restart").firstObject isEnabled], @"Busy status must disable worker maintenance");
            [controller setRefreshing:NO];
            [controller showWindow:nil]; Pump();
            Require(controller.refreshingAutomatically, @"A visible status window should schedule refresh");
            [controller.window orderOut:nil]; Pump();
            Require(!controller.refreshingAutomatically, @"A hidden window must stop its refresh timer");
            [controller showWindow:nil]; Pump();
            Require(controller.refreshingAutomatically, @"Reopening should restore visible refresh");
            [controller close]; Pump();
            Require(!controller.refreshingAutomatically && refreshes == 0, @"Close must cancel refresh without another request");
            passed++;
            __weak JasoStatusWindowController *weakController;
            @autoreleasepool {
                JasoStatusWindowController *temporary = [JasoStatusWindowController new]; weakController = temporary;
                [temporary updateSnapshot:warning error:@"Cannot read status" updatedAt:NSDate.date];
                [temporary close];
            }
            Require(weakController == nil, @"A closed status controller must be releasable"); passed++;
            printf("%d status window cases, 0 failures\n", passed);
        } @catch (NSException *error) {
            fprintf(stderr, "FAIL: %s\n", error.reason.UTF8String); return 1;
        }
    }
    return 0;
}
