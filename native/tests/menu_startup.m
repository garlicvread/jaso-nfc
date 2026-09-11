// Compile with Menu.m's companion AnimatedMark.m and the AppKit, QuartzCore,
// and CoreText frameworks. Every subprocess and file belongs to this test.
#define main JasoProductionMenuMain
#import "../macos/Menu.m"
#undef main
#import <signal.h>

static void Require(BOOL condition, NSString *message) {
    if (!condition) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}

static BOOL WaitUntil(NSTimeInterval seconds, BOOL (^condition)(void)) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:seconds];
    do {
        if (condition()) return YES;
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    } while (deadline.timeIntervalSinceNow > 0);
    return condition();
}

static NSDictionary *ReadRecord(NSString *path) {
    NSData *data = [NSData dataWithContentsOfFile:path];
    id value = data ? [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL] : nil;
    return [value isKindOfClass:NSDictionary.class] ? value : nil;
}

@interface MenuStartupFixture : NSObject <NSApplicationDelegate>
@property NSString *recordPath;
@end

@implementation MenuStartupFixture
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    NSDictionary *record = @{@"pid":@(getpid()), @"arguments":NSProcessInfo.processInfo.arguments};
    NSData *data = [NSJSONSerialization dataWithJSONObject:record options:0 error:NULL];
    if (![data writeToFile:self.recordPath atomically:YES]) _exit(2);
    // A failed test cannot leave a persistent fixture behind.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 60 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        [NSApp terminate:nil];
    });
}
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)application {
    [@"called" writeToFile:[self.recordPath stringByAppendingString:@".delegate-termination"] atomically:YES encoding:NSUTF8StringEncoding error:NULL];
    return NSTerminateCancel;
}

@end

static int RunFixture(NSArray<NSString *> *arguments) {
    if (arguments.count < 3 || ![arguments[1] isEqual:@"--fixture"]) return 2;
    NSApplication *app = NSApplication.sharedApplication;
    [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
    __attribute__((objc_precise_lifetime)) MenuStartupFixture *delegate = [MenuStartupFixture new];
    delegate.recordPath = arguments[2];
    app.delegate = delegate;
    [app run];
    return 0;
}

@interface OwnedFixture : NSObject
@property NSTask *task;
@property NSRunningApplication *app;
@property NSString *executable;
@property NSString *recordPath;
@property NSDictionary *record;
@end
@implementation OwnedFixture
@end

static NSString *CreateFixtureApp(NSString *directory) {
    NSFileManager *files = NSFileManager.defaultManager;
    NSString *contents = [directory stringByAppendingPathComponent:@"Menu Startup Fixture.app/Contents"];
    NSString *macOS = [contents stringByAppendingPathComponent:@"MacOS"];
    NSError *error = nil;
    Require([files createDirectoryAtPath:macOS withIntermediateDirectories:YES attributes:nil error:&error], error.localizedDescription);
    NSString *executable = [macOS stringByAppendingPathComponent:@"MenuStartupFixture"];
    NSString *selfPath = NSProcessInfo.processInfo.arguments.firstObject.stringByStandardizingPath;
    if (!selfPath.isAbsolutePath) selfPath = [files.currentDirectoryPath stringByAppendingPathComponent:selfPath];
    Require([files copyItemAtPath:selfPath toPath:executable error:&error], error.localizedDescription);
    Require([files setAttributes:@{NSFilePosixPermissions:@0755} ofItemAtPath:executable error:&error], error.localizedDescription);
    NSDictionary *plist = @{@"CFBundleIdentifier":[@"io.github.garlicvread.jaso-nfc.tests." stringByAppendingString:NSUUID.UUID.UUIDString],
                            @"CFBundleExecutable":@"MenuStartupFixture", @"CFBundleName":@"Menu Startup Fixture",
                            @"CFBundlePackageType":@"APPL", @"LSUIElement":@YES};
    Require([plist writeToFile:[contents stringByAppendingPathComponent:@"Info.plist"] atomically:YES], @"Could not write fixture Info.plist");
    return executable.stringByResolvingSymlinksInPath;
}

static OwnedFixture *LaunchFixture(NSString *executable, NSString *recordPath, NSString *customArgument) {
    OwnedFixture *fixture = [OwnedFixture new];
    fixture.executable = executable;
    fixture.recordPath = recordPath;
    fixture.task = [NSTask new];
    fixture.task.executableURL = [NSURL fileURLWithPath:executable];
    fixture.task.arguments = @[@"--fixture", recordPath, customArgument, @"--config", [recordPath stringByAppendingString:@".config.json"]];
    NSError *error = nil;
    Require([fixture.task launchAndReturnError:&error], error.localizedDescription);
    BOOL ready = WaitUntil(8, ^BOOL {
        NSDictionary *record = ReadRecord(recordPath);
        if ([record[@"pid"] intValue] != fixture.task.processIdentifier) return NO;
        fixture.app = [NSRunningApplication runningApplicationWithProcessIdentifier:fixture.task.processIdentifier];
        fixture.record = record;
        return fixture.app != nil && fixture.app.finishedLaunching;
    });
    if (!ready && fixture.task.running) [fixture.task terminate];
    Require(ready, @"Owned fixture did not finish launching");
    return fixture;
}

static BOOL FixtureAlive(OwnedFixture *fixture) {
    return !fixture.app.terminated && kill(fixture.app.processIdentifier, 0) == 0;
}

static void CleanFixture(OwnedFixture *fixture) {
    // Never enumerate apps. Only touch this fixture's exact known PID and path.
    if (!fixture || fixture.app.terminated) return;
    if (![fixture.app.executableURL.path.stringByResolvingSymlinksInPath isEqual:fixture.executable]) return;
    [fixture.app terminate];
    if (!WaitUntil(2, ^BOOL { return fixture.app.terminated; })) [fixture.app forceTerminate];
    WaitUntil(2, ^BOOL { return fixture.app.terminated; });
}

static void WriteRegistration(NSString *path, NSArray *arguments) {
    Require([@{@"Label":@"io.github.garlicvread.jaso-nfc.menu", @"ProgramArguments":arguments}
             writeToFile:path atomically:YES], @"Could not write test registration");
}

static NSUInteger Failures;
static NSUInteger Cases;
static void Test(NSString *name, void (^body)(void)) {
    Cases++;
    @try {
        body();
        fprintf(stdout, "PASS %s\n", name.UTF8String);
    } @catch (NSException *failure) {
        Failures++;
        fprintf(stderr, "FAIL %s: %s\n", name.UTF8String, failure.reason.UTF8String);
    }
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        NSArray<NSString *> *arguments = NSProcessInfo.processInfo.arguments;
        // Even an incorrectly restored argv must not recursively launch tests.
        if ([arguments.firstObject.lastPathComponent isEqual:@"MenuStartupFixture"]) return RunFixture(arguments);
        if (arguments.count > 1 && [arguments[1] isEqual:@"--fixture"]) return RunFixture(arguments);

        NSString *directory = [[NSTemporaryDirectory() stringByAppendingPathComponent:
                               [@"jaso-menu-startup-" stringByAppendingString:NSUUID.UUID.UUIDString]] stringByResolvingSymlinksInPath];
        NSError *setupError = nil;
        if (![NSFileManager.defaultManager createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:@{NSFilePosixPermissions:@0700} error:&setupError]) {
            fprintf(stderr, "Fixture directory: %s\n", setupError.localizedDescription.UTF8String);
            return 1;
        }
        NSString *registration = [directory stringByAppendingPathComponent:@"menu.plist"];
        NSString *defaultConfig = [directory stringByAppendingPathComponent:@"default/config.json"];
        NSString *customConfig = [directory stringByAppendingPathComponent:@"custom state/설정.json"];
        __block OwnedFixture *original = nil;
        __block OwnedFixture *unrelated = nil;
        __block OwnedFixture *restored = nil;
        @try {
            Test(@"explicit config wins over malformed registration", ^{
                [@"not a plist" writeToFile:registration atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                NSString *error = nil;
                NSString *config = JasoMenuConfig(@[@"menu", @"--config", customConfig], registration, defaultConfig, &error);
                Require([config isEqual:customConfig] && !error, @"Explicit config must take precedence");
            });
            Test(@"registered custom absolute config is retained", ^{
                WriteRegistration(registration, @[@"/fixture/menu", @"--config", customConfig]);
                NSString *error = nil;
                NSString *config = JasoMenuConfig(@[@"menu"], registration, defaultConfig, &error);
                Require([config isEqual:customConfig] && !error, @"Finder startup must use the registered custom path");
            });
            Test(@"missing registration uses the default", ^{
                NSString *error = nil;
                NSString *config = JasoMenuConfig(@[@"menu"], [directory stringByAppendingPathComponent:@"absent.plist"], defaultConfig, &error);
                Require([config isEqual:defaultConfig] && !error, @"Only missing registration should select the default");
            });
            Test(@"malformed or relative registration is rejected", ^{
                NSArray *invalidValues = @[@"not a plist", @[],
                    @{@"ProgramArguments":@[@"/fixture/menu", @"--config", customConfig]},
                    @{@"Label":@"io.github.garlicvread.jaso-nfc.menu", @"ProgramArguments":@[@"/fixture/menu", @"--config"]},
                    @{@"Label":@"io.github.garlicvread.jaso-nfc.menu", @"ProgramArguments":@[@"/fixture/menu", @"--config", @"relative.json"]}];
                for (id value in invalidValues) {
                    if ([value isKindOfClass:NSString.class]) [value writeToFile:registration atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                    else [value writeToFile:registration atomically:YES];
                    NSString *error = nil;
                    Require(JasoMenuConfig(@[@"menu"], registration, defaultConfig, &error) == nil && error.length > 0,
                            [NSString stringWithFormat:@"Invalid registration silently selected a config: %@", value]);
                }
            });

            NSString *workerRegistration = [directory stringByAppendingPathComponent:@"io.github.garlicvread.jaso-nfc.plist"];
            Test(@"single worker registration preserves Finder custom configuration", ^{
                [NSFileManager.defaultManager removeItemAtPath:registration error:NULL];
                for (NSArray *workerArguments in @[@[@"/fixture/worker", @"run", @"--config", customConfig],
                                                    @[@"/fixture/worker", @"watch", @"--config", customConfig],
                                                    @[@"/fixture/python", @"/fixture/run.py", @"watch", @"--config", customConfig]]) {
                    [@{@"Label":@"io.github.garlicvread.jaso-nfc", @"ProgramArguments":workerArguments} writeToFile:workerRegistration atomically:YES];
                    NSString *error = nil;
                    Require([JasoMenuConfig(@[@"menu"], registration, defaultConfig, &error) isEqual:customConfig] && !error,
                            @"Finder startup ignored the worker's saved custom configuration");
                }
            });
            Test(@"conflicting and malformed worker registrations never fall back", ^{
                WriteRegistration(registration, @[@"/fixture/menu", @"--config", defaultConfig]);
                NSString *error = nil;
                Require(!JasoMenuConfig(@[@"menu"], registration, defaultConfig, &error) && error.length,
                        @"Different menu and worker configs were silently accepted");
                [NSFileManager.defaultManager removeItemAtPath:registration error:NULL];
                [@"broken plist" writeToFile:workerRegistration atomically:YES encoding:NSUTF8StringEncoding error:NULL];
                error = nil;
                Require(!JasoMenuConfig(@[@"menu"], registration, defaultConfig, &error) && error.length,
                        @"Malformed worker registration selected default state");
                [NSFileManager.defaultManager removeItemAtPath:workerRegistration error:NULL];
            });

            NSString *executable = CreateFixtureApp(directory);
            original = LaunchFixture(executable, [directory stringByAppendingPathComponent:@"original.json"], @"custom argument with spaces 자소");
            unrelated = LaunchFixture(executable, [directory stringByAppendingPathComponent:@"unrelated.json"], @"independent fixture");
            __block NSDictionary *snapshot = nil;
            Test(@"snapshot accepts the exact allowed executable", ^{
                NSString *error = nil;
                snapshot = JasoMenuSnapshot(original.app, @[executable], &error);
                Require(snapshot != nil && !error, [NSString stringWithFormat:@"Could not snapshot owned fixture: %@", error]);
                Require([snapshot[@"pid"] intValue] == original.app.processIdentifier, @"Snapshot identifies the wrong PID");
                Require(FixtureAlive(original), @"Snapshot unexpectedly stopped the process");
            });
            Test(@"wrong executable is rejected without stopping", ^{
                NSString *error = nil;
                NSDictionary *denied = JasoMenuSnapshot(original.app, @[[executable stringByAppendingString:@".wrong"]], &error);
                Require(denied == nil && error.length > 0, @"An unapproved executable was accepted");
                Require(FixtureAlive(original), @"Rejected snapshot stopped the fixture");
            });
            Test(@"tampered PID cannot stop another owned process", ^{
                Require(snapshot != nil, @"Valid snapshot is required for the PID test");
                NSMutableDictionary *tampered = [snapshot mutableCopy];
                tampered[@"pid"] = @(unrelated.app.processIdentifier);
                NSString *error = nil;
                Require(!JasoMenuStop(tampered, &error) && error.length > 0, @"A snapshot with another process's PID was accepted");
                Require(FixtureAlive(unrelated) && FixtureAlive(original), @"An identity mismatch stopped an owned process");
            });
            Test(@"stop and restore preserve the original arguments", ^{
                Require(snapshot != nil, @"Valid snapshot is required for handoff");
                NSString *error = nil;
                Require(JasoMenuStop(snapshot, &error), [NSString stringWithFormat:@"Stop failed: %@", error]);
                Require(WaitUntil(3, ^BOOL { return original.app.terminated; }), @"Original process remained running");
                Require(![NSFileManager.defaultManager fileExistsAtPath:[original.recordPath stringByAppendingString:@".delegate-termination"]], @"Installer handoff must not invoke app-wide worker shutdown");
                Require(FixtureAlive(unrelated), @"Stopping the selected fixture stopped another process");
                [NSFileManager.defaultManager removeItemAtPath:original.recordPath error:NULL];
                error = nil;
                Require(JasoMenuRestore(snapshot, &error), [NSString stringWithFormat:@"Restore failed: %@", error]);
                Require(WaitUntil(8, ^BOOL {
                    NSDictionary *record = ReadRecord(original.recordPath);
                    pid_t pid = [record[@"pid"] intValue];
                    if (pid <= 0 || pid == original.app.processIdentifier) return NO;
                    NSRunningApplication *app = [NSRunningApplication runningApplicationWithProcessIdentifier:pid];
                    if (!app || !app.finishedLaunching) return NO;
                    restored = [OwnedFixture new];
                    restored.app = app;
                    restored.executable = executable;
                    restored.record = record;
                    return YES;
                }), @"Restored fixture did not report readiness");
                Require([restored.record[@"arguments"] isEqual:original.record[@"arguments"]], @"Restore changed the executable or original argument vector");
                Require(FixtureAlive(unrelated), @"Restore disturbed the independent fixture");
            });
        } @catch (NSException *failure) {
            Failures++;
            fprintf(stderr, "Fixture setup: %s\n", failure.reason.UTF8String);
        } @finally {
            CleanFixture(restored);
            CleanFixture(original);
            CleanFixture(unrelated);
            [NSFileManager.defaultManager removeItemAtPath:directory error:NULL];
        }
        fprintf(stdout, "%lu menu startup cases, %lu failures\n", (unsigned long)Cases, (unsigned long)Failures);
        return Failures == 0 ? 0 : 1;
    }
}
