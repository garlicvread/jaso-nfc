#define JASO_INSTALLER_TESTING 1
#import "../macos/Installer.m"

static NSUInteger Cases = 0;
static NSUInteger Failures = 0;
static void Require(BOOL condition, NSString *message) {
    if (!condition) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static void Test(NSString *name, void (^body)(void)) {
    Cases++;
    @try { body(); fprintf(stdout, "PASS %s\n", name.UTF8String); }
    @catch (NSException *failure) { Failures++; fprintf(stderr, "FAIL %s: %s\n", name.UTF8String, failure.reason.UTF8String); }
}
static void WriteRegistration(NSString *path, id arguments) {
    [@{@"Label":@"io.github.garlicvread.jaso-nfc.menu", @"ProgramArguments":arguments} writeToFile:path atomically:YES];
}
static NSString *FixturePayload(NSString *base) {
    NSString *app = [base stringByAppendingPathComponent:@"자소 ' ; $(fixture) NFC.app"];
    NSString *macos = [app stringByAppendingPathComponent:@"Contents/MacOS"];
    [NSFileManager.defaultManager createDirectoryAtPath:macos withIntermediateDirectories:YES attributes:nil error:NULL];
    [@{@"CFBundleIdentifier":@"io.github.garlicvread.jaso-nfc", @"CFBundleExecutable":@"jaso-nfc", @"CFBundlePackageType":@"APPL", @"CFBundleShortVersionString":@"0.4.0"} writeToFile:[app stringByAppendingPathComponent:@"Contents/Info.plist"] atomically:YES];
    for (NSString *name in @[@"jaso-nfc", @"Jaso NFC"]) {
        NSString *file = [macos stringByAppendingPathComponent:name];
        [@"#!/bin/sh\nexit 0\n" writeToFile:file atomically:YES encoding:NSUTF8StringEncoding error:NULL];
        [NSFileManager.defaultManager setAttributes:@{NSFilePosixPermissions:@0755} ofItemAtPath:file error:NULL];
    }
    return app;
}
static NSData *Data(NSString *value) { return [value dataUsingEncoding:NSUTF8StringEncoding]; }

@interface JasoInstaller (CleanupContract)
+ (NSString *)imageSourceFromInfo:(id)info installerPath:(NSString *)installerPath mountPath:(NSString *)mountPath device:(NSString *)device error:(NSString **)error;
+ (NSDictionary *)cleanupFile:(NSString *)path error:(NSString **)error;
@property NSButton *trashButton;
- (NSDictionary *)findCleanup:(NSString **)error;
- (void)trashInstaller:(id)sender;
- (void)recycleURL:(NSURL *)url completion:(void (^)(NSDictionary<NSURL *, NSURL *> *, NSError *))completion;
- (void)closeInstaller;
@end

@interface CleanupInstaller : JasoInstaller
@property NSDictionary *fixtureCleanup;
@property NSURL *recycledURL;
@property NSUInteger recycleCount;
@property BOOL closed;
@property (copy) void (^recycleCompletion)(NSDictionary<NSURL *, NSURL *> *, NSError *);
@end
@implementation CleanupInstaller
- (NSDictionary *)findCleanup:(NSString **)error {
    if (error) *error = self.fixtureCleanup ? nil : @"Open the installer from its disk image to enable cleanup.";
    return self.fixtureCleanup;
}
- (void)recycleURL:(NSURL *)url completion:(void (^)(NSDictionary<NSURL *, NSURL *> *, NSError *))completion {
    self.recycledURL = url;
    self.recycleCount++;
    self.recycleCompletion = completion;
}
- (void)closeInstaller { self.closed = YES; }
@end

static NSDictionary *DiskInfo(NSString *source, NSString *mount, NSString *device) {
    return @{@"images":@[@{@"image-path":source, @"writeable":@NO, @"system-entities":@[@{@"mount-point":mount, @"dev-entry":device}]}]};
}
static CleanupInstaller *CleanupWindow(NSDictionary *cleanup) {
    [NSApplication sharedApplication];
    CleanupInstaller *installer = [CleanupInstaller new];
    installer.fixtureCleanup = cleanup;
    [installer applicationDidFinishLaunching:[NSNotification notificationWithName:NSApplicationDidFinishLaunchingNotification object:NSApp]];
    installer.plan = @{@"existing":@NO};
    return installer;
}
static void WaitForRecycleCompletion(CleanupInstaller *installer) {
    NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 2;
    while (installer.busy && NSProcessInfo.processInfo.systemUptime < deadline) {
        [NSRunLoop.currentRunLoop runMode:NSDefaultRunLoopMode beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
    }
    Require(!installer.busy, @"Timed out waiting for the recycle completion callback");
}
static void CompleteRecycleLater(CleanupInstaller *installer, NSDictionary<NSURL *, NSURL *> *urls, NSError *error) {
    // Keep both outcomes slower than the previous 0.1-second queue drain.
    void (^completion)(NSDictionary<NSURL *, NSURL *> *, NSError *) = installer.recycleCompletion;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 250 * NSEC_PER_MSEC), dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
        completion(urls, error);
    });
}

int main(void) {
    @autoreleasepool {
        NSString *directory = [NSTemporaryDirectory() stringByAppendingPathComponent:[@"jaso-installer-test-" stringByAppendingString:NSUUID.UUID.UUIDString]];
        [NSFileManager.defaultManager createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:nil error:NULL];
        @try {
            NSString *payload = FixturePayload(directory);
            NSString *registration = [directory stringByAppendingPathComponent:@"menu.plist"];
            NSString *workerRegistration = [directory stringByAppendingPathComponent:@"io.github.garlicvread.jaso-nfc.plist"];
            NSString *defaultConfig = [directory stringByAppendingPathComponent:@"default.json"];
            NSString *customConfig = [directory stringByAppendingPathComponent:@"custom ' ; $(fixture) 설정.json"];
            Test(@"fresh installation previews Downloads only", ^{
                NSString *error = nil;
                NSDictionary *plan = JasoInstallerPlan(payload, registration, defaultConfig, &error);
                Require(plan && !error, error ?: @"No plan returned");
                Require([plan[@"arguments"] isEqual:@[@"install", @"--app", payload, @"--root", [NSHomeDirectory() stringByAppendingPathComponent:@"Downloads"]]], @"Fresh install must select Downloads and leave automatic cleanup disabled");
                Require(![plan[@"existing"] boolValue] && [plan[@"version"] isEqual:@"0.4.0"], @"Fresh mode or payload version is wrong");
            });
            Test(@"registered custom config remains one literal argument", ^{
                [@"{}" writeToFile:customConfig atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                WriteRegistration(registration, @[@"/Applications/Jaso NFC.app/Contents/MacOS/Jaso NFC", @"--config", customConfig]);
                NSString *error = nil;
                NSDictionary *plan = JasoInstallerPlan(payload, registration, defaultConfig, &error);
                Require([plan[@"arguments"] isEqual:@[@"install", @"--app", payload, @"--config", customConfig]], error ?: @"Existing arguments were changed");
                Require([plan[@"existing"] boolValue], @"Existing config was mistaken for a fresh install");
            });
            Test(@"malformed registration never falls back", ^{
                for (id arguments in @[@[], @[@"/menu", @"--config", @"relative.json"], @[@"/menu", @"--config", customConfig, @"extra"]]) {
                    WriteRegistration(registration, arguments);
                    NSString *error = nil;
                    Require(!JasoInstallerPlan(payload, registration, defaultConfig, &error) && error.length, @"Malformed registration selected a fallback");
                }
                [@"broken plist" writeToFile:registration atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                NSString *error = nil;
                Require(!JasoInstallerPlan(payload, registration, defaultConfig, &error) && error.length, @"Broken plist selected a fallback");
            });
            Test(@"missing registered config stops before installation", ^{
                WriteRegistration(registration, @[@"/menu", @"--config", [directory stringByAppendingPathComponent:@"missing.json"]]);
                NSString *error = nil;
                Require(!JasoInstallerPlan(payload, registration, defaultConfig, &error) && error.length, @"Missing custom state silently became a fresh install");
            });
            Test(@"existing default config is retained without registration", ^{
                [NSFileManager.defaultManager removeItemAtPath:registration error:NULL];
                [@"{}" writeToFile:defaultConfig atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                NSString *error = nil;
                NSDictionary *plan = JasoInstallerPlan(payload, registration, defaultConfig, &error);
                Require([plan[@"arguments"] isEqual:@[@"install", @"--app", payload, @"--config", defaultConfig]], error ?: @"Saved default config was ignored");
            });
            Test(@"worker-only native and Python registrations preserve custom state", ^{
                for (NSArray *arguments in @[@[@"/Applications/Jaso NFC.app/Contents/MacOS/jaso-nfc", @"run", @"--config", customConfig], @[@"/Applications/Jaso NFC.app/Contents/MacOS/jaso-nfc", @"watch", @"--config", customConfig], @[@"/usr/bin/python3", @"/custom/releases/0.3/run.py", @"watch", @"--config", customConfig]]) {
                    [@{@"Label":@"io.github.garlicvread.jaso-nfc", @"ProgramArguments":arguments} writeToFile:workerRegistration atomically:YES];
                    NSString *error = nil;
                    NSDictionary *plan = JasoInstallerPlan(payload, registration, defaultConfig, &error);
                    Require([plan[@"arguments"] isEqual:@[@"install", @"--app", payload, @"--config", customConfig]], error ?: @"Worker custom config was replaced with default state");
                }
                [NSFileManager.defaultManager removeItemAtPath:workerRegistration error:NULL];
            });
            Test(@"both registrations must agree and malformed workers cannot fall back", ^{
                WriteRegistration(registration, @[@"/menu", @"--config", customConfig]);
                [@{@"Label":@"io.github.garlicvread.jaso-nfc", @"ProgramArguments":@[@"/worker", @"watch", @"--config", customConfig]} writeToFile:workerRegistration atomically:YES];
                NSString *error = nil;
                Require(JasoInstallerPlan(payload, registration, defaultConfig, &error) != nil && !error, @"Matching registrations were rejected");
                for (id arguments in @[@[@"/worker", @"watch", @"--config", defaultConfig], @[@"/worker", @"watch", @"--config", @"relative.json"], @[@"/worker", @"--config", customConfig]]) {
                    [@{@"Label":@"io.github.garlicvread.jaso-nfc", @"ProgramArguments":arguments} writeToFile:workerRegistration atomically:YES];
                    error = nil;
                    Require(!JasoInstallerPlan(payload, registration, defaultConfig, &error) && error.length, @"Conflicting or malformed worker registration was ignored");
                }
                [NSFileManager.defaultManager removeItemAtPath:registration error:NULL];
                [@"broken worker plist" writeToFile:workerRegistration atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                error = nil;
                Require(!JasoInstallerPlan(payload, registration, defaultConfig, &error) && error.length, @"Malformed worker-only registration selected default state");
                [NSFileManager.defaultManager removeItemAtPath:workerRegistration error:NULL];
            });
            Test(@"payload identity is required", ^{
                NSString *info = [payload stringByAppendingPathComponent:@"Contents/Info.plist"];
                NSDictionary *saved = [NSDictionary dictionaryWithContentsOfFile:info];
                [@{@"CFBundleIdentifier":@"unrelated.app"} writeToFile:info atomically:YES];
                NSString *error = nil;
                Require(!JasoInstallerPlan(payload, registration, defaultConfig, &error) && error.length, @"An unrelated payload was accepted");
                [saved writeToFile:info atomically:YES];
            });
            Test(@"success requires the transaction's complete result", ^{
                NSDictionary *result = JasoInstallerOutcome(Data(@"{\"application\":\"/Applications/Jaso NFC.app\",\"recovery_history_retained\":true,\"version\":\"0.4.0\"}"), Data(@""), 0);
                Require([result[@"success"] boolValue], @"Valid transaction result failed");
                for (NSString *output in @[@"", @"{}", @"{\"application\":\"/elsewhere.app\",\"recovery_history_retained\":true}"]) {
                    Require(![JasoInstallerOutcome(Data(output), Data(@""), 0)[@"success"] boolValue], @"Incomplete success was accepted");
                }
            });
            Test(@"transaction failures preserve actionable diagnostics", ^{
                NSDictionary *result = JasoInstallerOutcome(Data(@"{}"), Data(@"cannot write to /Applications; rollback restored previous jobs"), 1);
                Require(![result[@"success"] boolValue] && [result[@"detail"] containsString:@"rollback restored previous jobs"], @"Failure lost its recovery detail");
            });
            Test(@"task arguments stay literal and stdout and stderr both drain", ^{
                NSString *script = [directory stringByAppendingPathComponent:@"fixture.sh"];
                NSString *body = @"#!/bin/sh\nprintf '%s\\n' \"$1\"\ni=0\nwhile [ \"$i\" -lt 3000 ]; do printf 'out0123456789012345678901234567890123456789\\n'; printf 'err0123456789012345678901234567890123456789\\n' >&2; i=$((i + 1)); done\nexit 7\n";
                [body writeToFile:script atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                [NSFileManager.defaultManager setAttributes:@{NSFilePosixPermissions:@0755} ofItemAtPath:script error:NULL];
                NSString *literal = @"literal ' ; $(not-a-command) 자소";
                NSDictionary *result = JasoInstallerRun(script, @[literal]);
                Require([result[@"status"] intValue] == 7, @"Task did not return its actual exit status");
                NSString *out = [[NSString alloc] initWithData:result[@"stdout"] encoding:NSUTF8StringEncoding];
                NSString *err = [[NSString alloc] initWithData:result[@"stderr"] encoding:NSUTF8StringEncoding];
                // The fixture reports decomposed Hangul in argv; compare
                // canonical Unicode equivalents while preserving shell syntax.
                Require([[out precomposedStringWithCanonicalMapping] hasPrefix:[literal stringByAppendingString:@"\n"]] && out.length > 100000 && err.length > 100000, @"Argument was interpreted or a pipe did not drain");
            });
            Test(@"launch failure is a normal error result", ^{
                NSDictionary *result = JasoInstallerRun([directory stringByAppendingPathComponent:@"absent"], @[]);
                Require([result[@"status"] intValue] != 0 && [result[@"stderr"] length] > 0, @"Launch failure was reported as success");
            });
            Test(@"cleanup maps only the installer's exact mounted disk image", ^{
                NSString *mount = @"/Volumes/Jaso NFC Installer", *device = @"/dev/disk99s1";
                NSString *app = [mount stringByAppendingPathComponent:@"Jaso NFC Installer.app"];
                NSString *source = [directory stringByAppendingPathComponent:@"installer ' ; $(literal).dmg"];
                NSString *error = nil;
                NSMutableDictionary *info = [DiskInfo(source, mount, device) mutableCopy];
                info[@"images"] = [info[@"images"] arrayByAddingObjectsFromArray:DiskInfo(@"/tmp/unrelated.dmg", @"/Volumes/Other", @"/dev/disk98s1")[@"images"]];
                Require([[JasoInstaller imageSourceFromInfo:info installerPath:app mountPath:mount device:device error:&error] isEqual:source] && !error, @"The exact disk image source was not selected literally");
                for (id invalid in @[@{}, @{@"images":@"invalid"}, DiskInfo(source, mount, @"/dev/disk98s1"), DiskInfo(source, @"/Volumes/Jaso NFC Installer 2", device), @{@"images":[info[@"images"] arrayByAddingObject:info[@"images"][0]]}]) {
                    error = nil;
                    Require(![JasoInstaller imageSourceFromInfo:invalid installerPath:app mountPath:mount device:device error:&error] && error.length, @"An absent, malformed or ambiguous mapping was accepted");
                }
            });
            Test(@"cleanup rejects folders, app bundles, unexpected extensions and mount escapes", ^{
                NSString *mount = @"/Volumes/Installer", *device = @"/dev/disk99s1";
                NSString *app = [mount stringByAppendingPathComponent:@"Jaso NFC Installer.app"];
                for (NSString *source in @[@"relative.dmg", @"https://example.com/install.dmg", @"/Applications/Jaso NFC.app", @"/Users/test/repository", @"/tmp/installer.iso", @"/Volumes/Installer/embedded.dmg", @"/Applications/Other.app/Contents/embedded.dmg", @"/tmp/../tmp/install.dmg"]) {
                    NSString *error = nil;
                    Require(![JasoInstaller imageSourceFromInfo:DiskInfo(source, mount, device) installerPath:app mountPath:mount device:device error:&error] && error.length, @"An unexpected source was accepted");
                }
                for (NSString *outside in @[@"/Applications/Jaso NFC Installer.app", @"/Volumes/Installer-copy/Jaso NFC Installer.app", @"/Volumes/Installer/../Other/Installer.app"]) {
                    NSString *error = nil;
                    Require(![JasoInstaller imageSourceFromInfo:DiskInfo(@"/tmp/installer.dmg", mount, device) installerPath:outside mountPath:mount device:device error:&error] && error.length, @"A folder launch or path prefix collision was accepted");
                }
            });
            Test(@"cleanup file identity rejects missing sources, directories and symlinks", ^{
                NSString *source = [directory stringByAppendingPathComponent:@"fixture.dmg"];
                [Data(@"owned fixture") writeToFile:source atomically:YES];
                NSString *error = nil;
                NSDictionary *identity = [JasoInstaller cleanupFile:source error:&error];
                Require(identity && !error && [identity[@"url"] isEqual:[NSURL fileURLWithPath:source.stringByResolvingSymlinksInPath]], @"Owned regular image was rejected");
                Require([identity isEqual:[JasoInstaller cleanupFile:source error:&error]], @"Unchanged image lost identity");
                [Data(@"replaced fixture with different contents") writeToFile:source atomically:YES];
                Require(![identity isEqual:[JasoInstaller cleanupFile:source error:&error]], @"Replaced source kept the old identity");
                NSString *link = [directory stringByAppendingPathComponent:@"link.dmg"];
                [NSFileManager.defaultManager createSymbolicLinkAtPath:link withDestinationPath:source error:NULL];
                NSString *folder = [directory stringByAppendingPathComponent:@"folder.dmg"];
                [NSFileManager.defaultManager createDirectoryAtPath:folder withIntermediateDirectories:YES attributes:nil error:NULL];
                for (NSString *invalid in @[link, folder, [directory stringByAppendingPathComponent:@"missing.dmg"], @"/Applications/Jaso NFC.app"]) {
                    error = nil;
                    Require(![JasoInstaller cleanupFile:invalid error:&error] && error.length, @"An unsafe cleanup target was accepted");
                }
            });
            Test(@"cleanup is an explicit completion choice and keeping does not recycle", ^{
                NSDictionary *cleanup = @{@"url":[NSURL fileURLWithPath:@"/tmp/owned-installer.dmg"], @"identity":@1};
                CleanupInstaller *installer = CleanupWindow(cleanup);
                Require(installer.trashButton.hidden, @"Cleanup was available before successful installation");
                [installer trashInstaller:nil];
                Require(installer.recycleCount == 0, @"Uninstalled state could recycle the installer");
                [installer finished:@{@"success":@NO, @"detail":@"failed"}];
                Require(installer.trashButton.hidden && installer.recycleCount == 0, @"Failure offered installer removal");
                [installer finished:@{@"success":@YES, @"version":@"0.4.0"}];
                Require(!installer.trashButton.hidden && installer.trashButton.enabled && installer.recycleCount == 0, @"Success did not offer an explicit cleanup choice");
                Require([installer.cancelButton.title isEqual:IL(@"Keep Installer", @"설치 파일 유지")] && [installer.details.string containsString:@"/tmp/owned-installer.dmg"], @"The completion choice omitted Keep or its exact target");
                [installer cancel:nil];
                Require(installer.closed && installer.recycleCount == 0, @"Keep changed a file");
                [installer.window orderOut:nil];
            });
            Test(@"normal folder launches disable cleanup with a reason", ^{
                CleanupInstaller *installer = CleanupWindow(nil);
                [installer finished:@{@"success":@YES, @"version":@"0.4.0"}];
                Require(!installer.trashButton.hidden && !installer.trashButton.enabled && [installer.details.string containsString:@"Open the installer from its disk image"], @"Unavailable cleanup was not explained");
                [installer trashInstaller:nil];
                Require(installer.recycleCount == 0, @"A folder launch recycled a file");
                [installer.window orderOut:nil];
            });
            Test(@"cleanup rechecks source identity before invoking Trash", ^{
                CleanupInstaller *installer = CleanupWindow(@{@"url":[NSURL fileURLWithPath:@"/tmp/owned-installer.dmg"], @"identity":@1});
                [installer finished:@{@"success":@YES, @"version":@"0.4.0"}];
                installer.fixtureCleanup = @{@"url":[NSURL fileURLWithPath:@"/tmp/owned-installer.dmg"], @"identity":@2};
                [installer trashInstaller:nil];
                Require(installer.recycleCount == 0 && !installer.trashButton.enabled && installer.cancelButton.enabled && !installer.busy, @"A replaced image was recycled or blocked close");
                [installer.window orderOut:nil];
            });
            Test(@"Trash completion restores controls and reports success or failure", ^{
                NSURL *url = [NSURL fileURLWithPath:@"/tmp/owned-installer.dmg"];
                CleanupInstaller *installer = CleanupWindow(@{@"url":url, @"identity":@1});
                [installer finished:@{@"success":@YES, @"version":@"0.4.0"}];
                [installer trashInstaller:nil];
                Require(installer.recycleCount == 1 && [installer.recycledURL isEqual:url] && installer.busy && !installer.cancelButton.enabled && !installer.installButton.enabled && !installer.trashButton.enabled, @"Trash did not receive exactly the approved image or protect its operation");
                [installer trashInstaller:nil];
                Require(installer.recycleCount == 1 && [installer applicationShouldTerminate:NSApp] == NSTerminateCancel, @"Repeated cleanup or quit interrupted recycling");
                CompleteRecycleLater(installer, @{}, [NSError errorWithDomain:NSCocoaErrorDomain code:NSFileWriteNoPermissionError userInfo:@{NSLocalizedDescriptionKey:@"Trash permission denied"}]);
                WaitForRecycleCompletion(installer);
                Require(!installer.busy && installer.installed && installer.trashButton.enabled && installer.cancelButton.enabled && installer.installButton.enabled && [installer.details.string containsString:@"Trash permission denied"], @"Cleanup failure lost the installed state, error or retry choice");
                [installer trashInstaller:nil];
                CompleteRecycleLater(installer, @{url:[NSURL fileURLWithPath:@"/tmp/fixture-trash/owned-installer.dmg"]}, nil);
                WaitForRecycleCompletion(installer);
                Require(!installer.busy && installer.installed && !installer.trashButton.enabled && installer.cancelButton.enabled && installer.installButton.enabled && [installer.details.string containsString:IL(@"moved to Trash", @"휴지통으로 이동")], @"Cleanup success was not reflected in the controls");
                [installer.window orderOut:nil];
            });
            Test(@"window reports failure and success and refuses quit during activation", ^{
                [NSApplication sharedApplication];
                JasoInstaller *installer = [JasoInstaller new];
                [installer applicationDidFinishLaunching:[NSNotification notificationWithName:NSApplicationDidFinishLaunchingNotification object:NSApp]];
                Require(installer.window && !installer.installButton.enabled, @"Missing payload must leave the install action disabled");
                installer.busy = YES;
                Require([installer applicationShouldTerminate:NSApp] == NSTerminateCancel, @"Quit could interrupt the activation transaction");
                installer.plan = @{@"existing":@NO};
                [installer finished:@{@"success":@NO, @"detail":@"Cannot write /Applications; rollback restored jobs"}];
                Require(!installer.busy && !installer.installed && installer.installButton.enabled && [installer.details.string containsString:@"rollback restored jobs"], @"Failure presentation is incomplete");
                [installer finished:@{@"success":@YES, @"version":@"0.4.0"}];
                Require(installer.installed && installer.installButton.enabled && [installer.descriptionLabel.stringValue containsString:@"0.4.0"] && [installer applicationShouldTerminate:NSApp] == NSTerminateNow, @"Successful installation cannot be opened or closed");
                [installer.window orderOut:nil];
            });
        } @finally { [NSFileManager.defaultManager removeItemAtPath:directory error:NULL]; }
        fprintf(stdout, "%lu installer cases, %lu failures\n", (unsigned long)Cases, (unsigned long)Failures);
        return Failures ? 1 : 0;
    }
}
