// Owned native GUI used by test_app_trampoline.py. Never starts the real menu.
#import <AppKit/AppKit.h>
#import <libproc.h>

@interface TrampolineFixture : NSObject <NSApplicationDelegate>
@end
@implementation TrampolineFixture
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    NSBundle *bundle = NSBundle.mainBundle;
    NSRunningApplication *application = NSRunningApplication.currentApplication;
    char path[PROC_PIDPATHINFO_MAXSIZE] = {0};
    proc_pidpath(getpid(), path, sizeof(path));
    NSDictionary *record = @{@"pid":@(getpid()), @"arguments":NSProcessInfo.processInfo.arguments,
                             @"bundle":bundle.bundlePath ?: @"", @"identifier":bundle.bundleIdentifier ?: @"",
                             @"executable":bundle.executablePath ?: @"", @"actual_executable":@(path),
                             @"application_identifier":application.bundleIdentifier ?: @"",
                             @"application_executable":application.executableURL.path ?: @""};
    NSData *data = [NSJSONSerialization dataWithJSONObject:record options:0 error:NULL];
    fwrite(data.bytes, 1, data.length, stdout);
    fputc('\n', stdout);
    fflush(stdout);
    [NSApp terminate:nil];
}
@end

int main(int argc, const char **argv) {
    @autoreleasepool {
        NSApplication *application = NSApplication.sharedApplication;
        [application setActivationPolicy:NSApplicationActivationPolicyAccessory];
        __attribute__((objc_precise_lifetime)) TrampolineFixture *delegate = [TrampolineFixture new];
        application.delegate = delegate;
        // Bound fixture lifetime even if AppKit cannot complete startup.
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
            _exit(3);
        });
        [application run];
    }
    return 0;
}
