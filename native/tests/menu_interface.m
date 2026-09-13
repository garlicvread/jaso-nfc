// Exercise the production menu/window wiring without launching the worker.
#define main JasoProductionMenuMain
#import "../macos/Menu.m"
#undef main
#import <objc/runtime.h>

static NSUserDefaults *TestDefaults;
static id IsolatedDefaults(id self, SEL selector) { (void)self; (void)selector; return TestDefaults; }
static void Require(BOOL condition, NSString *message) {
    if (!condition) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSView *FindView(NSView *view, NSString *identifier) {
    if ([view.identifier isEqual:identifier]) return view;
    for (NSView *child in view.subviews) {
        NSView *match = FindView(child, identifier);
        if (match) return match;
    }
    return nil;
}
static BOOL ContainsText(NSView *view, NSString *text) {
    if ([view isKindOfClass:NSTextField.class] && [[(NSTextField *)view stringValue] isEqual:text]) return YES;
    for (NSView *child in view.subviews) if (ContainsText(child, text)) return YES;
    return NO;
}
static void RejectExternalWorkspaceAction(id workspace, SEL selector, id value) {
    (void)workspace; (void)value;
    Require(NO, [NSString stringWithFormat:@"Interface tests must not open external apps: %@", NSStringFromSelector(selector)]);
}
static BOOL RejectExternalURL(id workspace, SEL selector, NSURL *url) {
    RejectExternalWorkspaceAction(workspace, selector, url);
    return NO;
}
static NSMenuItem *FindAction(NSMenu *menu, SEL selector) {
    for (NSMenuItem *item in menu.itemArray) if (item.action == selector) return item;
    return nil;
}
static NSMenu *ZoomMenu(void) {
    for (NSMenuItem *item in NSApp.mainMenu.itemArray)
        if (FindAction(item.submenu, NSSelectorFromString(@"zoomIn:"))) return item.submenu;
    return nil;
}
static NSTimeInterval ActivationTimeout = 2;
static BOOL ZoomKey(NSWindow *window, NSString *characters, NSEventModifierFlags modifiers, unsigned short keyCode) {
    // Activation and preceding window-close events arrive asynchronously. Wait
    // for the owned window to become key before testing its responder chain.
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:ActivationTimeout];
    do {
        [NSRunningApplication.currentApplication activateWithOptions:NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps];
        [NSApp activateIgnoringOtherApps:YES];
        [window makeKeyAndOrderFront:nil];
        [window makeKeyWindow];
        NSDate *settle = [NSDate dateWithTimeIntervalSinceNow:.05];
        NSEvent *pending;
        while ((pending = [NSApp nextEventMatchingMask:NSEventMaskAny untilDate:settle inMode:NSDefaultRunLoopMode dequeue:YES])) [NSApp sendEvent:pending];
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.02]];
        if (NSApp.keyWindow == window) break;
    } while (deadline.timeIntervalSinceNow > 0);
    Require(NSApp.keyWindow == window, [NSString stringWithFormat:@"The shortcut fixture window must be active (app=%d hidden=%d visible=%d eligible=%d key=%@ wanted=%@)", NSApp.active, NSApp.hidden, window.visible, window.canBecomeKeyWindow, NSApp.keyWindow, window]);
    NSEvent *event = [NSEvent keyEventWithType:NSEventTypeKeyDown location:NSZeroPoint modifierFlags:modifiers
        timestamp:NSProcessInfo.processInfo.systemUptime windowNumber:window.windowNumber context:nil
        characters:characters charactersIgnoringModifiers:characters isARepeat:NO keyCode:keyCode];
    return [NSApp.mainMenu performKeyEquivalent:event];
}

@interface InterfaceMenuFixture : JasoMenu
@property NSUInteger refreshCalls;
@property NSUInteger languageChanges;
@property NSMutableArray *permissionsRequests;
@property NSMutableArray *revealedPaths;
@property BOOL allowStop;
@property NSString *stopError;
@property NSDictionary *stopResponse;
@property NSMutableArray<NSArray<NSString *> *> *commands;
@property NSString *lastAlert;
@property dispatch_semaphore_t operationStarted;
@property dispatch_semaphore_t operationRelease;
@property BOOL recordCommands;
@property NSDictionary *setupReplies;
@end
@implementation InterfaceMenuFixture
- (void)requestWorkspace:(NSString *)request parameters:(NSDictionary *)parameters reply:(JasoWorkspaceReply)reply {
    if ([request isEqual:@"activity"]) reply(@{@"activity":@{@"phase":@"idle"}, @"recent_events":@[], @"generation":@"fixture", @"available":@YES}, nil);
    else if ([request isEqual:@"history"]) reply(@{@"records":@[], @"total":@0}, nil);
    else reply(@{}, nil);
}
- (void)refresh { self.refreshCalls++; }
- (void)run:(NSArray *)arguments { if (self.recordCommands) [self.commands addObject:arguments]; else [super run:arguments]; }
- (void)languageChanged { self.languageChanges++; [super languageChanged]; }
- (void)openLoginItems:(id)sender { [self.permissionsRequests addObject:@"login-items"]; }
- (void)openSystemSettings:(NSString *)url {
    Require([url hasSuffix:@"Privacy_AllFiles"], @"The Full Disk Access action must open its settings pane");
    [self.permissionsRequests addObject:@"full-disk-access"];
}
- (void)revealPath:(NSString *)path {
    [self.permissionsRequests addObject:@"reveal-app"];
    [self.revealedPaths addObject:path];
}
- (void)alert:(NSString *)title text:(NSString *)text { self.lastAlert = text; }
- (NSDictionary *)execute:(NSArray<NSString *> *)arguments error:(NSString **)error {
    if (self.setupReplies && [arguments.firstObject isEqual:@"setup"]) {
        [self.commands addObject:arguments];
        if ([arguments[1] isEqual:@"preview"] && self.operationStarted) {
            dispatch_semaphore_signal(self.operationStarted);
            dispatch_semaphore_wait(self.operationRelease, dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC));
        }
        return self.setupReplies[arguments[1]];
    }
    if (self.operationStarted && [arguments isEqual:@[@"test-operation"]]) {
        [self.commands addObject:arguments];
        dispatch_semaphore_signal(self.operationStarted);
        dispatch_semaphore_wait(self.operationRelease, dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC));
        return @{@"done":@YES};
    }
    if (self.allowStop && [arguments isEqual:@[@"stop"]]) {
        [self.commands addObject:arguments];
        if (error) *error = self.stopError;
        return self.stopResponse;
    }
    @throw [NSException exceptionWithName:@"TestFailure" reason:@"The interface test attempted to execute a worker command" userInfo:nil];
}
@end

@interface WorkspaceRequestFixture : JasoMenu
@property NSMutableArray<NSArray<NSString *> *> *commands;
@end
@implementation WorkspaceRequestFixture
- (NSDictionary *)execute:(NSArray<NSString *> *)arguments error:(NSString **)error { [self.commands addObject:arguments]; return @{@"ok":@YES}; }
@end

@interface StartupMenuFixture : JasoMenu
@property NSDictionary *statusReply;
@property NSDictionary *startupReply;
@end
@implementation StartupMenuFixture
- (void)requestWorkspace:(NSString *)request parameters:(NSDictionary *)parameters reply:(JasoWorkspaceReply)reply { reply(@{@"records":@[], @"total":@0}, nil); }
- (NSDictionary *)execute:(NSArray<NSString *> *)arguments error:(NSString **)error {
    if ([arguments.firstObject isEqual:@"status"]) return self.statusReply;
    if ([arguments isEqual:@[@"startup", @"status"]]) return self.startupReply;
    @throw [NSException exceptionWithName:@"TestFailure" reason:@"Startup fixture attempted a mutating command" userInfo:nil];
}
@end
static void WaitForMenuIdle(JasoMenu *menu) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:2];
    while ((menu.busy || menu.setupBusy) && deadline.timeIntervalSinceNow > 0) [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.01]];
    Require(!menu.busy && !menu.setupBusy, @"Fixture status refresh or setup did not finish");
}

@interface TerminationReplyFixture : NSObject
@property BOOL replied;
@property BOOL allowed;
@end
@implementation TerminationReplyFixture
- (void)replyToApplicationShouldTerminate:(BOOL)allowed { self.replied = YES; self.allowed = allowed; }
@end
static BOOL WaitForReply(TerminationReplyFixture *application) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:2];
    while (!application.replied && deadline.timeIntervalSinceNow > 0)
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.01]];
    return application.replied;
}

static NSUInteger Cases;
static NSUInteger Failures;
static void Test(NSString *name, void (^body)(void)) {
    Cases++;
    @try { body(); printf("PASS %s\n", name.UTF8String); }
    @catch (NSException *error) { Failures++; fprintf(stderr, "FAIL %s: %s\n", name.UTF8String, error.reason.UTF8String); }
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc > 1 && strcmp(argv[1], "--wait-for-activation") == 0) ActivationTimeout = 120;
        [NSApplication sharedApplication];
        [NSApp finishLaunching];
        NSApplicationActivationPolicy originalPolicy = NSApp.activationPolicy;
        NSMenu *originalMainMenu = NSApp.mainMenu;
        NSMenu *originalWindowsMenu = NSApp.windowsMenu;
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSString *suite = [@"jaso-nfc.menu-interface-test." stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults = [[NSUserDefaults alloc] initWithSuiteName:suite];
        [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"];
        [TestDefaults setBool:NO forKey:@"animateMark"];
        Method method = class_getClassMethod(NSUserDefaults.class, @selector(standardUserDefaults));
        IMP original = method_setImplementation(method, (IMP)IsolatedDefaults);
        Method openURL = class_getInstanceMethod(NSWorkspace.class, @selector(openURL:));
        IMP originalOpenURL = method_setImplementation(openURL, (IMP)RejectExternalURL);
        Method reveal = class_getInstanceMethod(NSWorkspace.class, @selector(activateFileViewerSelectingURLs:));
        IMP originalReveal = method_setImplementation(reveal, (IMP)RejectExternalWorkspaceAction);
        InterfaceMenuFixture *fixture = [InterfaceMenuFixture new];
        NSDictionary *snapshot = @{@"running":@YES, @"paused":@NO, @"apply":@YES, @"baseline_complete":@YES,
            @"indexed_entries":@128450, @"indexed_directories":@18400, @"pending_jobs":@0, @"deferred_jobs":@0,
            @"deferred_renames":@0, @"errors":@0, @"renamed":@256,
            @"active_roots":@[@"/Users/example"], @"unavailable_roots":@{}, @"catalog_unavailable":@{}, @"pending_recovery":@NO, @"needs_revalidation":@NO};
        @try {
            fixture.snapshot = snapshot;
            fixture.snapshotDate = NSDate.date;
            [fixture rebuildMenu];
            Test(@"menu provides folder setup without requiring worker status", ^{
                NSMenuItem *folders = FindAction(fixture.menu, NSSelectorFromString(@"setup:"));
                Require(folders != nil && folders.target == fixture, @"The menu must expose in-app folder setup");
                fixture.statusError = @"Worker is unavailable";
                [fixture.menu update];
                Require(folders.enabled, @"Folder setup must remain available while a stopped or misconfigured worker is being fixed");
                fixture.statusError = nil;
            });
            Test(@"native application menu exposes standard commands and shortcuts", ^{
                Require(NSApp.mainMenu.numberOfItems >= 2, @"No native application menu is installed");
                NSMenuItem *application = [NSApp.mainMenu itemAtIndex:0];
                Require([application.title isEqual:@"Jaso NFC"] && [application.submenu.title isEqual:@"Jaso NFC"], @"The application menu is not named Jaso NFC");
                NSMenu *menu = application.submenu;
                Require([FindAction(menu, @selector(about:)).title isEqual:@"About Jaso NFC"], @"The application menu has no About action");
                NSMenuItem *settings = FindAction(menu, @selector(settings:));
                Require(settings.target == fixture && [settings.keyEquivalent isEqual:@","] && settings.keyEquivalentModifierMask == NSEventModifierFlagCommand, @"Settings must use Command-comma");
                NSMenuItem *hide = FindAction(menu, @selector(hide:));
                Require(hide.target == NSApp && [hide.keyEquivalent isEqual:@"h"], @"The application menu lacks standard Hide");
                NSMenuItem *hideOthers = FindAction(menu, @selector(hideOtherApplications:));
                Require(hideOthers.target == NSApp && [hideOthers.keyEquivalent isEqual:@"h"] && hideOthers.keyEquivalentModifierMask == (NSEventModifierFlagCommand | NSEventModifierFlagOption), @"Hide Others must use Option-Command-H");
                Require(FindAction(menu, @selector(unhideAllApplications:)).target == NSApp, @"The application menu lacks Show All");
                NSMenuItem *quit = FindAction(menu, @selector(quit:));
                Require(quit.target == fixture && [quit.keyEquivalent isEqual:@"q"] && [quit.title isEqual:@"Quit Jaso NFC"], @"Quit must describe application shutdown");
                Require(NSApp.windowsMenu != nil && [NSApp.windowsMenu.title isEqual:@"Window"], @"No standard Window menu is installed");
                NSMenuItem *close = FindAction(NSApp.windowsMenu, @selector(performClose:));
                NSMenuItem *minimize = FindAction(NSApp.windowsMenu, @selector(performMiniaturize:));
                Require(close != nil && close.target == nil && [close.keyEquivalent isEqual:@"w"], @"Close must use Command-W through the responder chain");
                Require(minimize != nil && minimize.target == nil && [minimize.keyEquivalent isEqual:@"m"], @"Minimize must use Command-M through the responder chain");
                Require(FindAction(NSApp.windowsMenu, @selector(details:)).target == fixture, @"The Window menu lacks Index status");
                Require(NSApp.activationPolicy == NSApplicationActivationPolicyRegular, @"The app should remain visible to native application management without an open window");
            });
            Test(@"folder setup uses the active configuration and dedicated commands", ^{
                InterfaceMenuFixture *menu = [InterfaceMenuFixture new];
                menu.configPath = @"/fixture/custom settings/config.json";
                menu.commands = [NSMutableArray array];
                NSDictionary *draft = @{@"scope":@"configured", @"roots":@[@"/fixture/한글 ' files"], @"excludes":@[], @"apply":@YES};
                NSDictionary *read = @{@"config":draft, @"revision":@"fixture-revision", @"running":@NO, @"paused":@YES};
                menu.setupReplies = @{@"read":read, @"preview":@{@"entries":@1, @"candidates":@[], @"errors":@[], @"truncated":@NO, @"complete":@YES}, @"save":@{@"config":draft, @"revision":@"saved-revision", @"started":@YES, @"paused":@NO}};
                [menu setup:nil]; WaitForMenuIdle(menu);
                Require(menu.statusWindow.window.visible && !menu.setupWindow.window.visible && [menu.setupWindow.revision isEqual:@"fixture-revision"], @"Setup did not load the saved configuration into the real window");
                menu.setupWindow.previewHandler(draft); WaitForMenuIdle(menu);
                menu.setupWindow.saveHandler(draft, @"fixture-revision", YES); WaitForMenuIdle(menu);
                Require(menu.commands.count == 3, @"The setup flow must read, preview, then save through its dedicated endpoint");
                for (NSArray *arguments in menu.commands) {
                    Require([arguments.firstObject isEqual:@"setup"] && [arguments[3] isEqual:menu.configPath], @"Setup lost the registered custom configuration path");
                    NSUInteger position = [arguments indexOfObject:@"--draft"];
                    if (position != NSNotFound) {
                        NSDictionary *encoded = [NSJSONSerialization JSONObjectWithData:[arguments[position + 1] dataUsingEncoding:NSUTF8StringEncoding] options:0 error:NULL];
                        Require([encoded isEqual:draft], @"Folder paths must reach the backend literally through JSON arguments");
                    }
                }
                Require([menu.commands.lastObject containsObject:@"--start"] && [menu.commands.lastObject containsObject:@"fixture-revision"], @"Starting cleanup must include explicit intent and the loaded revision");
                [menu.statusWindow close];
                Require(menu.setupWindow != nil && !menu.statusWindow.window.visible, @"Closing the shared window must retain the folder draft for reopening");
            });
            Test(@"workspace queries preserve literal arguments and the registered configuration", ^{
                WorkspaceRequestFixture *menu = WorkspaceRequestFixture.new;
                menu.configPath = @"/fixture/custom settings/config.json"; menu.commands = NSMutableArray.new;
                NSString *query = @"보고서 ' $(touch never) --config";
                NSArray *requests = @[
                    @{@"name":@"history", @"parameters":@{@"search":query, @"offset":@12, @"limit":@24}, @"arguments":@[@"history",@"list",@"--search",query,@"--offset",@"12",@"--limit",@"24"]},
                    @{@"name":@"activity", @"parameters":@{}, @"arguments":@[@"activity"]},
                    @{@"name":@"storage", @"parameters":@{}, @"arguments":@[@"storage"]},
                    @{@"name":@"history-preview", @"parameters":@{@"id":query,@"revision":@"revision"}, @"arguments":@[@"history",@"preview",@"--id",query,@"--revision",@"revision"]},
                    @{@"name":@"history-restore", @"parameters":@{@"request_id":query,@"operation_id":@"operation",@"revision":@"revision"}, @"arguments":@[@"history",@"restore",@"--request-id",query,@"--operation-id",@"operation",@"--revision",@"revision"]},
                    @{@"name":@"history-result", @"parameters":@{@"request_id":query}, @"arguments":@[@"history",@"result",@"--request-id",query]}
                ];
                for (NSDictionary *request in requests) {
                    dispatch_semaphore_t finished = dispatch_semaphore_create(0);
                    [menu requestWorkspace:request[@"name"] parameters:request[@"parameters"] reply:^(NSDictionary *result, NSString *error) { dispatch_semaphore_signal(finished); }];
                    Require(dispatch_semaphore_wait(finished, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0, @"The request must finish through the bounded command fixture");
                    NSArray *expected = [request[@"arguments"] arrayByAddingObjectsFromArray:@[@"--config",menu.configPath]];
                    Require([menu.commands.lastObject isEqual:expected], @"A workspace query must retain every literal argument and custom config path");
                }
            });
            Test(@"reopening the shared window after preview cancellation preserves the folder draft", ^{
                InterfaceMenuFixture *menu = [InterfaceMenuFixture new];
                menu.configPath = @"/fixture/reopen/config.json";
                menu.commands = [NSMutableArray array];
                NSDictionary *draft = @{@"scope":@"configured", @"roots":@[@"/fixture/Downloads"], @"excludes":@[], @"apply":@NO};
                menu.setupReplies = @{@"read":@{@"config":draft, @"revision":@"reopen-revision", @"running":@NO, @"paused":@NO}, @"preview":@{@"entries":@0, @"candidates":@[], @"errors":@[], @"complete":@YES}};
                [menu setup:nil]; WaitForMenuIdle(menu);
                menu.operationStarted = dispatch_semaphore_create(0);
                menu.operationRelease = dispatch_semaphore_create(0);
                menu.setupWindow.previewHandler(draft);
                Require(dispatch_semaphore_wait(menu.operationStarted, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0, @"The preview command must be in progress before the close/reopen test");
                [menu.statusWindow close];
                [menu setup:nil];
                dispatch_semaphore_signal(menu.operationRelease);
                WaitForMenuIdle(menu);
                Require([menu.setupWindow.revision isEqual:@"reopen-revision"], @"Reopening the shared window must retain its loaded configuration after cancellation");
                [menu.statusWindow close];
            });
            Test(@"cancelled preview releases controls only after its command finishes", ^{
                InterfaceMenuFixture *menu = [InterfaceMenuFixture new];
                menu.configPath = @"/fixture/cancel/config.json"; menu.commands = [NSMutableArray array];
                NSDictionary *draft = @{@"scope":@"configured", @"roots":@[@"/fixture/Downloads"], @"excludes":@[], @"apply":@NO};
                menu.setupReplies = @{@"read":@{@"config":draft, @"revision":@"cancel-revision", @"running":@NO, @"paused":@NO}, @"preview":@{@"entries":@0, @"candidates":@[], @"errors":@[], @"complete":@YES}};
                [menu setup:nil]; WaitForMenuIdle(menu);
                menu.operationStarted = dispatch_semaphore_create(0); menu.operationRelease = dispatch_semaphore_create(0);
                NSButton *preview = (NSButton *)FindView(menu.statusWindow.window.contentView, @"setup-preview");
                NSButton *cancel = (NSButton *)FindView(menu.statusWindow.window.contentView, @"setup-cancel");
                NSButton *reload = (NSButton *)FindView(menu.statusWindow.window.contentView, @"setup-reload");
                NSButton *save = (NSButton *)FindView(menu.statusWindow.window.contentView, @"setup-save");
                [preview performClick:nil];
                Require(dispatch_semaphore_wait(menu.operationStarted, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0, @"The preview fixture did not start");
                [cancel performClick:nil];
                BOOL safelyWaiting = !reload.enabled && !save.enabled && !preview.enabled;
                dispatch_semaphore_signal(menu.operationRelease); WaitForMenuIdle(menu);
                Require(safelyWaiting, @"Cancel must wait for command acknowledgement before enabling reload/save/preview");
                Require(reload.enabled && preview.enabled && save.enabled, @"Discarding a cancelled response must release the active window controls");
                [reload performClick:nil]; WaitForMenuIdle(menu);
                Require([menu.commands.lastObject[1] isEqual:@"read"] && reload.enabled, @"Reload after cancellation must finish without a stuck busy window");
                [menu.statusWindow close];
            });
            Test(@"menu has a readable summary and one explicit status entry", ^{
                [fixture.menu update];
                NSMenuItem *summary = fixture.menu.itemArray.firstObject;
                Require(summary.view != nil && summary.action == nil, @"Status must be readable content, not another status-window action");
                Require(ContainsText(summary.view, @"Automatic cleanup is on"), @"Summary must show the current readable status");
                NSUInteger links = 0;
                for (NSMenuItem *item in fixture.menu.itemArray) {
                    if (item.action == @selector(details:)) links++;
                    Require(item.action != @selector(toggleStartup:) && item.action != @selector(toggleAnimation:) && item.action != @selector(changeStyle:), @"Preferences must live in Settings");
                    Require(item.action != @selector(stop:) && item.action != @selector(reconcileAll:) && item.action != @selector(copyDiagnostics:), @"Maintenance must live in Status");
                }
                Require(links == 1, @"Menu must offer exactly one explicit Open Status action");
                NSMenuItem *entry = FindAction(fixture.menu, @selector(details:));
                Require(entry.enabled && [NSApp sendAction:entry.action to:entry.target from:entry], @"Open Status cannot be dispatched");
                Require(fixture.statusWindow.window.visible, @"Open Status did not open its window");
                [fixture.statusWindow close];
            });

            Test(@"primary menu control follows stopped paused and running states", ^{
                fixture.recordCommands = YES; fixture.commands = [NSMutableArray array]; fixture.configPath = @"/fixture/config.json";
                for (NSDictionary *state in @[@{@"running":@NO, @"paused":@NO, @"action":@"start"}, @{@"running":@YES, @"paused":@YES, @"action":@"resume"}, @{@"running":@YES, @"paused":@NO, @"action":@"pause"}]) {
                    NSMutableDictionary *value = snapshot.mutableCopy; value[@"running"] = state[@"running"]; value[@"paused"] = state[@"paused"];
                    fixture.snapshot = value; [fixture updateMenuSnapshot]; [fixture.menu update];
                    Require(!fixture.pauseItem.hidden && fixture.pauseItem.enabled, @"The current primary action must be available");
                    [NSApp sendAction:fixture.pauseItem.action to:fixture.pauseItem.target from:fixture.pauseItem];
                    Require([fixture.commands.lastObject.firstObject isEqual:state[@"action"]], @"The primary action did not follow worker state");
                }
                fixture.statusError = @"Status failed"; [fixture updateMenuSnapshot]; [fixture.menu update];
                Require(fixture.pauseItem.hidden, @"Failed status must not present a stale primary command");
                fixture.statusError = nil; fixture.snapshot = snapshot; [fixture updateMenuSnapshot];
                [fixture details:nil];
                [fixture settings:nil];
                [(NSButton *)FindView(fixture.statusWindow.window.contentView, @"settings-advanced-toggle") performClick:nil];
                for (NSString *action in @[@"restart", @"reconcile"]) {
                    NSButton *button = (NSButton *)FindView(fixture.statusWindow.window.contentView, action);
                    Require(button != nil && !button.hiddenOrHasHiddenAncestor, @"Maintenance must be available under Advanced settings");
                    [button performClick:nil];
                    Require([fixture.commands.lastObject.firstObject isEqual:action], @"Advanced maintenance did not reach its worker command");
                }
                [fixture stop:nil];
                Require([fixture.commands.lastObject.firstObject isEqual:@"stop"], @"Stop must preserve the existing worker command");
                [fixture.statusWindow close]; fixture.recordCommands = NO;
            });

            Test(@"View shortcuts zoom the active owned window once and reset", ^{
                NSMenu *viewMenu = ZoomMenu();
                Require([viewMenu.title isEqual:@"View"], @"The native application menu needs a localized View menu");
                Require(FindAction(viewMenu, NSSelectorFromString(@"zoomIn:")).target == nil && FindAction(viewMenu, NSSelectorFromString(@"zoomOut:")).target == nil, @"Zoom commands must use the active window responder chain");
                [fixture details:nil];
                Require(ZoomKey(fixture.statusWindow.window, @"=", NSEventModifierFlagCommand, 24), @"Command-equals did not dispatch");
                Require(fabs([TestDefaults doubleForKey:@"contentZoom"] - 1.1) < .001, @"Command-equals must apply one zoom step");
                Require(ZoomKey(fixture.statusWindow.window, @"+", NSEventModifierFlagCommand | NSEventModifierFlagShift, 24), @"Command-plus did not dispatch");
                Require(fabs([TestDefaults doubleForKey:@"contentZoom"] - 1.2) < .001, @"Command-plus must apply one zoom step");
                [fixture settings:nil];
                Require(ZoomKey(fixture.statusWindow.window, @"-", NSEventModifierFlagCommand, 27), @"Command-minus did not dispatch to Settings");
                Require(fabs([TestDefaults doubleForKey:@"contentZoom"] - 1.1) < .001, @"Command-minus must apply one step in Settings");
                Require(ZoomKey(fixture.statusWindow.window, @"0", NSEventModifierFlagCommand, 29), @"Command-zero did not dispatch");
                Require(fabs([TestDefaults doubleForKey:@"contentZoom"] - 1) < .001, @"Command-zero must reset content zoom");
                [fixture.statusWindow close];
                [viewMenu update];
                Require(!FindAction(viewMenu, NSSelectorFromString(@"zoomIn:")).enabled, @"Zoom must be disabled with no owned active window");
            });

            Test(@"Settings routes permissions actions without opening external applications in tests", ^{
                fixture.permissionsRequests = [NSMutableArray array];
                fixture.revealedPaths = [NSMutableArray array];
                [fixture settings:nil];
                NSButton *help = (NSButton *)FindView(fixture.statusWindow.window.contentView, @"settings-advanced-toggle");
                if (![help.accessibilityValue boolValue]) [help performClick:nil];
                NSArray *actions = @[@"login-items", @"full-disk-access", @"reveal-app"];
                for (NSString *identifier in actions) {
                    NSButton *button = (NSButton *)FindView(fixture.statusWindow.window.contentView, identifier);
                    Require([button isKindOfClass:NSButton.class], @"A permissions action is absent from the real Settings window");
                    [button performClick:nil];
                }
                Require([fixture.permissionsRequests isEqual:@[@"login-items", @"reveal-app", @"full-disk-access", @"reveal-app"]], @"Full Disk Access must reveal the actual app before opening settings");
                Require([fixture.revealedPaths isEqual:@[@"/Applications/Jaso NFC.app", @"/Applications/Jaso NFC.app"]], @"Permission actions must reveal the canonical Applications app");
                [fixture.statusWindow close];
            });

            Test(@"Settings language changes translate the shared window and preserve navigation", ^{
                fixture.snapshot = snapshot; [fixture details:nil];
                [fixture.statusWindow selectSection:@"status"];
                NSWindow *host = fixture.statusWindow.window;
                Require([host.title isEqual:@"Jaso NFC"], @"The shared window must keep its stable product title");
                Require(ContainsText(host.contentView, @"Automatic cleanup is on"), @"Status must use its stable primary heading");
                NSRect originalFrame = host.frame;
                [fixture settings:nil];
                Require(fixture.statusWindow.window == host && !fixture.settingsWindow.window.visible, @"Settings must use the same visible window");
                NSPopUpButton *picker = (id)FindView(host.contentView, @"interface-language-picker");
                Require(picker != nil, @"The embedded Settings view has no language picker");
                [picker selectItemAtIndex:1];
                Require([picker.selectedItem.representedObject isEqual:@"ko"], @"The Korean option has an incorrect value");
                Require([NSApp sendAction:picker.action to:picker.target from:picker], @"Language selection was not dispatched");
                Require(fixture.languageChanges == 1, @"Language changes must reach the menu once");
                Require([FindAction(fixture.menu, @selector(settings:)).title isEqual:@"설정…"], @"The menu did not translate");
                NSArray *sections = @[@"status", @"activity", @"folders", @"history", @"settings"];
                NSArray *names = @[@"상태", @"활동", @"폴더", @"변경 기록", @"설정"];
                for (NSUInteger i = 0; i < sections.count; i++) {
                    NSButton *nav = (id)FindView(host.contentView, [@"nav-" stringByAppendingString:sections[i]]);
                    Require([nav.title isEqual:names[i]], @"Every navigation section must translate");
                }
                Require([fixture.statusWindow.selectedSection isEqual:@"settings"] && ContainsText(host.contentView,@"언어"), @"Changing language must retain the current page and localize its controls");
                [(NSButton *)FindView(host.contentView, @"nav-status") performClick:nil];
                Require(ContainsText(host.contentView, @"자동 정리가 켜져 있습니다"), @"The shared Status heading must translate on return");
                Require(NSEqualRects(host.frame, originalFrame), @"Language changes must preserve the window frame");
                NSMenu *application = [NSApp.mainMenu itemAtIndex:0].submenu;
                Require([FindAction(application, @selector(settings:)).title isEqual:@"설정…"] && [FindAction(application, @selector(about:)).title isEqual:@"Jaso NFC 정보"], @"Application commands must translate");
                Require([FindAction(application, @selector(hide:)).title isEqual:@"Jaso NFC 가리기"] && [FindAction(application, @selector(unhideAllApplications:)).title isEqual:@"모두 보기"], @"Standard commands must translate");
                Require([NSApp.windowsMenu.title isEqual:@"창"] && [FindAction(NSApp.windowsMenu, @selector(performClose:)).title isEqual:@"닫기"], @"Window commands must translate");
                Require([ZoomMenu().title isEqual:@"보기"] && [FindAction(ZoomMenu(), NSSelectorFromString(@"zoomIn:")).title isEqual:@"확대"] && [FindAction(ZoomMenu(), NSSelectorFromString(@"resetZoom:")).title isEqual:@"실제 크기"], @"View commands must translate");
                Require(NSApp.activationPolicy == NSApplicationActivationPolicyRegular, @"Localization must preserve the application's menu");
            });

            Test(@"saved Korean language survives rebuilding the menu", ^{
                Require([[TestDefaults stringForKey:@"interfaceLanguage"] isEqual:@"ko"], @"Settings did not persist Korean");
                NSStatusItem *ownedItem = fixture.statusItem;
                [fixture rebuildMenu];
                [fixture.menu update];
                Require([JasoLanguagePreference() isEqual:@"ko"] && [FindAction(fixture.menu, @selector(settings:)).title isEqual:@"설정…"], @"Rebuilding the menu lost the saved language");
                Require(fixture.statusItem == ownedItem, @"Rebuilding allocated another status item");
                Require(fixture.menu.itemArray.firstObject.view != nil, @"Localized summary lost its readable view");
            });

            Test(@"null root collections and paused state do not crash the real menu projection", ^{
                NSMutableDictionary *malformed = snapshot.mutableCopy;
                malformed[@"active_roots"] = NSNull.null;
                malformed[@"unavailable_roots"] = NSNull.null;
                malformed[@"paused"] = NSNull.null;
                fixture.snapshot = malformed;
                [fixture updateMenuSnapshot];
                [fixture.menu update];
                Require(fixture.menu.itemArray.firstObject.view != nil && fixture.pauseItem.title.length, @"Malformed status left required menu labels empty");
                Require(!ContainsText(fixture.menu.itemArray.firstObject.view, @"null"), @"Raw null data leaked into a menu label");
                fixture.snapshot = snapshot;
                [fixture updateMenuSnapshot];
            });

            Test(@"closing the shared window retains navigation and stops automatic view refresh", ^{
                [fixture settings:nil];
                JasoWorkspaceWindowController *workspace = fixture.statusWindow;
                JasoSettingsWindowController *settings = fixture.settingsWindow;
                [workspace close];
                Require(fixture.statusWindow == workspace && fixture.settingsWindow == settings && !workspace.window.visible, @"A closed shared window retains navigation and embedded controllers");
                Require(!workspace.refreshingAutomatically, @"Closing the shared window must stop its automatic activity refresh");
                Require(NSApp.activationPolicy == NSApplicationActivationPolicyRegular, @"Closing the shared window must retain application visibility");
                Require(fixture.refreshCalls >= 2 && !fixture.busy, @"The test must exercise the bounded refresh path");
            });
            Test(@"Settings and Status use one retained window and menu item", ^{
                NSStatusItem *ownedItem = fixture.statusItem;
                JasoWorkspaceWindowController *workspace = fixture.statusWindow;
                [fixture settings:nil];
                Require(workspace.window.visible && [workspace.selectedSection isEqual:@"settings"], @"Settings must reopen its shared window");
                [(NSButton *)FindView(workspace.window.contentView,@"nav-status") performClick:nil];
                Require([workspace.selectedSection isEqual:@"status"] && !fixture.settingsWindow.window.visible, @"Navigation must switch the shared content without another window");
                [workspace close]; [fixture details:nil];
                Require(fixture.statusWindow == workspace && workspace.window.visible && [workspace.selectedSection isEqual:@"status"], @"Reopening must retain the last section");
                Require(fixture.statusItem == ownedItem && NSApp.activationPolicy == NSApplicationActivationPolicyRegular, @"Navigation must retain the native menu and status item");
                [workspace close];
            });
            Test(@"Finder reopen and status-only language changes retain regular activation", ^{
                Require([fixture applicationShouldHandleReopen:NSApp hasVisibleWindows:NO], @"Finder reopen was rejected");
                Require(fixture.statusWindow.window.visible && NSApp.activationPolicy == NSApplicationActivationPolicyRegular, @"Finder reopen did not open the status window with the application menu");
                JasoSetLanguagePreference(@"en");
                [fixture languageChanged];
                Require(fixture.statusWindow.window.visible && [fixture.statusWindow.window.title isEqual:@"Jaso NFC"] && NSApp.activationPolicy == NSApplicationActivationPolicyRegular, @"Status-only relocalization lost the window or application menu");
                [fixture.statusWindow close];
                Require(NSApp.activationPolicy == NSApplicationActivationPolicyRegular, @"Closing the reopened window did not retain regular app visibility");
            });
            Test(@"startup refresh does not treat missing or inconsistent jobs as off", ^{
                StartupMenuFixture *menu = [StartupMenuFixture new]; menu.configPath = @"/fixture/config.json"; menu.statusReply = snapshot;
                menu.settingsWindow = [JasoSettingsWindowController new];
                NSButton *startup = (NSButton *)FindView(menu.settingsWindow.window.contentView, @"run-at-login");
                for (NSDictionary *reply in @[@{@"installed":@NO, @"consistent":@YES, @"enabled":@NO}, @{@"installed":@YES, @"consistent":@NO, @"enabled":@NO}]) {
                    menu.startupReply = reply; menu.refreshStartup = YES; [menu refresh]; WaitForMenuIdle(menu);
                    Require(!startup.enabled && startup.state == NSControlStateValueMixed, @"Missing or inconsistent registrations must not be shown as editable startup off");
                }
                menu.startupReply = @{@"installed":@YES, @"consistent":@YES, @"enabled":@YES}; menu.refreshStartup = YES;
                [menu refresh]; WaitForMenuIdle(menu);
                Require(startup.enabled && startup.state == NSControlStateValueOn, @"Confirmed startup registration must be editable");
                [menu.settingsWindow close];
            });
            Test(@"application Quit confirms worker shutdown before allowing termination", ^{
                Require([fixture respondsToSelector:@selector(applicationShouldTerminate:)], @"Quit must wait for graceful worker shutdown");
                fixture.allowStop = YES; fixture.commands = [NSMutableArray array];
                fixture.stopResponse = @{@"stopped":@"io.github.garlicvread.jaso-nfc"};
                TerminationReplyFixture *application = [TerminationReplyFixture new];
                Require([fixture applicationShouldTerminate:(NSApplication *)application] == NSTerminateLater, @"Quit must defer its decision");
                Require(WaitForReply(application) && application.allowed, @"A confirmed stopped worker must allow termination");
                Require([fixture.commands isEqual:@[@[@"stop"]]], @"Quit must stop only the worker, without changing startup registration");
            });
            Test(@"Quit waits behind the current worker command", ^{
                fixture.allowStop = YES; fixture.commands = [NSMutableArray array];
                fixture.operationStarted = dispatch_semaphore_create(0); fixture.operationRelease = dispatch_semaphore_create(0);
                [fixture run:@[@"test-operation"]];
                Require(dispatch_semaphore_wait(fixture.operationStarted, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC)) == 0, @"The owned command fixture did not start");
                TerminationReplyFixture *application = [TerminationReplyFixture new];
                NSApplicationTerminateReply reply = [fixture applicationShouldTerminate:(NSApplication *)application];
                dispatch_semaphore_signal(fixture.operationRelease);
                Require(reply == NSTerminateLater, @"Quit should wait for an active operation instead of making the user quit again");
                Require(WaitForReply(application) && application.allowed, @"Queued shutdown was not confirmed");
                Require([fixture.commands isEqual:@[@[@"test-operation"], @[@"stop"]]], @"Shutdown must follow the current command");
                fixture.operationStarted = nil; fixture.operationRelease = nil;
            });
            Test(@"failed or unconfirmed worker shutdown keeps the application open", ^{
                Require([fixture respondsToSelector:@selector(applicationShouldTerminate:)], @"Quit must wait for graceful worker shutdown");
                for (NSDictionary *response in @[@{}, @{@"stopped":@"wrong-service"}]) {
                    fixture.stopResponse = response;
                    fixture.lastAlert = nil;
                    TerminationReplyFixture *application = [TerminationReplyFixture new];
                    Require([fixture applicationShouldTerminate:(NSApplication *)application] == NSTerminateLater, @"Quit must await worker confirmation");
                    Require(WaitForReply(application) && !application.allowed && fixture.lastAlert.length, @"Unconfirmed shutdown must remain visible and leave the app open");
                }
                fixture.stopError = @"Worker stop failed";
                fixture.stopResponse = nil;
                TerminationReplyFixture *application = [TerminationReplyFixture new];
                [fixture applicationShouldTerminate:(NSApplication *)application];
                Require(WaitForReply(application) && !application.allowed && [fixture.lastAlert isEqual:fixture.stopError], @"Stop errors must be visible and cancel termination");
                fixture.allowStop = NO;
            });
        } @catch (NSException *error) {
            Failures++;
            fprintf(stderr, "FAIL fixture setup: %s\n", error.reason.UTF8String);
        } @finally {
            [fixture.statusWindow close];
            [fixture.statusWindow close];
            fixture.mark.animationEnabled = NO;
            if (fixture.statusItem) [NSStatusBar.systemStatusBar removeStatusItem:fixture.statusItem];
            fixture.statusItem = nil;
            NSApp.windowsMenu = originalWindowsMenu;
            NSApp.mainMenu = originalMainMenu;
            [NSApp setActivationPolicy:originalPolicy];
            method_setImplementation(openURL, originalOpenURL);
            method_setImplementation(reveal, originalReveal);
            method_setImplementation(method, original);
            [TestDefaults removePersistentDomainForName:suite];
            TestDefaults = nil;
        }
        printf("%lu menu interface cases, %lu failures\n", (unsigned long)Cases, (unsigned long)Failures);
        return Failures ? 1 : 0;
    }
}
