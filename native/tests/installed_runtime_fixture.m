// A native GUI substitute: no AppKit, launchd registration, or user state access.
#import <Foundation/Foundation.h>
#include <sys/file.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>

static volatile sig_atomic_t stopped;
static void stop(int sig) { stopped = 1; }
int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc != 3 || strcmp(argv[1], "--config")) return 2;
        NSDictionary *config = [NSJSONSerialization JSONObjectWithData:
            [NSData dataWithContentsOfFile:@(argv[2])] options:0 error:NULL];
        NSString *state = config[@"state_dir"];
        if (!state) return 3;
        int singleton = open([[state stringByAppendingPathComponent:@"fixture-menu.lock"] fileSystemRepresentation], O_CREAT | O_RDWR, 0600);
        if (singleton < 0 || flock(singleton, LOCK_EX | LOCK_NB)) return 0;
        int runtime = open([[state stringByAppendingPathComponent:@".lock"] fileSystemRepresentation], O_RDWR);
        BOOL locked = runtime >= 0 && flock(runtime, LOCK_EX | LOCK_NB) != 0;
        if (runtime >= 0) close(runtime);
        NSDictionary *record = @{@"pid":@(getpid()), @"pgid":@(getpgrp()),
            @"sid":@(getsid(0)), @"worker_locked":@(locked),
            @"arguments":NSProcessInfo.processInfo.arguments};
        NSData *bytes = [NSJSONSerialization dataWithJSONObject:record options:0 error:NULL];
        int log = open([[state stringByAppendingPathComponent:@"fixture-menu.jsonl"] fileSystemRepresentation], O_CREAT | O_WRONLY | O_APPEND, 0600);
        write(log, bytes.bytes, bytes.length); write(log, "\n", 1); close(log);
        if ([[NSProcessInfo.processInfo.environment objectForKey:@"JASO_FIXTURE_MENU_EXIT"] isEqual:@"1"]) return 7;
        signal(SIGTERM, stop);
        while (!stopped) usleep(10000);
        close(singleton);
    }
    return 0;
}
