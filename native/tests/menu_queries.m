// Exercise real query launch/tracking/cancellation with an owned C fixture.
// The production entry point is never called, so no service/app is installed.
#define main JasoProductionMenuMain
#import "../macos/Menu.m"
#undef main
#import <objc/runtime.h>
#include <errno.h>
#include <sys/wait.h>

static NSUserDefaults *TestDefaults;
static id IsolatedDefaults(id object,SEL selector) { (void)object;(void)selector;return TestDefaults; }
static void Require(BOOL condition,NSString *message) {
    if(!condition)@throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSArray<NSTask *> *TrackedTasks(JasoMenu *menu) {
    @synchronized(menu){return menu.queryTasks.allObjects;}
}
static BOOL Await(BOOL (^condition)(void),NSTimeInterval seconds) {
    NSDate *deadline=[NSDate dateWithTimeIntervalSinceNow:seconds];
    while(!condition()&&deadline.timeIntervalSinceNow>0)
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.01]];
    return condition();
}

static NSUInteger ActivityLaunches(NSString *path) {
    NSString *text=[NSString stringWithContentsOfFile:path encoding:NSUTF8StringEncoding error:nil];
    return text.length?[[text componentsSeparatedByString:@"\n"] count]-1:0;
}
static void TestSharedActivity(JasoMenu *menu,NSString *directory) {
    NSString *savedWorker=menu.workerPath;menu.workerPath=NSProcessInfo.processInfo.arguments.firstObject;
    menu.configPath=[directory stringByAppendingPathComponent:@"activity.calls"];
    menu.statusWindow=JasoWorkspaceWindowController.new;
    JasoWorkspaceWindowController *detached=JasoWorkspaceWindowController.new;
    [menu.statusWindow setValue:detached forKey:@"activityWindow"];
    __block NSUInteger replies=0;__block NSDictionary *first=nil,*second=nil,*third=nil;
    [menu requestWorkspace:@"activity" parameters:@{} reply:^(NSDictionary *result,NSString *error){Require(NSThread.isMainThread,@"Shared Activity responses must arrive on main");Require(!error&&[result[@"available"] boolValue],@"First Activity reader must receive a successful fixture response");first=result;replies++;}];
    [menu requestWorkspace:@"activity" parameters:@{} reply:^(NSDictionary *result,NSString *error){Require(!error&&[result[@"available"] boolValue],@"Second Activity reader must receive a successful fixture response");second=result;replies++;}];
    // Keep main unpumped while the real worker and reader queue complete.
    // A cache hit must not expose an unpublished result ahead of older fanout.
    NSDate *readyDeadline=[NSDate dateWithTimeIntervalSinceNow:2];
    while(ActivityLaunches(menu.configPath)==0&&readyDeadline.timeIntervalSinceNow>0)usleep(1000);
    Require(ActivityLaunches(menu.configPath)==1,@"The publication-order fixture must start its owned worker");
    dispatch_sync(menu.activityQueue,^{});
    __block BOOL early=NO;
    [menu requestWorkspace:@"activity" parameters:@{} reply:^(NSDictionary *result,NSString *error){Require(result&&!error,@"A poll joining completed but unpublished work must receive its result");third=result;early=YES;replies++;}];
    Require(!early,@"A cache hit must not expose a completed result before its ordered main publication");
    Require(Await(^BOOL{return replies==3;},4),@"Concurrent Activity consumers must all complete");
    Require(ActivityLaunches(menu.configPath)==1,@"Concurrent Activity windows must share one real query process");
    Require(first==second&&second==third,@"Concurrent Activity consumers must receive the same parsed snapshot");
    Require([menu.statusWindow valueForKey:@"activity"]==first&&[detached valueForKey:@"activity"]==first,@"Every fresh query result must publish to both windows before their next timer tick");
    [menu requestWorkspace:@"activity" parameters:@{} reply:^(NSDictionary *result,NSString *error){Require(result==first&&!error,@"A nearby window poll must reuse the shared parsed result");replies++;}];
    Require(Await(^BOOL{return replies==4;},2)&&ActivityLaunches(menu.configPath)==1,@"The short result cache must avoid a duplicate nearby poll");
    [menu requestWorkspace:@"activity" parameters:@{@"fresh":@YES} reply:^(NSDictionary *result,NSString *error){Require(result&&!error,@"Explicit refresh must complete");Require(![result[@"activity"][@"query_pid"] isEqual:first[@"activity"][@"query_pid"]],@"Explicit refresh must deliver newly fetched active state");Require([detached valueForKey:@"activity"]==result,@"Detached Activity must receive the fresh result without another poll");replies++;}];
    Require(Await(^BOOL{return replies==5;},4)&&ActivityLaunches(menu.configPath)==2,@"Explicit refresh must bypass a recent cached Activity result");
    [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:1.05]];
    [menu requestWorkspace:@"activity" parameters:@{} reply:^(NSDictionary *result,NSString *error){Require(result&&!error,@"Expired Activity cache must query again");replies++;}];
    Require(Await(^BOOL{return replies==6;},4)&&ActivityLaunches(menu.configPath)==3,@"The shared cache must expire within one normal polling interval");
    menu.configPath=[directory stringByAppendingPathComponent:@"failure.calls"];
    for(NSUInteger i=0;i<2;i++)[menu requestWorkspace:@"activity" parameters:@{} reply:^(NSDictionary *result,NSString *error){Require(!result&&error.length>0,@"A failed shared query must deliver its error to every waiting consumer");replies++;}];
    Require(Await(^BOOL{return replies==8;},4)&&ActivityLaunches(menu.configPath)==1,@"The failing query must also be shared and complete both consumers");
    Require([menu.statusWindow valueForKey:@"activity"]==nil&&[detached valueForKey:@"activity"]==nil,@"Query failure must immediately invalidate both windows' live state");
    [menu requestWorkspace:@"activity" parameters:@{} reply:^(NSDictionary *result,NSString *error){Require(!result&&error.length>0,@"A following read must retry a failed query");replies++;}];
    Require(Await(^BOOL{return replies==9;},4)&&ActivityLaunches(menu.configPath)==2,@"Errors must never be retained as a successful cached response");
    Require(menu.activityReplies.count==0,@"Success and error publication must release every pending consumer");
    [menu.statusWindow setValue:nil forKey:@"activityWindow"];[menu.statusWindow close];[detached close];menu.statusWindow=nil;
    menu.workerPath=savedWorker;
    puts("PASS shared Activity query/result publication, bounded cache expiry, explicit freshness and error recovery");
}

int main(int argc,const char **argv) {
    if(argc==4&&strcmp(argv[1],"activity")==0&&strcmp(argv[2],"--config")==0){
        int log=open(argv[3],O_WRONLY|O_CREAT|O_APPEND,0600);if(log<0)return 2;
        dprintf(log,"%ld\n",(long)getpid());close(log);usleep(100000);
        if(strstr(argv[3],"/failure.calls")){fputs("Owned query fixture failure\n",stderr);return 1;}
        printf("{\"available\":true,\"activity\":{\"session_id\":\"owned-query-fixture\",\"query_pid\":%ld}}\n",(long)getpid());return 0;
    }
    @autoreleasepool {
        if(argc!=2){fprintf(stderr,"usage: menu-queries-test /absolute/path/query-worker\n");return 2;}
        NSString *worker=@(argv[1]);
        NSString *suite=[@"jaso-menu-query-" stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults=[[NSUserDefaults alloc] initWithSuiteName:suite];
        Method method=class_getClassMethod(NSUserDefaults.class,@selector(standardUserDefaults));
        IMP original=method_setImplementation(method,(IMP)IsolatedDefaults);
        NSString *directory=[NSTemporaryDirectory() stringByAppendingPathComponent:suite];
        JasoMenu *menu=JasoMenu.new;
        dispatch_group_t commands=dispatch_group_create();
        int status=0;
        @try {
            Require(worker.isAbsolutePath&&[NSFileManager.defaultManager isExecutableFileAtPath:worker],@"The query fixture executable must be supplied explicitly");
            Require([NSFileManager.defaultManager createDirectoryAtPath:directory withIntermediateDirectories:NO attributes:@{NSFilePosixPermissions:@0700} error:nil],@"The fixture needs an owned temporary directory");
            [NSApplication sharedApplication];[NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
            JasoSetLanguagePreference(@"en");
            menu.workerPath=worker;
            TestSharedActivity(menu,directory);
            NSArray *readyPaths=@[[directory stringByAppendingPathComponent:@"history.ready"],
                [directory stringByAppendingPathComponent:@"preview.ready"]];
            NSArray *arguments=@[@[@"history",@"--ready",readyPaths[0]],@[@"setup",@"preview",@"--ready",readyPaths[1]]];
            NSMutableArray *failures=NSMutableArray.new;
            for(NSArray *args in arguments)dispatch_group_async(commands,dispatch_get_global_queue(QOS_CLASS_UTILITY,0),^{
                @autoreleasepool {
                    @try {NSString *error=nil;NSDictionary *result=[menu execute:args error:&error];
                        if(result){@synchronized(failures){[failures addObject:@"A killed query must not return a successful JSON response"];}}
                    } @catch(NSException *failure){@synchronized(failures){[failures addObject:failure.reason?:@"Query execution raised an exception"];}}
                }
            });
            Require(Await(^BOOL{
                return [NSFileManager.defaultManager fileExistsAtPath:readyPaths[0]]&&[NSFileManager.defaultManager fileExistsAtPath:readyPaths[1]];
            },4),@"Both workers must reach their TERM-ignoring readiness point");
            NSArray<NSTask *> *tasks=TrackedTasks(menu);
            Require(tasks.count==2,@"The ordinary history query and preview must both be tracked");
            Require(dispatch_group_wait(menu.queryGroup,DISPATCH_TIME_NOW)!=0,@"The query group must remain entered while workers are blocked");
            NSMutableSet *pids=NSMutableSet.new;
            for(NSString *ready in readyPaths){NSString *pidText=[NSString stringWithContentsOfFile:ready encoding:NSUTF8StringEncoding error:nil];[pids addObject:@(pidText.intValue)];}
            for(NSTask *task in tasks)Require(task.running&&[pids containsObject:@(task.processIdentifier)],@"Tracked tasks must be the exact owned live workers");

            NSTimeInterval started=NSProcessInfo.processInfo.systemUptime;
            menu.quitting=YES;
            [menu cancelReadTasks];
            Require(Await(^BOOL{return dispatch_group_wait(menu.queryGroup,DISPATCH_TIME_NOW)==0;},6),@"Cancel must release the query group after its bounded kill fallback");
            Require(dispatch_group_wait(commands,dispatch_time(DISPATCH_TIME_NOW,NSEC_PER_SEC))==0,@"All execute calls must return after their workers are reaped");
            NSTimeInterval elapsed=NSProcessInfo.processInfo.systemUptime-started;
            Require(elapsed>=1.8&&elapsed<6,@"TERM-ignoring workers must reach the two-second KILL fallback without indefinite wait");
            Require(TrackedTasks(menu).count==0,@"Cancelled workers must leave the tracked set");
            Require(menu.previewTask==nil,@"Cancellation must release the preview task reference");
            Require(failures.count==0,[failures componentsJoinedByString:@"; "]);
            for(NSTask *task in tasks){
                Require(!task.running&&task.terminationReason==NSTaskTerminationReasonUncaughtSignal&&task.terminationStatus==SIGKILL,@"The fallback must terminate each TERM-ignoring worker with KILL");
                pid_t pid=task.processIdentifier;
                errno=0;Require(kill(pid,0)==-1&&errno==ESRCH,@"A cancelled worker must no longer exist");
                int childStatus=0;errno=0;
                Require(waitpid(pid,&childStatus,WNOHANG)==-1&&errno==ECHILD,@"NSTask must reap its worker before the query group is released");
            }
            NSString *lateReady=[directory stringByAppendingPathComponent:@"late.ready"];
            NSString *error=nil;
            NSDictionary *late=[menu execute:@[@"history",@"--ready",lateReady] error:&error];
            Require(!late&&error.length>0,@"Once quitting starts, a new query must be rejected");
            Require(![NSFileManager.defaultManager fileExistsAtPath:lateReady]&&TrackedTasks(menu).count==0,@"Rejected queries must never launch another worker");
            Require(dispatch_group_wait(menu.queryGroup,DISPATCH_TIME_NOW)==0,@"A rejected query must not strand a group entry");
            printf("PASS menu query tracking, TERM-to-KILL cancellation, group completion, reaping and quit launch guard (%.2fs)\n",elapsed);
        } @catch(NSException *failure){fprintf(stderr,"FAIL %s\n",failure.reason.UTF8String);status=1;}
        @finally {
            menu.quitting=YES;
            for(NSTask *task in TrackedTasks(menu))if(task.running)kill(task.processIdentifier,SIGKILL);
            dispatch_group_wait(commands,dispatch_time(DISPATCH_TIME_NOW,3*NSEC_PER_SEC));
            [NSFileManager.defaultManager removeItemAtPath:directory error:nil];
            method_setImplementation(method,original);
            [TestDefaults removePersistentDomainForName:suite];
        }
        return status;
    }
}
