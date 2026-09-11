#import <AppKit/AppKit.h>
#import <sys/mount.h>
#import <sys/stat.h>

// The installer has its own bundle identifier and never takes the menu lock.
// Its payload CLI owns application replacement, job handoff and rollback.
static NSString *IL(NSString *english, NSString *korean) {
    return [NSLocale.preferredLanguages.firstObject hasPrefix:@"ko"] ? korean : english;
}
static BOOL JasoInstallerError(NSString **error, NSString *message) {
    if (error) *error = message;
    return NO;
}
static BOOL JasoInstallerAbsolutePath(id value) {
    return [value isKindOfClass:NSString.class] && [value isAbsolutePath] &&
        [value rangeOfString:[NSString stringWithFormat:@"%C", (unichar)0]].location == NSNotFound;
}
static BOOL JasoInstallerMissing(NSError *error) {
    return [error.domain isEqual:NSCocoaErrorDomain] && error.code == NSFileReadNoSuchFileError;
}
static NSString *JasoInstallerRegisteredConfig(NSString *registration, BOOL menu, BOOL *present, NSString **error) {
    NSError *readError = nil;
    NSData *data = [NSData dataWithContentsOfFile:registration options:0 error:&readError];
    *present = data != nil || !JasoInstallerMissing(readError);
    if (!*present) return nil;
    if (!data) {
        JasoInstallerError(error, readError.localizedDescription ?: IL(@"Could not read the saved login startup settings.", @"저장된 로그인 실행 설정을 읽을 수 없습니다."));
        return nil;
    }
    id plist = [NSPropertyListSerialization propertyListWithData:data options:NSPropertyListImmutable format:NULL error:&readError];
    NSArray *arguments = [plist isKindOfClass:NSDictionary.class] ? plist[@"ProgramArguments"] : nil;
    NSString *label = menu ? @"io.github.garlicvread.jaso-nfc.menu" : @"io.github.garlicvread.jaso-nfc";
    BOOL valid = [plist isKindOfClass:NSDictionary.class] && [plist[@"Label"] isEqual:label] && [arguments isKindOfClass:NSArray.class];
    if (valid && menu) {
        // Exact contract shared with Finder menu startup.
        valid = arguments.count == 3 && JasoInstallerAbsolutePath(arguments[0]) && [arguments[1] isEqual:@"--config"] && JasoInstallerAbsolutePath(arguments[2]);
    } else if (valid) {
        // Native: worker run (or earlier watch) --config PATH. Earlier Python:
        // interpreter /managed/release/run.py watch --config PATH.
        NSUInteger offset = arguments.count == 5 ? 1 : 0;
        valid = (arguments.count == 4 || arguments.count == 5) && JasoInstallerAbsolutePath(arguments[0]) &&
            (!offset || JasoInstallerAbsolutePath(arguments[1])) &&
            ([arguments[offset + 1] isEqual:@"watch"] || (!offset && [arguments[1] isEqual:@"run"])) &&
            [arguments[offset + 2] isEqual:@"--config"] && JasoInstallerAbsolutePath(arguments[offset + 3]);
    }
    if (!valid) {
        JasoInstallerError(error, [NSString stringWithFormat:IL(@"The saved login startup entry has an invalid settings path:\n%@\nCheck this entry before installing again, or contact support with these details.", @"로그인 실행에 저장된 설정 경로가 올바르지 않습니다:\n%@\n해당 항목을 확인한 뒤 다시 설치하거나, 이 내용을 지원팀에 전달해 주세요."), registration]);
        return nil;
    }
    return arguments.lastObject;
}

static NSDictionary *JasoInstallerPlan(NSString *payload, NSString *registration, NSString *defaultConfig, NSString **error) {
    if (error) *error = nil;
    NSDictionary *metadata = [NSDictionary dictionaryWithContentsOfFile:[payload stringByAppendingPathComponent:@"Contents/Info.plist"]];
    NSString *executable = [payload stringByAppendingPathComponent:@"Contents/MacOS/jaso-nfc"];
    NSString *menu = [payload stringByAppendingPathComponent:@"Contents/MacOS/Jaso NFC"];
    NSString *version = metadata[@"CFBundleShortVersionString"];
    if (!JasoInstallerAbsolutePath(payload) || ![metadata[@"CFBundleIdentifier"] isEqual:@"io.github.garlicvread.jaso-nfc"] ||
        ![metadata[@"CFBundleExecutable"] isEqual:@"jaso-nfc"] || ![metadata[@"CFBundlePackageType"] isEqual:@"APPL"] ||
        ![version isKindOfClass:NSString.class] || !version.length ||
        ![NSFileManager.defaultManager isExecutableFileAtPath:executable] || ![NSFileManager.defaultManager isExecutableFileAtPath:menu]) {
        JasoInstallerError(error, IL(@"The installer is missing a valid copy of Jaso NFC. Download the installer again and reopen it.", @"설치 프로그램에 포함된 Jaso NFC 앱이 없거나 올바르지 않습니다. 설치 파일을 다시 다운로드해 실행하세요."));
        return nil;
    }
    BOOL menuPresent = NO, workerPresent = NO;
    NSString *menuConfig = JasoInstallerRegisteredConfig(registration, YES, &menuPresent, error);
    if (menuPresent && !menuConfig) return nil;
    NSString *workerRegistration = [registration.stringByDeletingLastPathComponent stringByAppendingPathComponent:@"io.github.garlicvread.jaso-nfc.plist"];
    NSString *workerConfig = JasoInstallerRegisteredConfig(workerRegistration, NO, &workerPresent, error);
    if (workerPresent && !workerConfig) return nil;
    if (menuConfig && workerConfig && ![[menuConfig.stringByStandardizingPath precomposedStringWithCanonicalMapping] isEqual:[workerConfig.stringByStandardizingPath precomposedStringWithCanonicalMapping]]) {
        JasoInstallerError(error, IL(@"The menu and background cleanup use different saved settings files. Check their login startup entries before installing again, or contact support.", @"메뉴와 백그라운드 정리가 서로 다른 설정 파일을 사용합니다. 로그인 실행 항목을 확인한 뒤 다시 설치하거나 지원팀에 문의해 주세요."));
        return nil;
    }
    NSString *config = menuConfig ?: workerConfig ?: defaultConfig;
    BOOL registered = menuPresent || workerPresent;
    if (!JasoInstallerAbsolutePath(config)) {
        JasoInstallerError(error, IL(@"The settings file needs a full path beginning with /. Check the installation settings.", @"설정 파일은 /로 시작하는 전체 경로가 필요합니다. 설치 설정을 확인해 주세요."));
        return nil;
    }
    NSError *readError = nil;
    NSData *configData = [NSData dataWithContentsOfFile:config options:0 error:&readError];
    if (!configData && (registered || !JasoInstallerMissing(readError))) {
        JasoInstallerError(error, [NSString stringWithFormat:IL(@"Could not read the saved settings at %@.\n%@\nCheck this file and its access permissions before installing again.", @"저장된 설정을 읽을 수 없습니다: %@\n%@\n파일과 접근 권한을 확인한 뒤 다시 설치해 주세요."), config, readError.localizedDescription ?: @""]);
        return nil;
    }
    NSMutableArray *arguments = [NSMutableArray arrayWithArray:@[@"install", @"--app", payload]];
    if (configData) [arguments addObjectsFromArray:@[@"--config", config]];
    else [arguments addObjectsFromArray:@[@"--root", [NSHomeDirectory() stringByAppendingPathComponent:@"Downloads"]]];
    return @{@"executable":executable, @"arguments":arguments, @"config":config, @"existing":@(configData != nil), @"version":version};
}

static NSData *JasoInstallerDrain(NSFileHandle *handle) {
    // Continue draining after the diagnostic cap so a verbose child cannot
    // block the installation on a full pipe or grow the UI process unboundedly.
    const NSUInteger limit = 1024 * 1024;
    NSMutableData *captured = [NSMutableData data];
    while (YES) {
        NSData *chunk = [handle readDataOfLength:16384];
        if (!chunk.length) break;
        NSUInteger keep = MIN(chunk.length, limit - captured.length);
        if (keep) [captured appendBytes:chunk.bytes length:keep];
    }
    return captured;
}
static NSDictionary *JasoInstallerRun(NSString *executable, NSArray<NSString *> *arguments) {
    NSTask *task = [NSTask new];
    task.executableURL = [NSURL fileURLWithPath:executable];
    task.arguments = arguments;
    task.standardInput = NSFileHandle.fileHandleWithNullDevice;
    NSPipe *output = NSPipe.pipe, *errors = NSPipe.pipe;
    task.standardOutput = output;
    task.standardError = errors;
    NSError *launchError = nil;
    if (![task launchAndReturnError:&launchError]) {
        return @{@"status":@(-1), @"stdout":NSData.data, @"stderr":[(launchError.localizedDescription ?: @"Could not start the installation. Reopen the installer and try again.") dataUsingEncoding:NSUTF8StringEncoding]};
    }
    __block NSData *stdoutData = nil, *stderrData = nil;
    dispatch_group_t drains = dispatch_group_create();
    dispatch_queue_t queue = dispatch_get_global_queue(QOS_CLASS_UTILITY, 0);
    dispatch_group_async(drains, queue, ^{ stdoutData = JasoInstallerDrain(output.fileHandleForReading); });
    dispatch_group_async(drains, queue, ^{ stderrData = JasoInstallerDrain(errors.fileHandleForReading); });
    [task waitUntilExit];
    dispatch_group_wait(drains, DISPATCH_TIME_FOREVER);
    return @{@"status":@(task.terminationStatus), @"stdout":stdoutData ?: NSData.data, @"stderr":stderrData ?: NSData.data};
}
static NSDictionary *JasoInstallerOutcome(NSData *output, NSData *errors, int status) {
    id result = [NSJSONSerialization JSONObjectWithData:output options:0 error:NULL];
    BOOL valid = [result isKindOfClass:NSDictionary.class] &&
        [result[@"application"] isEqual:@"/Applications/Jaso NFC.app"] &&
        [result[@"recovery_history_retained"] isKindOfClass:NSNumber.class] && [result[@"recovery_history_retained"] boolValue] &&
        [result[@"version"] isKindOfClass:NSString.class] && [result[@"version"] length] > 0;
    if (status == 0 && valid) return @{@"success":@YES, @"detail":@"", @"version":result[@"version"]};
    NSString *detail = [[NSString alloc] initWithData:errors encoding:NSUTF8StringEncoding];
    if (!detail.length) detail = [[NSString alloc] initWithData:output encoding:NSUTF8StringEncoding];
    if (!detail.length) detail = status == 0 ? IL(@"The installer returned an incomplete result. Inspect the installed app and its status before trying again.", @"설치 결과를 모두 확인하지 못했습니다. 다시 시도하기 전에 설치된 앱을 열어 상태를 확인해 주세요.") : [NSString stringWithFormat:IL(@"The installation process exited with status %d.", @"설치가 오류 코드 %d로 종료되었습니다."), status];
    if (detail.length > 12000) detail = [[detail substringToIndex:12000] stringByAppendingString:IL(@"\n[Further diagnostic output omitted.]", @"\n[이후 진단 출력 생략]")];
    return @{@"success":@NO, @"detail":detail};
}

@interface JasoInstaller : NSObject <NSApplicationDelegate, NSWindowDelegate>
@property NSWindow *window;
@property NSTextField *heading;
@property NSTextField *descriptionLabel;
@property NSTextView *details;
@property NSProgressIndicator *progress;
@property NSButton *installButton;
@property NSButton *cancelButton;
@property NSButton *trashButton;
@property NSDictionary *plan;
@property NSDictionary *cleanup;
@property NSString *cleanupError;
@property BOOL busy;
@property BOOL installed;
+ (NSString *)imageSourceFromInfo:(id)info installerPath:(NSString *)installerPath mountPath:(NSString *)mountPath device:(NSString *)device error:(NSString **)error;
+ (NSDictionary *)cleanupFile:(NSString *)path error:(NSString **)error;
- (NSDictionary *)findCleanup:(NSString **)error;
- (void)trashInstaller:(id)sender;
- (void)recycleURL:(NSURL *)url completion:(void (^)(NSDictionary<NSURL *, NSURL *> *, NSError *))completion;
- (void)closeInstaller;
@end

static BOOL JasoInstallerPlainPath(id path) {
    return JasoInstallerAbsolutePath(path) && ![[path pathComponents] containsObject:@".."] && ![[path pathComponents] containsObject:@"."];
}
static BOOL JasoInstallerDiskImagePath(id path) {
    if (!JasoInstallerPlainPath(path) || ![[path pathExtension].lowercaseString isEqual:@"dmg"]) return NO;
    for (NSString *component in [path pathComponents]) if ([component.pathExtension.lowercaseString isEqual:@"app"]) return NO;
    return YES;
}

@implementation JasoInstaller
+ (NSString *)imageSourceFromInfo:(id)info installerPath:(NSString *)installerPath mountPath:(NSString *)mountPath device:(NSString *)device error:(NSString **)error {
    if (error) *error = nil;
    NSString *unavailable = IL(@"Could not locate the original installer DMG. Choose Keep Installer, then manage the file in Finder.", @"원본 설치 DMG의 위치를 확인하지 못했습니다. ‘설치 파일 유지’를 선택한 뒤 Finder에서 파일을 정리하세요.");
    if (!JasoInstallerPlainPath(installerPath) || !JasoInstallerPlainPath(mountPath) || ![mountPath hasPrefix:@"/Volumes/"] ||
        ![installerPath hasPrefix:[mountPath stringByAppendingString:@"/"]] || ![device isKindOfClass:NSString.class] || ![device hasPrefix:@"/dev/disk"] ||
        ![info isKindOfClass:NSDictionary.class] || ![info[@"images"] isKindOfClass:NSArray.class]) {
        JasoInstallerError(error, unavailable);
        return nil;
    }
    NSString *source = nil;
    NSUInteger matches = 0;
    for (id image in info[@"images"]) {
        if (![image isKindOfClass:NSDictionary.class] || ![image[@"system-entities"] isKindOfClass:NSArray.class]) continue;
        for (id entity in image[@"system-entities"]) {
            if (![entity isKindOfClass:NSDictionary.class] || ![entity[@"mount-point"] isEqual:mountPath] || ![entity[@"dev-entry"] isEqual:device]) continue;
            matches++;
            id path = image[@"image-path"];
            if (JasoInstallerDiskImagePath(path) && ![path hasPrefix:[mountPath stringByAppendingString:@"/"]] &&
                [image[@"writeable"] isKindOfClass:NSNumber.class] && ![image[@"writeable"] boolValue]) source = path;
        }
    }
    if (matches != 1 || !source) {
        JasoInstallerError(error, unavailable);
        return nil;
    }
    return source;
}
+ (NSDictionary *)cleanupFile:(NSString *)path error:(NSString **)error {
    if (error) *error = nil;
    struct stat identity;
    if (!JasoInstallerDiskImagePath(path) || lstat(path.fileSystemRepresentation, &identity) != 0 || !S_ISREG(identity.st_mode)) {
        JasoInstallerError(error, IL(@"The original .dmg is missing or has been replaced with another kind of item. Choose Keep Installer and check the file in Finder.", @"원본 .dmg 파일이 없거나 다른 종류의 항목으로 바뀌었습니다. ‘설치 파일 유지’를 선택한 뒤 Finder에서 확인하세요."));
        return nil;
    }
    NSString *canonical = path.stringByResolvingSymlinksInPath;
    if (!JasoInstallerDiskImagePath(canonical)) {
        JasoInstallerError(error, IL(@"The installer DMG points to an unexpected location. Check the file in Finder.", @"설치 DMG가 예상과 다른 위치를 가리킵니다. Finder에서 파일을 확인하세요."));
        return nil;
    }
    return @{@"url":[NSURL fileURLWithPath:canonical], @"identity":@[@(identity.st_dev), @(identity.st_ino), @(identity.st_size), @(identity.st_mtimespec.tv_sec), @(identity.st_mtimespec.tv_nsec)]};
}
- (NSDictionary *)findCleanup:(NSString **)error {
    if (error) *error = nil;
    NSString *bundle = NSBundle.mainBundle.bundlePath.stringByResolvingSymlinksInPath;
    struct statfs volume;
    if (!bundle || statfs(bundle.fileSystemRepresentation, &volume) != 0 || !(volume.f_flags & MNT_RDONLY) ||
        ![[NSString stringWithUTF8String:volume.f_mntonname] hasPrefix:@"/Volumes/"]) {
        JasoInstallerError(error, IL(@"To use Move Installer to Trash, open the downloaded .dmg and run the installer inside it. You can also manage this copy in Finder.", @"‘설치 파일 휴지통 이동’을 사용하려면 다운로드한 .dmg를 열고 그 안의 설치 프로그램을 실행하세요. 현재 복사본은 Finder에서 정리할 수 있습니다."));
        return nil;
    }
    NSDictionary *result = JasoInstallerRun(@"/usr/bin/hdiutil", @[@"info", @"-plist"]);
    id info = [result[@"status"] intValue] == 0 ? [NSPropertyListSerialization propertyListWithData:result[@"stdout"] options:NSPropertyListImmutable format:NULL error:NULL] : nil;
    NSString *source = [JasoInstaller imageSourceFromInfo:info installerPath:bundle mountPath:[NSString stringWithUTF8String:volume.f_mntonname] device:[NSString stringWithUTF8String:volume.f_mntfromname] error:error];
    return source ? [JasoInstaller cleanupFile:source error:error] : nil;
}
- (NSTextField *)label:(NSString *)text size:(CGFloat)size bold:(BOOL)bold {
    NSTextField *label = [NSTextField wrappingLabelWithString:text];
    label.font = bold ? [NSFont boldSystemFontOfSize:size] : [NSFont systemFontOfSize:size];
    label.translatesAutoresizingMaskIntoConstraints = NO;
    label.selectable = YES;
    return label;
}
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    NSMenu *main = [NSMenu new];
    NSMenuItem *appItem = [NSMenuItem new];
    [main addItem:appItem];
    NSMenu *application = [NSMenu new];
    appItem.submenu = application;
    [application addItemWithTitle:IL(@"Quit Jaso NFC Installer", @"Jaso NFC 설치 종료") action:@selector(terminate:) keyEquivalent:@"q"];
    NSApp.mainMenu = main;
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 630, 540) styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
    self.window.title = IL(@"Install Jaso NFC", @"Jaso NFC 설치");
    self.window.delegate = self;
    self.window.releasedWhenClosed = NO;
    NSView *content = self.window.contentView;
    self.heading = [self label:IL(@"Install Jaso NFC", @"Jaso NFC 설치") size:25 bold:YES];
    self.descriptionLabel = [self label:@"" size:14 bold:NO];
    NSTextField *author = [self label:IL(@"Publisher: AidALL Inc. · MIT License\nSupport: aidall_manager@aidall.tech\nSource: github.com/garlicvread/jaso-nfc", @"게시자: AidALL Inc. · MIT 라이선스\n문의: aidall_manager@aidall.tech\n소스: github.com/garlicvread/jaso-nfc") size:12 bold:NO];
    author.textColor = NSColor.secondaryLabelColor;
    NSScrollView *scroll = [NSScrollView new];
    scroll.translatesAutoresizingMaskIntoConstraints = NO;
    scroll.hasVerticalScroller = YES;
    scroll.borderType = NSBezelBorder;
    self.details = [[NSTextView alloc] initWithFrame:NSMakeRect(0, 0, 562, 200)];
    self.details.editable = NO;
    self.details.selectable = YES;
    self.details.richText = NO;
    self.details.font = [NSFont systemFontOfSize:13];
    self.details.textContainerInset = NSMakeSize(10, 10);
    self.details.autoresizingMask = NSViewWidthSizable;
    self.details.textContainer.widthTracksTextView = YES;
    scroll.documentView = self.details;
    self.progress = [NSProgressIndicator new];
    self.progress.translatesAutoresizingMaskIntoConstraints = NO;
    self.progress.style = NSProgressIndicatorStyleBar;
    self.progress.indeterminate = YES;
    self.progress.hidden = YES;
    self.installButton = [NSButton buttonWithTitle:IL(@"Install", @"설치") target:self action:@selector(install:)];
    self.installButton.keyEquivalent = @"\r";
    self.installButton.translatesAutoresizingMaskIntoConstraints = NO;
    self.cancelButton = [NSButton buttonWithTitle:IL(@"Cancel", @"취소") target:self action:@selector(cancel:)];
    self.cancelButton.keyEquivalent = @"\e";
    self.cancelButton.translatesAutoresizingMaskIntoConstraints = NO;
    self.trashButton = [NSButton buttonWithTitle:IL(@"Move Installer to Trash", @"설치 파일 휴지통 이동") target:self action:@selector(trashInstaller:)];
    self.trashButton.translatesAutoresizingMaskIntoConstraints = NO;
    self.trashButton.hidden = YES;
    self.trashButton.enabled = NO;
    for (NSView *view in @[self.heading, self.descriptionLabel, author, scroll, self.progress, self.installButton, self.cancelButton, self.trashButton]) [content addSubview:view];
    [NSLayoutConstraint activateConstraints:@[
        [self.heading.topAnchor constraintEqualToAnchor:content.topAnchor constant:28], [self.heading.leadingAnchor constraintEqualToAnchor:content.leadingAnchor constant:30], [self.heading.trailingAnchor constraintEqualToAnchor:content.trailingAnchor constant:-30],
        [self.descriptionLabel.topAnchor constraintEqualToAnchor:self.heading.bottomAnchor constant:14], [self.descriptionLabel.leadingAnchor constraintEqualToAnchor:self.heading.leadingAnchor], [self.descriptionLabel.trailingAnchor constraintEqualToAnchor:self.heading.trailingAnchor],
        [author.topAnchor constraintEqualToAnchor:self.descriptionLabel.bottomAnchor constant:16], [author.leadingAnchor constraintEqualToAnchor:self.heading.leadingAnchor], [author.trailingAnchor constraintEqualToAnchor:self.heading.trailingAnchor],
        [scroll.topAnchor constraintEqualToAnchor:author.bottomAnchor constant:18], [scroll.leadingAnchor constraintEqualToAnchor:self.heading.leadingAnchor], [scroll.trailingAnchor constraintEqualToAnchor:self.heading.trailingAnchor],
        [scroll.bottomAnchor constraintEqualToAnchor:self.progress.topAnchor constant:-14], [self.progress.leadingAnchor constraintEqualToAnchor:self.heading.leadingAnchor], [self.progress.trailingAnchor constraintEqualToAnchor:self.heading.trailingAnchor],
        [self.progress.bottomAnchor constraintEqualToAnchor:self.installButton.topAnchor constant:-18], [self.progress.heightAnchor constraintEqualToConstant:8],
        [self.installButton.trailingAnchor constraintEqualToAnchor:self.heading.trailingAnchor], [self.installButton.bottomAnchor constraintEqualToAnchor:content.bottomAnchor constant:-24], [self.installButton.widthAnchor constraintGreaterThanOrEqualToConstant:105],
        [self.cancelButton.trailingAnchor constraintEqualToAnchor:self.installButton.leadingAnchor constant:-12], [self.cancelButton.centerYAnchor constraintEqualToAnchor:self.installButton.centerYAnchor], [self.cancelButton.widthAnchor constraintGreaterThanOrEqualToConstant:85],
        [self.trashButton.trailingAnchor constraintEqualToAnchor:self.cancelButton.leadingAnchor constant:-12], [self.trashButton.centerYAnchor constraintEqualToAnchor:self.installButton.centerYAnchor], [self.trashButton.leadingAnchor constraintGreaterThanOrEqualToAnchor:self.heading.leadingAnchor]
    ]];
    [self prepare];
    // Capture the mounted source before installation, then recheck it when the
    // user chooses Trash. A changed path must never select a replacement file.
    NSString *cleanupError = nil;
    self.cleanup = [self findCleanup:&cleanupError];
    self.cleanupError = cleanupError;
    [self.window center];
    [self.window makeKeyAndOrderFront:nil];
    [NSApp activateIgnoringOtherApps:YES];
}
- (void)prepare {
    NSString *payload = [NSBundle.mainBundle.resourcePath stringByAppendingPathComponent:@"Jaso NFC.app"];
    NSString *registration = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/LaunchAgents/io.github.garlicvread.jaso-nfc.menu.plist"];
    NSString *config = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support/jaso-nfc/config.json"];
    NSString *error = nil;
    self.plan = JasoInstallerPlan(payload, registration, config, &error);
    if (!self.plan) {
        self.heading.stringValue = IL(@"Installation unavailable", @"설치를 시작할 수 없음");
        self.descriptionLabel.stringValue = IL(@"Check the details below, then reopen the installer to try again.", @"아래 내용을 확인한 뒤 설치 프로그램을 다시 열어 주세요.");
        self.details.string = error ?: IL(@"The installer could not prepare the application.", @"앱 설치를 준비하지 못했습니다.");
        self.installButton.enabled = NO;
        self.cancelButton.title = IL(@"Close", @"닫기");
        return;
    }
    self.descriptionLabel.stringValue = [NSString stringWithFormat:IL(@"Install version %@ in Applications and open Jaso NFC for this account.", @"응용 프로그램 폴더에 버전 %@을 설치하고 현재 계정에서 Jaso NFC를 실행합니다."), self.plan[@"version"]];
    self.details.string = [self.plan[@"existing"] boolValue] ? [NSString stringWithFormat:IL(@"Continue with your saved folders, cleanup mode, login preference, and recovery history.\n\nSettings: %@\n\nKeep this window open until installation finishes. Then check cleanup activity in Jaso NFC.", @"저장된 폴더, 정리 모드, 로그인 실행 선택과 복구 기록을 이어서 사용합니다.\n\n설정: %@\n\n설치가 끝날 때까지 이 창을 열어 두세요. 완료 후 Jaso NFC에서 정리 상태를 확인하세요."), self.plan[@"config"]] : IL(@"Start with Downloads. After installing, choose your folders in Jaso NFC, preview the names, and start automatic cleanup.\n\nKeep this window open until installation finishes. Any existing recovery history will remain available.", @"다운로드 폴더부터 시작해 보세요. 설치 후 Jaso NFC에서 정리할 폴더를 선택하고, 변경할 이름을 미리 확인한 뒤 자동 정리를 시작하세요.\n\n설치가 끝날 때까지 이 창을 열어 두세요. 기존 복구 기록도 이어서 사용할 수 있습니다.");
}
- (void)install:(id)sender {
    if (self.busy || !self.plan) return;
    if (self.installed) {
        [NSWorkspace.sharedWorkspace openURL:[NSURL fileURLWithPath:@"/Applications/Jaso NFC.app"]];
        [NSApp terminate:nil];
        return;
    }
    // Recheck registration and payload immediately before creating the task.
    [self prepare];
    if (!self.plan) return;
    self.busy = YES;
    self.installButton.enabled = NO;
    self.cancelButton.enabled = NO;
    self.progress.hidden = NO;
    [self.progress startAnimation:nil];
    self.heading.stringValue = IL(@"Installing Jaso NFC…", @"Jaso NFC 설치 중…");
    self.descriptionLabel.stringValue = IL(@"Preparing and opening Jaso NFC. Please keep this window open until installation finishes.", @"Jaso NFC를 준비하고 실행하고 있습니다. 설치가 끝날 때까지 이 창을 열어 두세요.");
    NSDictionary *plan = self.plan;
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
        @autoreleasepool {
            NSDictionary *result = JasoInstallerRun(plan[@"executable"], plan[@"arguments"]);
            NSDictionary *outcome = JasoInstallerOutcome(result[@"stdout"], result[@"stderr"], [result[@"status"] intValue]);
            dispatch_async(dispatch_get_main_queue(), ^{ [self finished:outcome]; });
        }
    });
}
- (void)finished:(NSDictionary *)outcome {
    self.busy = NO;
    [self.progress stopAnimation:nil];
    self.progress.hidden = YES;
    self.cancelButton.enabled = YES;
    self.cancelButton.title = IL(@"Close", @"닫기");
    self.installed = [outcome[@"success"] boolValue];
    self.trashButton.hidden = !self.installed;
    self.trashButton.enabled = self.installed && self.cleanup != nil;
    if (self.installed) {
        self.heading.stringValue = IL(@"Jaso NFC is installed", @"Jaso NFC 설치 완료");
        self.descriptionLabel.stringValue = [NSString stringWithFormat:IL(@"Version %@ · /Applications/Jaso NFC.app", @"버전 %@ · /Applications/Jaso NFC.app"), outcome[@"version"]];
        self.details.string = [self.plan[@"existing"] boolValue] ? IL(@"Jaso NFC is running with your saved folders, cleanup mode, login preference, and recovery history.\n\nOpen Jaso NFC and choose Open Status… to check its activity. Use Manage folders… to review or update the folders you want to clean up. For an access error, open Full Disk Access from Settings and add Jaso NFC from Applications.", @"저장된 폴더, 정리 모드, 로그인 실행 선택과 복구 기록을 이어서 Jaso NFC를 실행했습니다.\n\nJaso NFC를 열고 ‘상태 보기…’에서 활동을 확인하세요. 폴더를 확인하거나 바꾸려면 ‘정리할 폴더…’를 사용하세요. 접근 권한 오류가 표시되면 설정에서 전체 디스크 접근을 열고 응용 프로그램 폴더의 Jaso NFC를 추가하세요.") : IL(@"Open Jaso NFC to start cleaning up Downloads.\n\n1. Open Manage folders… and choose the folders to clean up.\n2. Choose Preview filenames to compare current and proposed names.\n3. Choose Start automatic cleanup.\n\nJaso NFC will clean up those folders and keep checking new files.", @"Jaso NFC를 열고 다운로드 폴더부터 정리해 보세요.\n\n1. ‘정리할 폴더…’에서 원하는 폴더를 선택하세요.\n2. ‘파일명 미리보기’에서 현재 이름과 정리 후 이름을 확인하세요.\n3. ‘자동 정리 시작’을 누르세요.\n\n선택한 폴더를 정리하고 이후 들어오는 파일도 계속 확인합니다.");
        self.installButton.title = IL(@"Open Jaso NFC", @"Jaso NFC 열기");
        self.installButton.enabled = YES;
        self.installButton.keyEquivalent = @"";
        self.cancelButton.title = IL(@"Keep Installer", @"설치 파일 유지");
        self.cancelButton.keyEquivalent = @"\r";
        NSString *cleanupDetail = self.cleanup ? [NSString stringWithFormat:IL(@"Choose Keep Installer or move this original disk image to Trash:\n%@\n\nAfter closing the installer, eject its mounted disk in Finder.", @"설치 파일을 유지하거나 아래 원본 DMG를 휴지통으로 옮기세요:\n%@\n\n설치 창을 닫은 뒤 Finder에서 설치 디스크를 추출하세요."), [self.cleanup[@"url"] path]] : self.cleanupError;
        self.details.string = [self.details.string stringByAppendingFormat:@"\n\n%@", cleanupDetail ?: @""];
    } else {
        self.heading.stringValue = IL(@"Installation could not finish", @"설치를 완료할 수 없음");
        self.descriptionLabel.stringValue = IL(@"Review the details below before retrying.\nKeep your configuration, backups and recovery history.", @"아래 내용을 확인한 뒤 다시 시도하세요.\n설정, 백업과 복구 기록은 보관해 주세요.");
        self.details.string = outcome[@"detail"] ?: @"";
        self.installButton.title = IL(@"Try Again", @"다시 시도");
        self.installButton.enabled = YES;
    }
}
- (void)trashInstaller:(id)sender {
    if (self.busy || !self.installed || !self.cleanup) return;
    NSString *error = nil;
    NSDictionary *current = [self findCleanup:&error];
    if (![current isEqual:self.cleanup]) {
        self.cleanup = nil;
        self.trashButton.enabled = NO;
        self.details.string = [self.details.string stringByAppendingFormat:@"\n\n%@", error ?: IL(@"The installer DMG changed after this window opened, so the Trash action was skipped. Check and manage the file in Finder.", @"설치 창을 연 뒤 DMG가 바뀌어 휴지통 이동을 건너뛰었습니다. Finder에서 파일을 확인하고 정리하세요.")];
        return;
    }
    NSURL *source = self.cleanup[@"url"];
    self.busy = YES;
    self.installButton.enabled = NO;
    self.cancelButton.enabled = NO;
    self.trashButton.enabled = NO;
    self.progress.hidden = NO;
    [self.progress startAnimation:nil];
    [self recycleURL:source completion:^(NSDictionary<NSURL *, NSURL *> *newURLs, NSError *recycleError) {
        dispatch_async(dispatch_get_main_queue(), ^{
            self.busy = NO;
            [self.progress stopAnimation:nil];
            self.progress.hidden = YES;
            self.installButton.enabled = YES;
            self.cancelButton.enabled = YES;
            NSURL *destination = newURLs[source];
            BOOL moved = !recycleError && [destination isKindOfClass:NSURL.class] && destination.isFileURL;
            if (moved) {
                self.cleanup = nil;
                self.cancelButton.title = IL(@"Close", @"닫기");
                self.trashButton.title = IL(@"Moved to Trash", @"휴지통 이동 완료");
                self.details.string = [self.details.string stringByAppendingString:IL(@"\n\nThe original installer disk image was moved to Trash. Jaso NFC is installed. Close this installer, then eject its mounted disk in Finder.", @"\n\n원본 설치 DMG를 휴지통으로 이동했습니다. Jaso NFC 설치가 완료되었습니다. 설치 창을 닫은 뒤 Finder에서 설치 디스크를 추출하세요.")];
            } else {
                self.trashButton.enabled = YES;
                self.details.string = [self.details.string stringByAppendingFormat:IL(@"\n\nJaso NFC is installed, but the installer disk image could not be moved to Trash.\n%@\nKeep the installer or try again.", @"\n\nJaso NFC 설치가 완료되었습니다. 설치 DMG의 휴지통 이동 중 오류가 발생했습니다.\n%@\n설치 파일을 유지하거나 다시 시도하세요."), recycleError.localizedDescription ?: IL(@"Finder did not confirm a Trash destination.", @"Finder에서 휴지통 이동 위치를 확인하지 못했습니다.")];
            }
        });
    }];
}
- (void)recycleURL:(NSURL *)url completion:(void (^)(NSDictionary<NSURL *, NSURL *> *, NSError *))completion {
    [NSWorkspace.sharedWorkspace recycleURLs:@[url] completionHandler:completion];
}
- (void)closeInstaller { [NSApp terminate:nil]; }
- (void)cancel:(id)sender { if (!self.busy) [self closeInstaller]; }
- (BOOL)windowShouldClose:(NSWindow *)sender { if (self.busy) { NSBeep(); return NO; } [self closeInstaller]; return NO; }
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender { if (self.busy) { NSBeep(); return NSTerminateCancel; } return NSTerminateNow; }
- (BOOL)applicationShouldHandleReopen:(NSApplication *)sender hasVisibleWindows:(BOOL)visible { [self.window makeKeyAndOrderFront:nil]; return YES; }
@end

#ifndef JASO_INSTALLER_TESTING
int main(void) {
    @autoreleasepool {
        NSApplication *app = NSApplication.sharedApplication;
        [app setActivationPolicy:NSApplicationActivationPolicyRegular];
        JasoInstaller *delegate = [JasoInstaller new];
        app.delegate = delegate;
        [app run];
    }
    return 0;
}
#endif
