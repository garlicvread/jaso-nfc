#import <AppKit/AppKit.h>
#import <sys/file.h>
#import <fcntl.h>
#import <libproc.h>
#import <sys/sysctl.h>
#import <signal.h>
#import <errno.h>
#import "AnimatedMark.h"
#import "StatusWindow.h"
#import "WorkspaceWindow.h"
#import "StatusPresentation.h"
#import "Localization.h"
#import "SettingsWindow.h"
#import "SetupWindow.h"

static BOOL JasoMenuError(NSString **error, NSString *message) {
    if (error) *error = message;
    return NO;
}
static BOOL JasoAbsolutePath(id value) {
    return [value isKindOfClass:NSString.class] && [value isAbsolutePath] &&
        [value rangeOfString:[NSString stringWithFormat:@"%C", (unichar)0]].location == NSNotFound;
}
static NSString *JasoMenuRegisteredConfig(NSString *registration, BOOL menu, BOOL *present, NSString **error) {
    NSError *readError = nil;
    NSData *data = [NSData dataWithContentsOfFile:registration options:0 error:&readError];
    *present = data != nil || !([readError.domain isEqual:NSCocoaErrorDomain] && readError.code == NSFileReadNoSuchFileError);
    if (!*present) return nil;
    if (!data) {
        JasoMenuError(error, readError.localizedDescription ?: @"Cannot read the installed login registration.");
        return nil;
    }
    id plist = [NSPropertyListSerialization propertyListWithData:data options:NSPropertyListImmutable format:NULL error:&readError];
    NSArray *registered = [plist isKindOfClass:NSDictionary.class] ? plist[@"ProgramArguments"] : nil;
    NSString *label = menu ? @"io.github.garlicvread.jaso-nfc.menu" : @"io.github.garlicvread.jaso-nfc";
    BOOL valid = [plist isKindOfClass:NSDictionary.class] && [plist[@"Label"] isEqual:label] && [registered isKindOfClass:NSArray.class];
    if (valid && menu) {
        valid = registered.count == 3 && JasoAbsolutePath(registered[0]) && [registered[1] isEqual:@"--config"] && JasoAbsolutePath(registered[2]);
    } else if (valid) {
        NSUInteger offset = registered.count == 5 ? 1 : 0;
        valid = (registered.count == 4 || registered.count == 5) && JasoAbsolutePath(registered[0]) &&
            (!offset || JasoAbsolutePath(registered[1])) &&
            ([registered[offset + 1] isEqual:@"watch"] || (!offset && [registered[offset + 1] isEqual:@"run"])) &&
            [registered[offset + 2] isEqual:@"--config"] && JasoAbsolutePath(registered[offset + 3]);
    }
    if (!valid) {
        JasoMenuError(error, @"An installed login registration has no valid configuration path. Reinstall Jaso NFC to repair it.");
        return nil;
    }
    return registered.lastObject;
}
static NSString *JasoMenuConfig(NSArray<NSString *> *args, NSString *registration, NSString *defaultConfig, NSString **error) {
    if (error) *error = nil;
    NSUInteger index = [args indexOfObject:@"--config"];
    if (index != NSNotFound) {
        if (index + 1 < args.count && JasoAbsolutePath(args[index + 1])) return args[index + 1];
        JasoMenuError(error, @"--config requires an absolute configuration path.");
        return nil;
    }
    BOOL menuPresent = NO, workerPresent = NO;
    NSString *menuConfig = JasoMenuRegisteredConfig(registration, YES, &menuPresent, error);
    if (menuPresent && !menuConfig) return nil;
    NSString *workerRegistration = [registration.stringByDeletingLastPathComponent stringByAppendingPathComponent:@"io.github.garlicvread.jaso-nfc.plist"];
    NSString *workerConfig = JasoMenuRegisteredConfig(workerRegistration, NO, &workerPresent, error);
    if (workerPresent && !workerConfig) return nil;
    if (menuConfig && workerConfig && ![[menuConfig.stringByStandardizingPath precomposedStringWithCanonicalMapping] isEqual:[workerConfig.stringByStandardizingPath precomposedStringWithCanonicalMapping]]) {
        JasoMenuError(error, @"The menu and worker registrations use different configuration files. Repair the registrations before opening Jaso NFC.");
        return nil;
    }
    return workerConfig ?: menuConfig ?: defaultConfig;
}
static NSString *JasoProcessPath(pid_t pid) {
    char path[PROC_PIDPATHINFO_MAXSIZE];
    if (proc_pidpath(pid, path, sizeof(path)) <= 0) return nil;
    return [NSFileManager.defaultManager stringWithFileSystemRepresentation:path length:strlen(path)].stringByResolvingSymlinksInPath;
}
static NSArray<NSString *> *JasoProcessArguments(pid_t pid, NSString **error) {
    int maximum = 0, sizeQuery[] = {CTL_KERN, KERN_ARGMAX};
    size_t size = sizeof(maximum);
    if (sysctl(sizeQuery, 2, &maximum, &size, NULL, 0) != 0 || maximum <= 0) {
        JasoMenuError(error, @"Cannot determine the process argument limit."); return nil;
    }
    NSMutableData *data = [NSMutableData dataWithLength:(NSUInteger)maximum];
    int query[] = {CTL_KERN, KERN_PROCARGS2, pid};
    size = data.length;
    if (sysctl(query, 3, data.mutableBytes, &size, NULL, 0) != 0 || size < sizeof(int)) {
        JasoMenuError(error, @"Cannot snapshot the previous menu arguments."); return nil;
    }
    int count = 0;
    memcpy(&count, data.bytes, sizeof(count));
    const char *cursor = (const char *)data.bytes + sizeof(count), *end = (const char *)data.bytes + size;
    // KERN_PROCARGS2 begins with argc, executable path, padding, then argv.
    while (cursor < end && *cursor) cursor++;
    while (cursor < end && !*cursor) cursor++;
    NSMutableArray *arguments = [NSMutableArray array];
    for (int i = 0; i < count && cursor < end; i++) {
        const char *nul = memchr(cursor, 0, (size_t)(end - cursor));
        if (!nul) break;
        NSString *argument = [[NSString alloc] initWithBytes:cursor length:(NSUInteger)(nul - cursor) encoding:NSUTF8StringEncoding];
        if (!argument) break;
        [arguments addObject:argument]; cursor = nul + 1;
    }
    if (count <= 0 || arguments.count != (NSUInteger)count) {
        JasoMenuError(error, @"The previous menu arguments could not be read exactly."); return nil;
    }
    return [arguments subarrayWithRange:NSMakeRange(1, arguments.count - 1)];
}
static NSDictionary *JasoMenuSnapshot(NSRunningApplication *app, NSArray<NSString *> *allowed, NSString **error) {
    NSString *executable = JasoProcessPath(app.processIdentifier);
    struct proc_bsdinfo process;
    BOOL owned = proc_pidinfo(app.processIdentifier, PROC_PIDTBSDINFO, 0, &process, sizeof(process)) == sizeof(process) && process.pbi_uid == getuid();
    BOOL accepted = NO;
    for (NSString *path in allowed) if (JasoAbsolutePath(path) && [path.stringByResolvingSymlinksInPath isEqual:executable]) accepted = YES;
    if (!owned || !accepted || app.terminated || app.processIdentifier == getpid()) {
        JasoMenuError(error, @"A running Jaso NFC menu could not be identified as the previous installation or requested source app. Quit that menu before installing.");
        return nil;
    }
    NSArray *arguments = JasoProcessArguments(app.processIdentifier, error);
    if (!arguments) return nil;
    // NSRunningApplication.launchDate can be absent for a directly opened menu.
    // Kernel birth time distinguishes PID reuse without relying on that field.
    return @{@"pid":@(app.processIdentifier), @"executable":executable, @"arguments":arguments,
             @"launched":@[@(process.pbi_start_tvsec), @(process.pbi_start_tvusec)]};
}
static BOOL JasoMenuValidSnapshot(NSDictionary *snapshot, NSString **error) {
    if (![snapshot isKindOfClass:NSDictionary.class] || ![snapshot[@"pid"] isKindOfClass:NSNumber.class] || [snapshot[@"pid"] intValue] <= 0 ||
        !JasoAbsolutePath(snapshot[@"executable"]) || ![snapshot[@"launched"] isKindOfClass:NSArray.class] || [snapshot[@"launched"] count] != 2 ||
        ![snapshot[@"arguments"] isKindOfClass:NSArray.class]) return JasoMenuError(error, @"Invalid menu handoff snapshot.");
    for (id value in snapshot[@"launched"]) if (![value isKindOfClass:NSNumber.class]) return JasoMenuError(error, @"Invalid menu process identity.");
    for (id argument in snapshot[@"arguments"]) if (![argument isKindOfClass:NSString.class]) return JasoMenuError(error, @"Invalid menu argument snapshot.");
    return YES;
}
static BOOL JasoMenuIdentity(NSRunningApplication *app, NSDictionary *snapshot) {
    struct proc_bsdinfo process;
    BOOL owned = proc_pidinfo(app.processIdentifier, PROC_PIDTBSDINFO, 0, &process, sizeof(process)) == sizeof(process) && process.pbi_uid == getuid();
    return app && !app.terminated && app.processIdentifier != getpid() &&
        [JasoProcessPath(app.processIdentifier) isEqual:snapshot[@"executable"]] &&
        owned && process.pbi_start_tvsec == [snapshot[@"launched"][0] unsignedLongLongValue] &&
        process.pbi_start_tvusec == [snapshot[@"launched"][1] unsignedLongLongValue];
}
static BOOL JasoMenuStop(NSDictionary *snapshot, NSString **error) {
    if (!JasoMenuValidSnapshot(snapshot, error)) return NO;
    pid_t pid = [snapshot[@"pid"] intValue];
    // launchd can finish bootout before Launch Services removes its cached app.
    // Only a kernel-confirmed missing PID counts as already stopped.
    if (kill(pid, 0) == -1 && errno == ESRCH) return YES;
    NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
    if (!app || app.terminated) return YES; // launchd may already have stopped it.
    if (!JasoMenuIdentity(app, snapshot)) return JasoMenuError(error, @"Menu process identity changed; refusing to stop it.");
    // Installer handoff closes only this already-verified GUI process. Normal
    // application termination also stops the worker and belongs to user Quit.
    if (kill(pid, SIGTERM) != 0 && errno != ESRCH) return JasoMenuError(error, @"Could not stop the previous menu.");
    NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 5;
    while (!(kill(pid, 0) == -1 && errno == ESRCH) && NSProcessInfo.processInfo.systemUptime < deadline) {
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    }
    if (!(kill(pid, 0) == -1 && errno == ESRCH)) return JasoMenuError(error, @"Timed out stopping the previous menu; installation was not activated.");
    return YES;
}
static BOOL JasoMenuRestore(NSDictionary *snapshot, NSString **error) {
    if (!JasoMenuValidSnapshot(snapshot, error)) return NO;
    NSRunningApplication *previous = [NSRunningApplication runningApplicationWithProcessIdentifier:[snapshot[@"pid"] intValue]];
    if (JasoMenuIdentity(previous, snapshot)) return YES;
    NSTask *task = [NSTask new];
    task.executableURL = [NSURL fileURLWithPath:snapshot[@"executable"]];
    task.arguments = snapshot[@"arguments"];
    task.standardInput = NSFileHandle.fileHandleWithNullDevice;
    task.standardOutput = NSFileHandle.fileHandleWithNullDevice;
    task.standardError = NSFileHandle.fileHandleWithNullDevice;
    NSError *launchError = nil;
    if (![task launchAndReturnError:&launchError]) return JasoMenuError(error, launchError.localizedDescription);
    NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 5;
    while (task.running && NSProcessInfo.processInfo.systemUptime < deadline) {
        NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:task.processIdentifier];
        if (app.finishedLaunching && [JasoProcessPath(app.processIdentifier) isEqual:snapshot[@"executable"]]) return YES;
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    }
    return JasoMenuError(error, @"Could not verify that the previous menu restarted.");
}
static int JasoMenuHandoff(NSArray<NSString *> *args) {
    NSString *error = nil;
    id input = args.count == 4 ? [NSJSONSerialization JSONObjectWithData:[args[3] dataUsingEncoding:NSUTF8StringEncoding] options:0 error:NULL] : nil;
    NSMutableArray *snapshots = [NSMutableArray array];
    if (![input isKindOfClass:NSArray.class]) error = @"Invalid menu handoff arguments.";
    else if ([args[2] isEqual:@"snapshot"]) {
        for (NSRunningApplication *app in [NSRunningApplication runningApplicationsWithBundleIdentifier:@"io.github.garlicvread.jaso-nfc"]) {
            if (app.processIdentifier == getpid() || app.terminated) continue;
            NSDictionary *snapshot = JasoMenuSnapshot(app, input, &error);
            if (!snapshot) break;
            [snapshots addObject:snapshot];
        }
    } else if ([args[2] isEqual:@"stop"] || [args[2] isEqual:@"restore"]) {
        for (NSDictionary *snapshot in input) {
            BOOL success = [args[2] isEqual:@"stop"] ? JasoMenuStop(snapshot, &error) : JasoMenuRestore(snapshot, &error);
            if (!success) break;
        }
    } else error = @"Unknown menu handoff operation.";
    if (error) { fprintf(stderr, "%s\n", error.UTF8String); return 1; }
    NSData *data = [NSJSONSerialization dataWithJSONObject:snapshots options:0 error:NULL];
    fwrite(data.bytes, 1, data.length, stdout); fputc('\n', stdout);
    return 0;
}

static NSString *L(NSString *en, NSString *ko) {
    return JasoText(en, ko);
}
static NSMenuItem *JasoMenuCommand(NSString *title, SEL action, NSString *key, id target) {
    NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:title action:action keyEquivalent:key];
    item.target = target;
    item.keyEquivalentModifierMask = NSEventModifierFlagCommand;
    return item;
}

// A summary is informational content; a single command below opens Status.
@interface JasoMenuSummary : NSView
@property NSTextField *titleLabel;
@property NSTextField *detailLabel;
- (void)showTitle:(NSString *)title detail:(NSString *)detail;
@end
@implementation JasoMenuSummary
- (instancetype)init {
    if (!(self = [super initWithFrame:NSMakeRect(0, 0, 340, 88)])) return nil;
    self.identifier = @"menu-summary";
    self.titleLabel = [NSTextField wrappingLabelWithString:@""];
    self.detailLabel = [NSTextField wrappingLabelWithString:@""];
    self.titleLabel.font = [NSFont systemFontOfSize:13 weight:NSFontWeightSemibold];
    self.detailLabel.font = [NSFont systemFontOfSize:12];
    self.titleLabel.textColor = NSColor.labelColor;
    self.detailLabel.textColor = NSColor.labelColor;
    for (NSTextField *field in @[self.titleLabel, self.detailLabel]) [self addSubview:field];
    return self;
}
- (BOOL)isFlipped { return YES; }
- (NSView *)hitTest:(NSPoint)point { return nil; }
- (void)showTitle:(NSString *)title detail:(NSString *)detail {
    self.titleLabel.stringValue = title ?: @""; self.detailLabel.stringValue = detail ?: @"";
    CGFloat width = self.bounds.size.width - 32;
    CGFloat headingHeight = [self.titleLabel.cell cellSizeForBounds:NSMakeRect(0, 0, width, CGFLOAT_MAX)].height;
    CGFloat detailHeight = [self.detailLabel.cell cellSizeForBounds:NSMakeRect(0, 0, width, CGFLOAT_MAX)].height;
    self.titleLabel.frame = NSMakeRect(16, 10, width, headingHeight);
    self.detailLabel.frame = NSMakeRect(16, 16 + headingHeight, width, detailHeight);
    [self setFrameSize:NSMakeSize(self.bounds.size.width, 26 + headingHeight + detailHeight)];
}
@end

@interface JasoMenu : NSObject <NSApplicationDelegate, NSMenuDelegate>
@property NSStatusItem *statusItem;
@property NSMenu *menu;
@property JasoMenuSummary *summaryView;
@property NSMenuItem *pauseItem;
@property NSNumber *startupEnabled;
@property NSString *startupError;
@property BOOL quitting;
@property dispatch_queue_t workerQueue;
@property JasoAnimatedMark *mark;
@property NSDictionary *snapshot;
@property NSString *configPath;
@property NSString *workerPath;
@property BOOL busy;
@property JasoWorkspaceWindowController *statusWindow;
@property dispatch_queue_t activityQueue;
@property NSMutableArray<JasoWorkspaceReply> *activityReplies;
@property NSDictionary *activityResult;
@property NSString *activityResultConfig;
@property NSTimeInterval activityResultTime;
@property dispatch_queue_t historyQueue;
@property NSMutableSet<NSTask *> *queryTasks;
@property dispatch_group_t queryGroup;
@property NSString *statusError;
@property NSDate *snapshotDate;
@property BOOL refreshStartup;
@property JasoSettingsWindowController *settingsWindow;
@property JasoSetupWindowController *setupWindow;
@property BOOL setupBusy;
@property BOOL setupIntroductionChecked;
@property NSTask *previewTask;
@property BOOL previewCancelled;
@property BOOL rebuildingWindows;
@end

@implementation JasoMenu
- (instancetype)init {
    if ((self = [super init])) {
        _workerQueue = dispatch_queue_create("io.github.garlicvread.jaso-nfc.ui-commands", dispatch_queue_attr_make_with_qos_class(DISPATCH_QUEUE_SERIAL, QOS_CLASS_UTILITY, 0));
        _activityQueue = dispatch_queue_create("io.github.garlicvread.jaso-nfc.activity", DISPATCH_QUEUE_SERIAL);
        _activityReplies = NSMutableArray.new;
        _queryTasks=NSMutableSet.new;_queryGroup=dispatch_group_create();
        _historyQueue = dispatch_queue_create("io.github.garlicvread.jaso-nfc.history", DISPATCH_QUEUE_SERIAL);
    }
    return self;
}
- (NSMenuItem *)item:(NSString *)title action:(SEL)action {
    NSMenuItem *item = [[NSMenuItem alloc] initWithTitle:title action:action keyEquivalent:@""];
    item.target = self;
    [self.menu addItem:item];
    return item;
}
- (void)rebuildApplicationMenu {
    NSMenu *mainMenu = [[NSMenu alloc] initWithTitle:@"Jaso NFC"];
    NSMenuItem *application = JasoMenuCommand(@"Jaso NFC", nil, @"", nil);
    NSMenu *applicationMenu = [[NSMenu alloc] initWithTitle:@"Jaso NFC"];
    application.submenu = applicationMenu;
    [mainMenu addItem:application];
    [applicationMenu addItem:JasoMenuCommand(L(@"About Jaso NFC", @"Jaso NFC 정보"), @selector(about:), @"", self)];
    [applicationMenu addItem:NSMenuItem.separatorItem];
    [applicationMenu addItem:JasoMenuCommand(L(@"Manage folders…", @"정리할 폴더…"), @selector(setup:), @"", self)];
    [applicationMenu addItem:JasoMenuCommand(L(@"Settings…", @"설정…"), @selector(settings:), @",", self)];
    [applicationMenu addItem:NSMenuItem.separatorItem];
    [applicationMenu addItem:JasoMenuCommand(L(@"Hide Jaso NFC", @"Jaso NFC 가리기"), @selector(hide:), @"h", NSApp)];
    NSMenuItem *hideOthers = JasoMenuCommand(L(@"Hide Others", @"기타 가리기"), @selector(hideOtherApplications:), @"h", NSApp);
    hideOthers.keyEquivalentModifierMask = NSEventModifierFlagCommand | NSEventModifierFlagOption;
    [applicationMenu addItem:hideOthers];
    [applicationMenu addItem:JasoMenuCommand(L(@"Show All", @"모두 보기"), @selector(unhideAllApplications:), @"", NSApp)];
    [applicationMenu addItem:NSMenuItem.separatorItem];
    [applicationMenu addItem:JasoMenuCommand(L(@"Quit Jaso NFC", @"Jaso NFC 종료"), @selector(quit:), @"q", self)];

    NSMenuItem *view = JasoMenuCommand(L(@"View", @"보기"), nil, @"", nil);
    NSMenu *viewMenu = [[NSMenu alloc] initWithTitle:view.title];
    view.submenu = viewMenu;
    [mainMenu addItem:view];
    [viewMenu addItem:JasoMenuCommand(L(@"Zoom In", @"확대"), @selector(zoomIn:), @"+", nil)];
    NSMenuItem *equals = JasoMenuCommand(L(@"Zoom In", @"확대"), @selector(zoomIn:), @"=", nil);
    equals.hidden = YES;
    equals.allowsKeyEquivalentWhenHidden = YES;
    [viewMenu addItem:equals];
    [viewMenu addItem:JasoMenuCommand(L(@"Zoom Out", @"축소"), @selector(zoomOut:), @"-", nil)];
    [viewMenu addItem:JasoMenuCommand(L(@"Actual Size", @"실제 크기"), @selector(resetZoom:), @"0", nil)];

    NSMenuItem *windows = JasoMenuCommand(L(@"Window", @"창"), nil, @"", nil);
    NSMenu *windowMenu = [[NSMenu alloc] initWithTitle:windows.title];
    windows.submenu = windowMenu;
    [mainMenu addItem:windows];
    [windowMenu addItem:JasoMenuCommand(L(@"Open Jaso NFC…", @"Jaso NFC 열기…"), @selector(details:), @"", self)];
    [windowMenu addItem:NSMenuItem.separatorItem];
    [windowMenu addItem:JasoMenuCommand(L(@"Close", @"닫기"), @selector(performClose:), @"w", nil)];
    [windowMenu addItem:JasoMenuCommand(L(@"Minimize", @"최소화"), @selector(performMiniaturize:), @"m", nil)];
    NSApp.mainMenu = mainMenu;
    NSApp.windowsMenu = windowMenu;
}
- (void)updateActivationPolicy {
    // Jaso NFC remains a normal, manageable application when its windows close.
    if (NSApp.activationPolicy != NSApplicationActivationPolicyRegular) [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
}
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    self.workerPath = [[[NSBundle mainBundle] executablePath].stringByDeletingLastPathComponent stringByAppendingPathComponent:@"jaso-nfc"];
    NSArray *args = NSProcessInfo.processInfo.arguments;
    NSString *configError = nil;
    self.configPath = JasoMenuConfig(args,
        [NSHomeDirectory() stringByAppendingPathComponent:@"Library/LaunchAgents/io.github.garlicvread.jaso-nfc.menu.plist"],
        [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support/jaso-nfc/config.json"], &configError);
    if (!self.configPath) { [self alert:L(@"Configuration unavailable", @"설정 파일 확인 불가") text:configError]; [NSApp terminate:nil]; return; }
    [self rebuildMenu];
    self.refreshStartup = YES;
    [self refresh];
    if ([args containsObject:@"--show-status"]) [self details:nil];
}
- (void)rebuildMenu {
    NSArray *args = NSProcessInfo.processInfo.arguments;
    self.menu = [[NSMenu alloc] initWithTitle:@"Jaso NFC"];
    self.menu.delegate = self;
    self.summaryView = [JasoMenuSummary new];
    NSMenuItem *summary = [self item:@"" action:nil]; summary.view = self.summaryView;
    [self.menu addItem:NSMenuItem.separatorItem];
    [self item:L(@"Open Jaso NFC…", @"Jaso NFC 열기…") action:@selector(details:)];
    self.pauseItem = [self item:@"" action:@selector(togglePause:)];
    [self item:L(@"Manage folders…", @"정리할 폴더…") action:@selector(setup:)];
    [self.menu addItem:NSMenuItem.separatorItem];
    NSMenuItem *settingsItem = [self item:L(@"Settings…", @"설정…") action:@selector(settings:)];
    settingsItem.keyEquivalent = @",";
    [self item:L(@"Quit Jaso NFC", @"Jaso NFC 종료") action:@selector(quit:)];
    [NSUserDefaults.standardUserDefaults registerDefaults:@{@"animateMark":@YES,@"markStyle":@1}];
    NSInteger style = [NSUserDefaults.standardUserDefaults integerForKey:@"markStyle"];
    NSUInteger styleArg = [args indexOfObject:@"--mark-style"];
    if (!self.statusItem && styleArg != NSNotFound && styleArg + 1 < args.count) style = [args[styleArg + 1] integerValue];
    JasoMarkSetStyle(MAX(0, MIN(2, style)));
    if (!self.statusItem) {
        self.statusItem = [NSStatusBar.systemStatusBar statusItemWithLength:JasoMarkWidth];
        self.mark = [[JasoAnimatedMark alloc] initWithFrame:NSMakeRect(0,0,JasoMarkWidth,22)];
        self.mark.animationEnabled = [NSUserDefaults.standardUserDefaults boolForKey:@"animateMark"];
        [self.statusItem.button addSubview:self.mark];
    }
    self.statusItem.button.accessibilityLabel = @"Jaso NFC";
    self.statusItem.button.toolTip = @"Jaso NFC";
    self.statusItem.menu = self.menu;
    [self rebuildApplicationMenu];
    [self updateActivationPolicy];
    [self updateMenuSnapshot];
}
- (NSDictionary *)execute:(NSArray<NSString *> *)arguments error:(NSString **)error {
    NSTask *task = [[NSTask alloc] init];
    task.executableURL = [NSURL fileURLWithPath:self.workerPath];
    task.arguments = arguments;
    NSPipe *pipe = NSPipe.pipe;
    task.standardOutput = pipe;
    task.standardError = pipe;
    BOOL preview = arguments.count > 1 && [arguments[0] isEqual:@"setup"] && [arguments[1] isEqual:@"preview"];
    BOOL query = [@[@"activity",@"history",@"status",@"storage"] containsObject:arguments.firstObject];
    NSError *launchError = nil;
    @synchronized (self) {
        if((preview||query)&&self.quitting){*error=L(@"Jaso NFC is closing.",@"Jaso NFC를 종료하고 있습니다.");return nil;}
        if (preview && self.previewCancelled) { *error = L(@"Preview cancelled.", @"미리보기를 취소했습니다."); return nil; }
        if (![task launchAndReturnError:&launchError]) { *error = launchError.localizedDescription; return nil; }
        if (preview) self.previewTask = task;
        if(preview||query){[self.queryTasks addObject:task];dispatch_group_enter(self.queryGroup);}
    }
    __block BOOL timedOut = NO;
    __block BOOL finished = NO;
    if (preview || query) {
        NSTimeInterval deadline=preview?30:[arguments.firstObject isEqual:@"activity"]?3:15;
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(deadline*NSEC_PER_SEC)), dispatch_get_global_queue(QOS_CLASS_UTILITY,0), ^{
            @synchronized(task) { if(finished || !task.running)return; timedOut=YES; [task terminate]; }
            dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 2*NSEC_PER_SEC),dispatch_get_global_queue(QOS_CLASS_UTILITY,0),^{
                @synchronized(task) { if(!finished && task.running)kill(task.processIdentifier,SIGKILL); }
            });
        });
    }
    NSMutableData *data=NSMutableData.new;
    BOOL oversized=NO;
    for (;;) {
        NSData *part=[pipe.fileHandleForReading readDataOfLength:65536];
        if(!part.length)break;
        if(data.length+part.length<=8*1024*1024)[data appendData:part];
        else { oversized=YES; if(preview||query){ @synchronized(task){if(task.running)[task terminate];} } }
    }
    [task waitUntilExit];
    @synchronized(task){finished=YES;}
    if(preview||query){@synchronized(self){[self.queryTasks removeObject:task];}dispatch_group_leave(self.queryGroup);}
    if (timedOut || oversized) {
        if(preview) { @synchronized(self){if(self.previewTask==task)self.previewTask=nil;} }
        *error = timedOut ? L(@"This request is taking longer than expected. Try again in a moment.",@"응답이 늦어지고 있습니다. 잠시 후 다시 시도하세요.") : L(@"There is too much information to show at once. Choose a smaller folder.",@"한 번에 표시할 항목이 많습니다. 더 작은 폴더를 선택하세요.");
        return nil;
    }
    if (preview) {
        @synchronized (self) { if (self.previewTask == task) self.previewTask = nil; }
        if (self.previewCancelled || task.terminationReason == NSTaskTerminationReasonUncaughtSignal) {
            *error = self.previewCancelled ? L(@"Preview cancelled.", @"미리보기를 취소했습니다.") : L(@"This preview took too long. Try a smaller folder or check that the drive is connected.", @"미리보기 시간이 길어지고 있습니다. 더 작은 폴더를 선택하거나 드라이브 연결을 확인하세요.");
            return nil;
        }
    }
    if (task.terminationStatus != 0) { *error = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding] ?: @"Worker command failed"; return nil; }
    id value = [NSJSONSerialization JSONObjectWithData:data options:0 error:&launchError];
    if (![value isKindOfClass:NSDictionary.class]) { *error = launchError.localizedDescription ?: @"Invalid worker response"; return nil; }
    return value;
}
- (NSArray *)configured:(NSArray *)arguments { return [arguments arrayByAddingObjectsFromArray:@[@"--config", self.configPath]]; }
- (BOOL)applicationShouldHandleReopen:(NSApplication *)application hasVisibleWindows:(BOOL)flag { [self details:nil]; return YES; }
- (void)menuWillOpen:(NSMenu *)menu { self.refreshStartup = YES; [self refresh]; }
- (void)refresh {
    if (self.busy || self.setupBusy || self.quitting) return;
    self.busy = YES;
    [self.statusWindow setRefreshing:YES];
    [self.settingsWindow updateStartupEnabled:self.startupEnabled error:self.startupError busy:YES];
    BOOL refreshStartup = self.refreshStartup;
    self.refreshStartup = NO;
    dispatch_async(self.workerQueue, ^{
        @autoreleasepool {
            NSString *error = nil;
            NSDictionary *snapshot = [self execute:[self configured:@[@"status"]] error:&error];
            NSString *startupError = nil;
            NSDictionary *startup = refreshStartup ? [self execute:@[@"startup", @"status"] error:&startupError] : nil;
            dispatch_async(dispatch_get_main_queue(), ^{
                self.busy = NO;
                if (self.quitting) return;
                self.statusError = error;
                [self.statusWindow setRefreshing:NO];
                if (refreshStartup) {
                    BOOL consistent = [startup[@"consistent"] respondsToSelector:@selector(boolValue)] && [startup[@"consistent"] boolValue];
                    BOOL installed = [startup[@"installed"] isKindOfClass:NSNumber.class] && [startup[@"installed"] boolValue];
                    self.startupError = startupError ?: (!installed ? L(@"Install Jaso NFC to manage login startup.", @"로그인 실행을 설정하려면 Jaso NFC를 설치하세요.") : consistent ? nil : L(@"Startup registrations need attention.", @"로그인 실행 설정을 확인하세요."));
                    self.startupEnabled = !self.startupError && [startup[@"enabled"] isKindOfClass:NSNumber.class] ? startup[@"enabled"] : nil;
                }
                [self.settingsWindow updateStartupEnabled:self.startupEnabled error:self.startupError busy:NO];
                if (snapshot) { self.snapshot = snapshot; self.snapshotDate = NSDate.date; }
                [self updateMenuSnapshot];
                [self.statusWindow updateSnapshot:self.snapshot error:error updatedAt:self.snapshotDate];
                if (snapshot && self.configPath.length && [snapshot[@"apply"] isKindOfClass:NSNumber.class] && !self.setupIntroductionChecked) {
                    self.setupIntroductionChecked = YES;
                    NSString *key = [@"setupPresented:" stringByAppendingString:self.configPath];
                    if (![NSUserDefaults.standardUserDefaults boolForKey:key] && ![snapshot[@"apply"] boolValue]) [self setup:nil];
                    else [NSUserDefaults.standardUserDefaults setBool:YES forKey:key];
                }
            });
        }
    });
}
- (void)updateMenuSnapshot {
    NSDictionary *presentation = JasoWorkspaceStatus(self.snapshot, self.statusError, JasoUsesKorean());
    NSString *detail = presentation[@"subtitle"];
    [self.summaryView showTitle:presentation[@"title"] detail:detail];
    self.pauseItem.title = [presentation[@"primaryTitle"] length] ? presentation[@"primaryTitle"] : L(@"Worker control unavailable", @"작업 제어 확인 불가");
    self.pauseItem.hidden = ![presentation[@"primaryAction"] length];
}
- (void)alert:(NSString *)title text:(NSString *)text {
    [NSApp activateIgnoringOtherApps:YES];
    NSAlert *alert = [[NSAlert alloc] init]; alert.messageText = title; alert.informativeText = text; [alert runModal];
}
- (void)run:(NSArray *)arguments {
    if (self.busy || self.setupBusy || self.quitting) return;
    self.busy = YES;
    [self.statusWindow setRefreshing:YES];
    [self.settingsWindow updateStartupEnabled:self.startupEnabled error:self.startupError busy:YES];
    dispatch_async(self.workerQueue, ^{
        @autoreleasepool {
            NSString *error = nil; [self execute:arguments error:&error];
            dispatch_async(dispatch_get_main_queue(), ^{
                self.busy = NO;
                if (self.quitting) return;
                if (error) [self alert:L(@"Command could not finish", @"작업을 실행하지 못했습니다") text:error];
                self.refreshStartup = YES;
                [self refresh];
            });
        }
    });
}
- (void)togglePause:(id)sender {
    NSString *action = JasoWorkspaceStatus(self.snapshot, self.statusError, JasoUsesKorean())[@"primaryAction"];
    if ([action isEqual:@"start"]) [self start:sender];
    else if ([@[@"pause", @"resume"] containsObject:action]) [self run:[self configured:@[action]]];
}
- (void)start:(id)sender { [self run:@[@"start"]]; }
- (void)stop:(id)sender { [self run:@[@"stop"]]; }
- (void)reconcileAll:(id)sender { [self run:[self configured:@[@"reconcile"]]]; }
- (void)updateAppearance {
    self.mark.animationEnabled = [NSUserDefaults.standardUserDefaults boolForKey:@"animateMark"];
    JasoMarkSetStyle(MAX(0, MIN(2, [NSUserDefaults.standardUserDefaults integerForKey:@"markStyle"])));
    [self.mark refreshAnimation];
}
- (BOOL)validateMenuItem:(NSMenuItem *)item {
    if (item.action==@selector(copyDiagnostics:)) return self.snapshot != nil;
    if (item.action==@selector(togglePause:)) {
        NSString *action = JasoWorkspaceStatus(self.snapshot, self.statusError, JasoUsesKorean())[@"primaryAction"];
        return !self.busy && !self.setupBusy && !self.quitting && [@[@"start", @"pause", @"resume"] containsObject:action];
    }
    if (item.action == @selector(setup:)) return !self.quitting;
    return YES;
}
- (void)details:(id)sender {
    if (!self.statusWindow) {
        self.statusWindow = [JasoWorkspaceWindowController new];
        __weak JasoMenu *weakSelf = self;
        self.statusWindow.refreshHandler = ^{ [weakSelf refresh]; };
        self.statusWindow.actionHandler = ^(NSString *action) {
            JasoMenu *menu = weakSelf;
            if ([action isEqual:@"start"]) [menu start:nil];
            else if ([action isEqual:@"pause"] || [action isEqual:@"resume"]) [menu run:[menu configured:@[action]]];
            else if ([action isEqual:@"storage-settings"]) [menu openSystemSettings:@"x-apple.systempreferences:com.apple.settings.Storage"];
            else if ([action isEqual:@"permissions"]) [menu permissions:nil];
            else if ([action isEqual:@"folders"]) [menu setup:nil];
            else if ([action isEqual:@"cancel-preview"]) [menu cancelSetupPreview];
            else if ([action isEqual:@"settings"]) [menu settings:nil];
            else if ([action isEqual:@"restart"]) [menu run:@[@"restart"]];
            else if ([action isEqual:@"stop"]) [menu stop:nil];
            else if ([action isEqual:@"reconcile"]) [menu reconcileAll:nil];
        };
        self.statusWindow.requestHandler = ^(NSString *request, NSDictionary *parameters, JasoWorkspaceReply reply) { [weakSelf requestWorkspace:request parameters:parameters reply:reply]; };
        self.statusWindow.diagnosticsHandler = ^{ [weakSelf copyDiagnostics:nil]; };
        self.statusWindow.pathHandler = ^(NSString *path) { [weakSelf revealPath:path]; };
        self.statusWindow.closeHandler = ^{
            JasoMenu *menu = weakSelf;
            [menu cancelSetupPreview];
            [menu updateActivationPolicy];
        };
    }
    [self updateActivationPolicy];
    [self.statusWindow updateSnapshot:self.snapshot error:self.statusError updatedAt:self.snapshotDate];
    [self.statusWindow setRefreshing:self.busy];
    [self.statusWindow showWindow:sender];
    [self refresh];
}
- (void)requestWorkspace:(NSString *)request parameters:(NSDictionary *)parameters reply:(JasoWorkspaceReply)reply {
    if (self.quitting) { reply(nil, L(@"Jaso NFC is closing.", @"Jaso NFC를 종료하고 있습니다.")); return; }
    if ([request isEqual:@"activity"]) { [self requestActivityFresh:[parameters[@"fresh"] boolValue] reply:reply]; return; }
    NSMutableArray *arguments=NSMutableArray.new;
    if ([@[@"activity",@"storage"] containsObject:request]) [arguments addObject:request];
    else if ([request isEqual:@"history"]) {
        [arguments addObjectsFromArray:@[@"history", @"list"]];
        for (NSString *key in @[@"search",@"offset",@"limit",@"date",@"result"]) if (parameters[key]) [arguments addObjectsFromArray:@[[@"--" stringByAppendingString:key],[parameters[key] description]]];
    } else if ([@[@"history-preview",@"history-restore",@"history-result"] containsObject:request]) {
        [arguments addObjectsFromArray:@[@"history",[request substringFromIndex:8]]];
        NSArray *keys=[request isEqual:@"history-preview"]?@[@"id",@"revision"]:[request isEqual:@"history-result"]?@[@"request_id"]:@[@"request_id",@"operation_id",@"revision"];
        for (NSString *key in keys) if (parameters[key]) [arguments addObjectsFromArray:@[[@"--" stringByAppendingString:[key stringByReplacingOccurrencesOfString:@"_" withString:@"-"]],[parameters[key] description]]];
    } else { reply(nil,L(@"This view could not be loaded.",@"화면을 불러오지 못했습니다.")); return; }
    [arguments addObjectsFromArray:@[@"--config",self.configPath]];
    dispatch_async([request isEqual:@"activity"]?self.activityQueue:self.historyQueue, ^{
        @autoreleasepool { NSString *error=nil; NSDictionary *result=[self execute:arguments error:&error]; reply(result,error); }
    });
}
- (void)requestActivityFresh:(BOOL)fresh reply:(JasoWorkspaceReply)reply {
    NSDictionary *cached=nil; NSString *rejected=nil; BOOL launch=NO;
    @synchronized(self) {
        // One parsed result serves both Activity windows. The short cache only
        // joins nearby polls; explicit refresh always bypasses it. Errors retry.
        if(self.quitting)rejected=L(@"Jaso NFC is closing.",@"Jaso NFC를 종료하고 있습니다.");
        else if(!fresh && self.activityResult && [self.activityResultConfig isEqual:self.configPath] && NSProcessInfo.processInfo.systemUptime-self.activityResultTime<.75)cached=self.activityResult;
        else if(self.activityReplies.count>=8)rejected=L(@"Activity is already being refreshed. Try again in a moment.",@"활동을 새로고침하고 있습니다. 잠시 후 다시 시도하세요.");
        else { [self.activityReplies addObject:[reply copy]];launch=self.activityReplies.count==1; }
    }
    if(cached||rejected){reply(cached,rejected);return;}
    if(!launch)return;
    NSString *config=[self.configPath copy];
    dispatch_async(self.activityQueue, ^{
        @autoreleasepool {
            NSString *error=nil;NSDictionary *result=[self execute:@[@"activity",@"--config",config] error:&error];
            dispatch_async(dispatch_get_main_queue(),^{
                NSArray<JasoWorkspaceReply> *replies;
                @synchronized(self) {
                    // Keep the flight pending until it publishes. Otherwise a
                    // cache hit could expose B before queued publication A.
                    self.activityResult=!error&&!self.quitting?result:nil;
                    self.activityResultConfig=config;self.activityResultTime=NSProcessInfo.processInfo.systemUptime;
                    replies=self.activityReplies.copy;[self.activityReplies removeAllObjects];
                }
                // Publish immediately to both views, including a detached view
                // whose own timer just reused the previous cached response.
                if(!self.quitting)[self.statusWindow updateActivity:result error:error];
                for(JasoWorkspaceReply consumer in replies)consumer(result,error);
            });
        }
    });
}
- (void)copyDiagnostics:(id)sender {
    if (!self.snapshot) return;
    NSMutableDictionary *diagnostics = self.snapshot.mutableCopy;
    if (self.statusError) diagnostics[@"status_refresh_error"] = self.statusError;
    if (self.snapshotDate) diagnostics[@"status_checked_at"] = [NSISO8601DateFormatter.new stringFromDate:self.snapshotDate];
    NSData *data = [NSJSONSerialization dataWithJSONObject:diagnostics options:NSJSONWritingPrettyPrinted | NSJSONWritingSortedKeys error:nil];
    if (!data) return;
    [NSPasteboard.generalPasteboard clearContents];
    [NSPasteboard.generalPasteboard setString:[[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding] forType:NSPasteboardTypeString];
}
- (void)revealPath:(NSString *)path {
    if (!JasoAbsolutePath(path)) return;
    [NSWorkspace.sharedWorkspace activateFileViewerSelectingURLs:@[[NSURL fileURLWithPath:path]]];
}
- (void)cancelReadTasks {
    @synchronized(self){
        self.previewCancelled=YES;
        for(NSTask *task in self.queryTasks.copy)if(task.running){
            [task terminate];
            dispatch_after(dispatch_time(DISPATCH_TIME_NOW,2*NSEC_PER_SEC),dispatch_get_global_queue(QOS_CLASS_UTILITY,0),^{
                @synchronized(self){if([self.queryTasks containsObject:task]&&task.running)kill(task.processIdentifier,SIGKILL);}
            });
        }
    }
}
- (void)cancelSetupPreview {
    @synchronized (self) {
        self.previewCancelled = YES;
        NSTask *task=self.previewTask;
        if (task.running) {
            [task terminate];
            dispatch_after(dispatch_time(DISPATCH_TIME_NOW,2*NSEC_PER_SEC),dispatch_get_global_queue(QOS_CLASS_UTILITY,0),^{
                @synchronized(self){if(self.previewTask==task&&task.running)kill(task.processIdentifier,SIGKILL);}
            });
        }
    }
}
- (void)runSetup:(NSString *)operation draft:(NSDictionary *)draft revision:(NSString *)revision start:(BOOL)start {
    if (self.setupBusy || self.quitting || !self.setupWindow) return;
    JasoSetupWindowController *window = self.setupWindow;
    NSString *command = [operation isEqual:@"refresh-drives"] ? @"read" : operation;
    NSMutableArray *arguments = [NSMutableArray arrayWithArray:@[@"setup", command, @"--config", self.configPath]];
    if (draft) {
        NSError *jsonError = nil;
        NSData *data = [NSJSONSerialization dataWithJSONObject:draft options:0 error:&jsonError];
        if (!data) { [window updatePreview:nil error:jsonError.localizedDescription]; return; }
        [arguments addObjectsFromArray:@[@"--draft", [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding]]];
    }
    if (revision.length) [arguments addObjectsFromArray:@[@"--revision", revision]];
    if (start) [arguments addObject:@"--start"];
    BOOL preview = [operation isEqual:@"preview"];
    @synchronized (self) { self.previewCancelled = NO; }
    self.setupBusy = YES;
    [window setBusy:YES cancellable:preview];
    dispatch_async(self.workerQueue, ^{
        @autoreleasepool {
            NSString *error = nil;
            NSDictionary *result = [self execute:arguments error:&error];
            dispatch_async(dispatch_get_main_queue(), ^{
                self.setupBusy = NO;
                if (self.quitting) return;
                if (self.setupWindow == window) {
                    [window setBusy:NO cancellable:NO];
                    if ([operation isEqual:@"refresh-drives"]) [window updateDriveInventory:result error:error];
                    else if ([operation isEqual:@"read"]) [window updateConfiguration:result error:error];
                    else if (preview) [window updatePreview:result error:error];
                    else [window updateSave:result error:error];
                }
                if ([operation isEqual:@"save"] && result) {
                    [NSUserDefaults.standardUserDefaults setBool:YES forKey:[@"setupPresented:" stringByAppendingString:self.configPath]];
                    self.refreshStartup = YES;
                }
                if (self.setupWindow && self.setupWindow != window && !self.setupWindow.revision.length) {
                    [self runSetup:@"read" draft:nil revision:nil start:NO];
                } else {
                    [self refresh];
                }
            });
        }
    });
}
- (void)setup:(id)sender {
    if (self.quitting) return;
    BOOL created = self.setupWindow == nil;
    if (created) {
        self.setupWindow = [JasoSetupWindowController new];
        __weak JasoMenu *weakSelf = self;
        self.setupWindow.previewHandler = ^(NSDictionary *draft) { [weakSelf runSetup:@"preview" draft:draft revision:nil start:NO]; };
        self.setupWindow.saveHandler = ^(NSDictionary *draft, NSString *revision, BOOL start) { [weakSelf runSetup:@"save" draft:draft revision:revision start:start]; };
        self.setupWindow.reloadHandler = ^{ [weakSelf runSetup:@"read" draft:nil revision:nil start:NO]; };
        self.setupWindow.refreshDrivesHandler = ^{ [weakSelf runSetup:@"refresh-drives" draft:nil revision:nil start:NO]; };
        self.setupWindow.startDriveHandler = ^(NSString *uuid,NSString *revision) { [weakSelf startDrive:uuid revision:revision]; };
        self.setupWindow.cancelHandler = ^{ [weakSelf cancelSetupPreview]; };
        self.setupWindow.closeHandler = ^{
            JasoMenu *menu = weakSelf;
            [menu cancelSetupPreview];
            menu.setupWindow = nil;
            [menu updateActivationPolicy];
        };
    }
    if (!self.statusWindow) [self details:nil];
    [self.statusWindow embedView:[self.setupWindow embeddedContentViewForWindow:self.statusWindow.window] inSection:@"folders"];
    [self.statusWindow selectSection:@"folders"];
    [self.statusWindow showWindow:sender];
    if (self.configPath.length) [NSUserDefaults.standardUserDefaults setBool:YES forKey:[@"setupPresented:" stringByAppendingString:self.configPath]];
    if (created) [self runSetup:@"read" draft:nil revision:nil start:NO];
}
- (void)startDrive:(NSString *)uuid revision:(NSString *)revision {
    if(self.setupBusy||self.quitting)return;self.setupBusy=YES;[self.setupWindow setBusy:YES cancellable:NO];
    dispatch_async(self.workerQueue,^{@autoreleasepool {NSString *error=nil;NSDictionary *result=[self execute:@[@"setup",@"start-drive",@"--config",self.configPath,@"--uuid",uuid,@"--revision",revision] error:&error];dispatch_async(dispatch_get_main_queue(),^{self.setupBusy=NO;[self.setupWindow setBusy:NO cancellable:NO];[self.setupWindow updateDriveStart:result error:error];[self refresh];});}});
}
- (void)settings:(id)sender {
    if (!self.settingsWindow) {
        self.settingsWindow = [JasoSettingsWindowController new];
        __weak JasoMenu *weakSelf = self;
        self.settingsWindow.storageHandler = ^{ [weakSelf.statusWindow showStorage:nil]; };
        self.settingsWindow.languageChangedHandler = ^{ [weakSelf languageChanged]; };
        self.settingsWindow.appearanceChangedHandler = ^{ [weakSelf updateAppearance]; };
        self.settingsWindow.startupChangedHandler = ^(BOOL enabled) { [weakSelf run:@[@"startup", enabled ? @"on" : @"off"]]; };
        self.settingsWindow.availableStyles = @[@(JasoMarkStyleAvailable(0)), @(JasoMarkStyleAvailable(1)), @(JasoMarkStyleAvailable(2))];
        self.settingsWindow.actionHandler = ^(NSString *action) {
            JasoMenu *menu = weakSelf;
            if ([action isEqual:@"reconcile"]) [menu reconcileAll:nil];
            else if ([action isEqual:@"restart"]) [menu run:@[@"restart"]];
            else if ([action isEqual:@"diagnostics"]) [menu copyDiagnostics:nil];
            else if ([action isEqual:@"login-items"]) [menu openLoginItems:nil];
            else if ([action isEqual:@"setup"]) [menu setup:nil];
            else if ([action isEqual:@"full-disk-access"]) [menu openFullDiskAccess:nil];
            else if ([action isEqual:@"reveal-app"]) [menu showInstalledApp:nil];
            else if ([action isEqual:@"user-guide"]) [NSWorkspace.sharedWorkspace openURL:[NSURL URLWithString:L(@"https://github.com/garlicvread/jaso-nfc/blob/main/docs/usage.en.md", @"https://github.com/garlicvread/jaso-nfc/blob/main/docs/usage.md")]];
        };
        self.settingsWindow.closeHandler = ^{
            JasoMenu *menu = weakSelf;
            menu.settingsWindow = nil;
            [menu updateActivationPolicy];
        };
    }
    [self updateActivationPolicy];
    [self.settingsWindow updateStartupEnabled:self.startupEnabled error:self.startupError busy:self.busy];
    if (!self.statusWindow) [self details:nil];
    [self.statusWindow embedView:[self.settingsWindow embeddedContentViewForWindow:self.statusWindow.window] inSection:@"settings"];
    [self.statusWindow selectSection:@"settings"];
    [self.statusWindow showWindow:sender];
    self.refreshStartup = YES;
    [self refresh];
}
- (void)languageChanged {
    self.rebuildingWindows = YES;
    @try {
        [self rebuildMenu];
        [self.setupWindow reloadLocalization];
        [self.statusWindow reloadLocalization];
        [self.statusWindow.window makeKeyAndOrderFront:nil];
    } @finally {
        self.rebuildingWindows = NO;
        [self updateActivationPolicy];
    }
}
- (void)openLogs:(id)sender {
    NSData *data = [NSData dataWithContentsOfFile:self.configPath];
    id config = data ? [NSJSONSerialization JSONObjectWithData:data options:0 error:nil] : nil;
    if (![config isKindOfClass:NSDictionary.class]) { [self alert:L(@"Configuration unavailable", @"설정 파일 확인 불가") text:self.configPath]; return; }
    id log = config[@"log_dir"], state = config[@"state_dir"];
    NSString *support = [state isKindOfClass:NSString.class] ? state : [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support/jaso-nfc"];
    NSString *path = [log isKindOfClass:NSString.class] ? log : [support stringByAppendingPathComponent:@"logs"];
    [NSWorkspace.sharedWorkspace openURL:[NSURL fileURLWithPath:path isDirectory:YES]];
}
- (void)permissions:(id)sender { [self settings:sender]; }
- (void)openSystemSettings:(NSString *)url {
    [NSWorkspace.sharedWorkspace openURL:[NSURL URLWithString:url]];
}
- (void)openLoginItems:(id)sender {
    [self openSystemSettings:@"x-apple.systempreferences:com.apple.LoginItems-Settings.extension"];
}
- (void)openFullDiskAccess:(id)sender {
    [self showInstalledApp:sender];
    [self openSystemSettings:@"x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"];
}
- (void)showInstalledApp:(id)sender {
    [self revealPath:@"/Applications/Jaso NFC.app"];
}
- (void)about:(id)sender { [self alert:@"Jaso NFC" text:L(@"Jaso NFC joins decomposed Korean filename characters and automatically tidies names in your selected folders.\nAidALL Inc. · MIT license.\nhttps://github.com/garlicvread/jaso-nfc", @"분리된 한글 파일명을 정리하고 선택한 폴더의 새 파일도 자동으로 처리합니다.\nAidALL Inc. · MIT 라이선스.\nhttps://github.com/garlicvread/jaso-nfc")]; }
- (void)quit:(id)sender { [NSApp terminate:nil]; }
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)application {
    if (self.quitting) return NSTerminateLater;
    self.quitting = YES;
    [self cancelReadTasks];
    [self.summaryView showTitle:L(@"Stopping worker…", @"백그라운드 작업 중지 중…") detail:L(@"Jaso NFC will quit when shutdown is confirmed.", @"진행 중인 작업을 마무리한 뒤 종료합니다.")];
    [self.statusWindow setRefreshing:YES];
    [self.settingsWindow updateStartupEnabled:self.startupEnabled error:self.startupError busy:YES];
    dispatch_async(self.workerQueue, ^{
        @autoreleasepool {
            NSString *error = nil;
            NSDictionary *result = [self execute:@[@"stop"] error:&error];
            BOOL queriesStopped=dispatch_group_wait(self.queryGroup,dispatch_time(DISPATCH_TIME_NOW,5*NSEC_PER_SEC))==0;
            BOOL stopped = queriesStopped && !error && [result[@"stopped"] isEqual:@"io.github.garlicvread.jaso-nfc"];
            if(!queriesStopped)error=L(@"A background request is still closing. Try quitting again in a moment.",@"진행 중인 조회를 종료하고 있습니다. 잠시 후 다시 종료하세요.");
            if (!stopped && !error) error = L(@"Worker shutdown was not confirmed. Jaso NFC remains open.", @"작업이 아직 실행 중입니다. 잠시 후 다시 종료하세요.");
            dispatch_async(dispatch_get_main_queue(), ^{
                self.quitting = NO;
                self.busy = NO;
                if (!stopped) {
                    [self.statusWindow setRefreshing:NO];
                    [self.settingsWindow updateStartupEnabled:self.startupEnabled error:self.startupError busy:NO];
                    [self updateMenuSnapshot];
                    [self alert:L(@"Could not quit Jaso NFC", @"Jaso NFC 종료 실패") text:error];
                }
                [application replyToApplicationShouldTerminate:stopped];
            });
        }
    });
    return NSTerminateLater;
}
@end

int main(int argc, const char **argv) {
    @autoreleasepool {
        NSArray *arguments = NSProcessInfo.processInfo.arguments;
        if (arguments.count > 1 && [arguments[1] isEqual:@"--menu-handoff"]) return JasoMenuHandoff(arguments);
        NSString *support = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support/jaso-nfc"];
        [[NSFileManager defaultManager] createDirectoryAtPath:support withIntermediateDirectories:YES attributes:@{NSFilePosixPermissions:@0700} error:nil];
        int lock = open([[support stringByAppendingPathComponent:@"menu.lock"] fileSystemRepresentation], O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, 0600);
        if (lock < 0 || flock(lock, LOCK_EX | LOCK_NB) != 0) return 0;
        NSApplication *app = NSApplication.sharedApplication; [app setActivationPolicy:NSApplicationActivationPolicyRegular];
        JasoMenu *delegate = [[JasoMenu alloc] init]; app.delegate = delegate; [app run];
        close(lock);
    }
    return 0;
}
