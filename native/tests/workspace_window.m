#import <AppKit/AppKit.h>
#import "../macos/Localization.h"
#import "../macos/WorkspaceWindow.h"
#import "../macos/ContentZoom.h"
#import <objc/runtime.h>
static NSUserDefaults *TestDefaults;
static id IsolatedDefaults(id self, SEL selector) { return TestDefaults; }

@protocol WorkspaceFixture
@property (readonly) NSWindow *window;
@property (readonly, copy) NSString *selectedSection;
- (void)selectSection:(NSString *)section;
- (void)updateSnapshot:(NSDictionary *)snapshot error:(NSString *)error updatedAt:(NSDate *)date;
- (void)updateActivity:(NSDictionary *)response error:(NSString *)error;
- (void)updateHistory:(NSDictionary *)response error:(NSString *)error;
- (void)reloadLocalization;
- (void)close;
@end
static void Require(BOOL condition, NSString *message) {
    if (!condition) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSView *Find(NSView *view, NSString *identifier) {
    if ([view.identifier isEqual:identifier]) return view;
    for (NSView *child in view.subviews) { NSView *found = Find(child, identifier); if (found) return found; }
    return nil;
}
static void CheckVisibleBounds(NSView *view, NSView *root) {
    if([view isKindOfClass:NSButton.class]||[view isKindOfClass:NSTextField.class]){
        NSRect frame=[view convertRect:view.bounds toView:root];
        Require(NSMinX(frame)>=-1&&NSMaxX(frame)<=root.bounds.size.width+1,@"A control extends beyond the window at large text size");
        if([view.identifier isEqual:@"activity-window"]||[view.identifier isEqual:@"activity-refresh"])Require([(NSButton *)view cell].cellSize.width<=view.bounds.size.width+1,@"Activity actions are clipped at large text size");
    }
    for(NSView *child in view.subviews)CheckVisibleBounds(child,root);
}
static void Capture(NSWindow *window, NSString *path) {
    [window orderFront:nil];[NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.1]];[window.contentView layoutSubtreeIfNeeded];
    NSBitmapImageRep *bitmap=[window.contentView bitmapImageRepForCachingDisplayInRect:window.contentView.bounds];
    [window.contentView cacheDisplayInRect:window.contentView.bounds toBitmapImageRep:bitmap];
    Require([[bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}] writeToFile:path atomically:YES],@"Could not capture workspace fixture");
}
int main(int argc,const char **argv) {
    @autoreleasepool {
        [NSApplication sharedApplication];[NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];[NSApp finishLaunching];
        NSString *suite=[@"jaso-workspace-test-" stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults=[[NSUserDefaults alloc] initWithSuiteName:suite];
        Method defaultsMethod=class_getClassMethod(NSUserDefaults.class,@selector(standardUserDefaults));
        IMP originalDefaults=method_setImplementation(defaultsMethod,(IMP)IsolatedDefaults);
        @try {
            Class implementation = NSClassFromString(@"JasoWorkspaceWindowController");
            Require(implementation != Nil, @"The application needs one navigable workspace window");
            id<WorkspaceFixture> app = [implementation new];
            NSDictionary *snapshot=@{@"running":@YES,@"paused":@NO,@"apply":@YES,@"baseline_complete":@YES,@"pending_recovery":@NO,@"pending_jobs":@0,@"deferred_jobs":@0,@"deferred_renames":@0,@"active_roots":@[@"/tmp/fixture"],@"roots":@[@"/tmp/fixture"]};
            [app updateSnapshot:snapshot error:nil updatedAt:NSDate.date];
            for(NSDictionary *change in @[@{@"pending_jobs":@300},@{@"pending_jobs":@0},@{@"directory_retry_count":@20}]){
                NSMutableDictionary *state=snapshot.mutableCopy;[state addEntriesFromDictionary:change];
                Require([JasoWorkspaceStatus(state,nil,NO)[@"title"] isEqual:@"Automatic cleanup is on"],@"Routine enabled states must share the main heading");
            }
            NSMutableDictionary *blocked=snapshot.mutableCopy;blocked[@"activity"]=@{@"state":@"blocked",@"issue":@{@"code":@"low_storage"}};
            Require([JasoWorkspaceStatus(blocked,nil,NO)[@"title"] isEqual:@"Storage needs attention"],@"Global storage blockage must remain visible");
            NSMutableDictionary *malformed=snapshot.mutableCopy;malformed[@"activity"]=NSNull.null;Require(JasoWorkspaceStatus(malformed,nil,NO)!=nil,@"Older or missing activity must remain supported");
            for (NSString *name in @[@"status",@"activity",@"folders",@"history",@"settings"]) {
                NSButton *button=(id)Find(app.window.contentView, [@"nav-" stringByAppendingString:name]);
                Require(button && button.enabled, [@"Missing usable destination: " stringByAppendingString:name]);
                [button performClick:nil];
                Require([app.selectedSection isEqual:name], @"Navigation must select its destination");
                [app updateSnapshot:snapshot error:nil updatedAt:NSDate.date];
                Require([app.selectedSection isEqual:name], @"Background refresh must preserve the selected page");
            }
            [app selectSection:@"activity"];
            [app updateActivity:@{@"available":@YES,@"activity":@{@"session_id":@"fixture",@"state":@"processing",@"phase":@"directory_read",@"scope_path":@"/tmp/fixture",@"item_path":NSNull.null,@"phase_started_at":@100,@"last_progress_at":@100,@"counters":@{@"processed":@7,@"renamed":@2},@"scope":@{@"processed":@7,@"observed":@7,@"total":NSNull.null},@"events":@[]}} error:nil];
            Require([[(NSTextField *)Find(app.window.contentView,@"activity-location") stringValue] containsString:@"/tmp/fixture"], @"Current location must come from the live activity snapshot");
            NSProgressIndicator *progress=(id)Find(app.window.contentView,@"activity-progress");
            Require(progress && progress.indeterminate, @"An unknown folder total must use indeterminate progress");
            [app updateActivity:@{@"available":@NO,@"activity":NSNull.null} error:nil];
            Require(![[(NSTextField *)Find(app.window.contentView,@"activity-location") stringValue] containsString:@"/tmp/fixture"], @"Unavailable live state must not keep showing the old location as current");
            [app selectSection:@"history"];
            [app updateHistory:@{@"items":@[],@"total":@0,@"today_count":@0,@"limit":@50,@"offset":@0} error:nil];
            Require(Find(app.window.contentView,@"history-empty")!=nil,@"Empty history needs its own explanation");
            for (NSString *language in @[@"ko",@"en"]) {
                JasoSetLanguagePreference(language); [app reloadLocalization];
                Require([app.selectedSection isEqual:@"history"],@"Language changes must preserve navigation");
                Require([[(NSButton *)Find(app.window.contentView,@"nav-history") title] isEqual:[language isEqual:@"ko"]?@"변경 기록":@"History"],@"Menu labels must follow the selected language");
            }
            JasoSetContentZoom(2);
            [app.window setContentSize:NSMakeSize(800,560)];
            for(NSString *section in @[@"status",@"activity",@"history"]){[app selectSection:section];[app.window.contentView layoutSubtreeIfNeeded];CheckVisibleBounds(app.window.contentView,app.window.contentView);}
            JasoSetContentZoom(1);[app.window setContentSize:NSMakeSize(980,720)];
            if(argc>1){
                NSString *folder=@(argv[1]);[[NSFileManager defaultManager] createDirectoryAtPath:folder withIntermediateDirectories:YES attributes:nil error:nil];
                for(NSString *language in @[@"ko",@"en"]){JasoSetLanguagePreference(language);[app reloadLocalization];
                    for(NSString *section in @[@"status",@"activity",@"history"]){[app selectSection:section];Capture(app.window,[folder stringByAppendingPathComponent:[NSString stringWithFormat:@"%@-%@.png",section,language]]);}
                }
            }
            [app close]; Require(![(JasoWorkspaceWindowController *)app refreshingAutomatically],@"Closing a workspace must stop periodic queries"); printf("PASS native workspace navigation, live-state truthfulness and localization\n");
        } @catch(NSException *error) {
            fprintf(stderr,"FAIL %s\n",error.reason.UTF8String); method_setImplementation(defaultsMethod,originalDefaults); [TestDefaults removePersistentDomainForName:suite]; return 1;
        }
        method_setImplementation(defaultsMethod,originalDefaults); [TestDefaults removePersistentDomainForName:suite];
    }
    return 0;
}
