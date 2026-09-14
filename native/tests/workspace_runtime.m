// Deterministic production-controller regressions. No worker or visible window.
#import <AppKit/AppKit.h>
#import <objc/runtime.h>
#import "../macos/Localization.h"
#import "../macos/WorkspaceWindow.h"
#import "../macos/ContentZoom.h"
#import "../macos/NamePresentation.h"
#import "../macos/StatusPresentation.h"
#include <sys/resource.h>

@interface JasoWorkspaceWindowController (RuntimeFixture)
- (void)openDetail:(NSString *)title;
- (void)closeDetail:(id)sender;
- (void)showHistoryItem:(NSButton *)sender;
- (void)showActivityItem:(id)sender;
- (void)refreshActivityFields;
- (void)showIssue:(NSButton *)sender;
- (void)reviewRestore:(NSButton *)sender;
- (void)confirmRestore:(NSButton *)sender;
- (void)showRestoreResult:(NSDictionary *)result error:(NSString *)error request:(NSDictionary *)request;
- (void)render;
@end

// Only replace presentation of a sheet, avoiding key-window/activation races.
// The production history actions, request callbacks and result rendering run intact.
@interface HiddenWorkspace : JasoWorkspaceWindowController
@end
@implementation HiddenWorkspace
- (void)openDetail:(NSString *)title {
    [self closeDetail:nil];
    NSPanel *panel=[[NSPanel alloc] initWithContentRect:NSMakeRect(0,0,580,450)
        styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    panel.title=title;
    NSStackView *body=NSStackView.new;
    body.orientation=NSUserInterfaceLayoutOrientationVertical;
    [panel.contentView addSubview:body];
    [self setValue:panel forKey:@"detailPanel"];
    [self setValue:body forKey:@"detailBody"];
}
- (void)closeDetail:(id)sender {
    (void)sender;
    [self setValue:nil forKey:@"detailPanel"];
    [self setValue:nil forKey:@"detailBody"];
}
@end

@interface CapturedRequest : NSObject
@property NSDictionary *parameters;
@property(copy) JasoWorkspaceReply reply;
@end
@implementation CapturedRequest
@end

static NSUserDefaults *TestDefaults;
static NSPasteboard *NameCopyPasteboard;
static id IsolatedNamePasteboard(id object,SEL selector) { (void)object;(void)selector;return NameCopyPasteboard; }
static id IsolatedDefaults(id object,SEL selector) { (void)object;(void)selector;return TestDefaults; }
static void Require(BOOL condition,NSString *message) {
    if(!condition)@throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSView *Find(NSView *view,NSString *identifier) {
    if([view.identifier isEqual:identifier])return view;
    for(NSView *child in view.subviews){NSView *found=Find(child,identifier);if(found)return found;}
    return nil;
}
static void DrainReplies(void) {
    __block BOOL drained=NO;
    dispatch_async(dispatch_get_main_queue(),^{drained=YES;});
    NSDate *deadline=[NSDate dateWithTimeIntervalSinceNow:2];
    while(!drained&&deadline.timeIntervalSinceNow>0)
        [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.005]];
    Require(drained,@"The fixture must deliver queued main-thread callbacks");
}
static NSDictionary *Record(NSString *identifier) {
    return @{@"id":identifier,@"revision":[@"revision-" stringByAppendingString:identifier],
        @"old_name":[identifier stringByAppendingString:@"-original"],@"new_name":[identifier stringByAppendingString:@"-changed"],
        @"new_path":[@"/disposable-fixture/" stringByAppendingString:identifier],
        @"timestamp":@"2026-09-12T12:00:00Z",@"result":@"renamed",@"restored":@NO};
}
static void ShowRecord(HiddenWorkspace *workspace,NSString *identifier) {
    NSButton *sender=[NSClassFromString(@"WorkspaceButton") new];
    Require(sender!=nil,@"The production history button class must be available");
    [sender setValue:Record(identifier) forKey:@"record"];
    [workspace showHistoryItem:sender];
}
static NSDictionary *Result(NSDictionary *parameters,NSString *state) {
    return @{@"version":@1,@"request_id":parameters[@"request_id"],
        @"operation_id":parameters[@"operation_id"],@"state":state,@"message":@"fixture result"};
}
static BOOL ExactName(NSString *a,NSString *b) {
    return [[a dataUsingEncoding:NSUTF8StringEncoding] isEqual:[b dataUsingEncoding:NSUTF8StringEncoding]];
}
static void TestNameComparison(void) {
    HiddenWorkspace *workspace=HiddenWorkspace.new;
    NSString *before=@"\u1112\u1161\u11AB\u1100\u1173\u11AF (1)_1.xlsx";
    NSString *after=@"한글 (1)_1.xlsx";
    NSDictionary *record=@{@"id":@"name-comparison",@"revision":@"exact-revision",@"old_name":before,@"new_name":after,
        @"old_path":[@"/disposable-fixture/" stringByAppendingString:before],@"new_path":[@"/disposable-fixture/" stringByAppendingString:after],
        @"timestamp":@"2026-09-12T12:00:00Z",@"result":@"renamed",@"restored":@NO};
    __block NSString *revealed=nil;
    __block NSDictionary *preview=nil;
    workspace.pathHandler=^(NSString *path){revealed=path;};
    workspace.requestHandler=^(NSString *name,NSDictionary *parameters,JasoWorkspaceReply reply){
        if([name isEqual:@"history-preview"]){preview=parameters;reply(@{@"allowed":@NO,@"message":@"Fixture preview"},nil);}
        else if([name isEqual:@"history"])reply(@{@"items":@[record],@"total":@1,@"today_count":@1},nil);
        else Require(NO,[@"Unexpected name comparison request: " stringByAppendingString:name]);
    };
    NameCopyPasteboard=[NSPasteboard pasteboardWithName:[@"jaso-name-copy-" stringByAppendingString:NSUUID.UUID.UUIDString]];
    Method method=class_getClassMethod(NSPasteboard.class,@selector(generalPasteboard));
    IMP original=method_setImplementation(method,(IMP)IsolatedNamePasteboard);
    @try {
        for(NSString *language in @[@"en",@"ko"]){
            JasoSetLanguagePreference(language);
            for(NSNumber *scale in @[@1,@2]){
                JasoSetContentZoom(scale.doubleValue);
                [workspace updateSnapshot:@{@"running":@YES} error:nil updatedAt:NSDate.date];
                [workspace updateHistory:@{@"items":@[record],@"total":@1} error:nil];[workspace selectSection:@"history"];
                NSButton *oldRow=(id)Find(workspace.window.contentView,JasoHistoryRowIdentifier(record,@"before"));
                NSButton *newRow=(id)Find(workspace.window.contentView,JasoHistoryRowIdentifier(record,@"after"));
                NSString *oldTitle=[NSString stringWithFormat:@"%@: ㅎㅏㄴㄱㅡㄹ (1)_1.xlsx",[language isEqual:@"ko"]?@"변경 전":@"Before"];
                NSString *newTitle=[NSString stringWithFormat:@"%@: %@",[language isEqual:@"ko"]?@"변경 후":@"After",after];
                Require(ExactName(oldRow.title,oldTitle)&&ExactName(newRow.title,newTitle),@"History rows must visibly distinguish separated and combined names with localized labels");
                Require([oldRow valueForKey:@"record"]==record&&[newRow valueForKey:@"record"]==record,@"Both row actions must retain the exact original record");
                [workspace showHistoryItem:oldRow];
                NSView *body=[workspace valueForKey:@"detailBody"];
                NSTextField *oldName=(id)Find(body,@"history-before-name");
                NSTextField *newName=(id)Find(body,@"history-after-name");
                Require(ExactName(oldName.stringValue,@"ㅎㅏㄴㄱㅡㄹ (1)_1.xlsx")&&ExactName(newName.stringValue,after),@"Details must use the same independent name comparison");
                Require(!oldName.selectable&&!newName.selectable,@"Display-only characters must not be copied as operational names");
                Require(oldName.font.pointSize==14*scale.doubleValue&&newName.font.pointSize==14*scale.doubleValue,@"Both comparison sides must honor text size");
                [(NSButton *)Find(body,@"history-copy-before") performClick:nil];
                Require(ExactName([NameCopyPasteboard stringForType:NSPasteboardTypeString],before),@"Copy original name must preserve every original character");
                [(NSButton *)Find(body,@"history-copy-after") performClick:nil];
                Require(ExactName([NameCopyPasteboard stringForType:NSPasteboardTypeString],after),@"Copy changed name must preserve the actual changed name");
                [(NSButton *)Find(body,@"history-reveal") performClick:nil];
                Require(ExactName(revealed,record[@"new_path"]),@"Reveal must use the raw path, never comparison text");
                [workspace reviewRestore:(id)Find(body,@"history-restore")];DrainReplies();
                Require([preview isEqual:@{@"id":record[@"id"],@"revision":record[@"revision"]}],@"Restore preview must retain the operation identity and revision");
                [workspace closeDetail:nil];
            }
        }
    } @finally {
        method_setImplementation(method,original);[NameCopyPasteboard releaseGlobally];NameCopyPasteboard=nil;
        workspace.requestHandler=nil;[workspace close];JasoSetContentZoom(1);JasoSetLanguagePreference(@"en");
    }
    puts("PASS History comparison, exact-name copy and raw actions in EN/KO at 100/200 percent");
}
static void TestHistoryRowFocus(void) {
    HiddenWorkspace *workspace=HiddenWorkspace.new;
    NSMutableDictionary *first=[Record(@"first-operation") mutableCopy];
    NSMutableDictionary *second=[Record(@"second-operation") mutableCopy];
    for(NSMutableDictionary *record in @[first,second]){
        record[@"old_name"]=[@"한글.txt" decomposedStringWithCanonicalMapping];record[@"new_name"]=@"한글.txt";
    }
    NSDictionary *page=@{@"items":@[first,second],@"total":@2};
    workspace.requestHandler=^(NSString *name,NSDictionary *parameters,JasoWorkspaceReply reply){
        (void)parameters;Require([name isEqual:@"history"],@"Focus fixture only reads History");reply(page,nil);
    };
    @try {
        for(NSString *language in @[@"en",@"ko"]){
            JasoSetLanguagePreference(language);JasoSetContentZoom(1);
            [workspace updateHistory:page error:nil];[workspace selectSection:@"history"];DrainReplies();
            for(NSString *side in @[@"before",@"after"]){
                NSButton *target=(id)Find(workspace.window.contentView,JasoHistoryRowIdentifier(second,side));
                Require(target&&[workspace.window makeFirstResponder:target],@"The second history record must accept keyboard focus");
                for(NSNumber *zoom in @[@2,@1]){
                    JasoSetContentZoom(zoom.doubleValue);[workspace.window.contentView layoutSubtreeIfNeeded];
                    NSButton *focused=(id)workspace.window.firstResponder;
                    NSButton *expected=(id)Find(workspace.window.contentView,JasoHistoryRowIdentifier(second,side));
                    Require(focused==expected&&[focused valueForKey:@"record"]==second,@"Zoom must retain the second record and its selected side, never the first matching name");
                    [focused performClick:nil];
                    Require([workspace valueForKey:@"selectedRecord"]==second,@"Activating the focused row after zoom must open the original second record");
                    [workspace closeDetail:nil];
                }
            }
        }
        Require(!workspace.window.visible,@"History focus fixture must leave its window hidden");
    } @finally {workspace.requestHandler=nil;[workspace close];JasoSetContentZoom(1);JasoSetLanguagePreference(@"en");}
    puts("PASS zoom preserves the second History record and action target in EN/KO");
}
static void TestRestoreAttribution(void) {
    HiddenWorkspace *workspace=HiddenWorkspace.new;
    NSMutableArray<CapturedRequest *> *requests=NSMutableArray.new;
    workspace.requestHandler=^(NSString *name,NSDictionary *parameters,JasoWorkspaceReply reply){
        if([name isEqual:@"history-restore"]){CapturedRequest *request=CapturedRequest.new;request.parameters=parameters;request.reply=reply;[requests addObject:request];}
        else if([name isEqual:@"history"])reply(@{@"items":@[],@"total":@0,@"today_count":@0,@"limit":@50,@"offset":@0},nil);
        else Require(NO,[@"Unexpected fixture request: " stringByAppendingString:name]);
    };
    @try {
        ShowRecord(workspace,@"A");
        [workspace confirmRestore:NSButton.new];
        Require(requests.count==1,@"Confirm must submit one selected restore");
        CapturedRequest *first=requests[0];
        Require([first.parameters[@"operation_id"] isEqual:@"A"],@"Restore must capture A's identity at confirmation");

        ShowRecord(workspace,@"B");
        NSView *bodyB=[workspace valueForKey:@"detailBody"];
        NSButton *restoreB=(id)Find(bodyB,@"history-restore");
        Require(restoreB&&!restoreB.enabled,@"A pending restore must disable B's restore action");
        [workspace confirmRestore:NSButton.new];
        Require(requests.count==1,@"Direct or repeated confirmation must not submit a second active restore");
        Require([[workspace valueForKey:@"restoreRequest"] isEqual:first.parameters],@"A second confirmation must preserve A's tracked request");

        first.reply(Result(first.parameters,@"queued"),nil);
        DrainReplies();
        Require(Find(bodyB,@"restore-result")==nil,@"A delayed queued result must not appear in B's details");
        [workspace showRestoreResult:Result(first.parameters,@"restored") error:nil request:first.parameters];
        DrainReplies();
        Require(Find(bodyB,@"restore-result")==nil,@"A's completed restore must not claim that B was restored");
        Require([workspace valueForKey:@"restoreRequest"]==nil,@"A terminal result must release A's active request");

        ShowRecord(workspace,@"B");
        [workspace confirmRestore:NSButton.new];
        Require(requests.count==2,@"B must become restorable after A finishes");
        CapturedRequest *second=requests[1];
        Require(![second.parameters[@"request_id"] isEqual:first.parameters[@"request_id"]],@"Every restore must receive a fresh request identifier");
        bodyB=[workspace valueForKey:@"detailBody"];
        // A result poll may have been started before another callback completed A.
        [workspace showRestoreResult:Result(first.parameters,@"restored") error:nil request:first.parameters];
        Require([[workspace valueForKey:@"restoreRequest"] isEqual:second.parameters],@"A late terminal result must not clear B's newer request");
        Require(Find(bodyB,@"restore-result")==nil,@"A late result must not overwrite B's new details");

        NSMutableDictionary *mismatched=[Result(second.parameters,@"restored") mutableCopy];
        mismatched[@"operation_id"]=@"A";
        [workspace showRestoreResult:mismatched error:nil request:second.parameters];
        Require([[workspace valueForKey:@"restoreRequest"] isEqual:second.parameters],@"A mismatched response operation must not complete the active restore");
        Require(Find(bodyB,@"restore-result")==nil,@"An unrelated response must not render a success message");

        second.reply(Result(second.parameters,@"queued"),nil);
        DrainReplies();
        Require(Find(bodyB,@"restore-result")!=nil,@"The matching queued result must be visible in B's original sheet");
        [workspace showRestoreResult:Result(second.parameters,@"restored") error:nil request:second.parameters];
        DrainReplies();
        Require([[(NSTextField *)Find(bodyB,@"restore-result") stringValue] containsString:@"original name was restored"],@"The matching terminal result must report B's completion");
        Require([workspace valueForKey:@"restoreRequest"]==nil,@"B's terminal result must clear its active request");
        Require(!workspace.window.visible,@"Runtime fixtures must never show or activate the workspace");
    } @finally { workspace.requestHandler=nil;[workspace close]; }
    printf("PASS workspace restore attribution, active request guard and stale result rejection\n");
}
static NSDictionary *Activity(NSString *session,NSArray<NSNumber *> *sequences,NSUInteger dropped) {
    NSMutableArray *events=NSMutableArray.new;
    for(NSNumber *sequence in sequences)[events addObject:@{@"sequence":sequence,@"kind":@"renamed",@"phase":@"renaming",
        @"at":@100,@"path":[NSString stringWithFormat:@"/disposable-fixture/item-%@",sequence]}];
    return @{@"version":@1,@"available":@YES,@"activity":@{@"session_id":session,@"state":@"processing",
        @"phase":@"enumerating",@"scope_path":@"/disposable-fixture",@"item_path":NSNull.null,
        @"phase_elapsed_seconds":@1,@"last_progress_at":@100,@"counters":@{@"processed":@10,@"renamed":@3},
        @"scope":@{@"processed":@10,@"observed":@10,@"total":NSNull.null},@"events":[events copy],@"dropped_events":@(dropped)}};
}
static void SelectRow(JasoWorkspaceWindowController *workspace,NSInteger row) {
    NSTableView *table=[workspace valueForKey:@"activityTable"];
    [table selectRowIndexes:[NSIndexSet indexSetWithIndex:row] byExtendingSelection:NO];
    [workspace tableViewSelectionDidChange:[NSNotification notificationWithName:NSTableViewSelectionDidChangeNotification object:table]];
}
static void TestActivitySelection(void) {
    JasoWorkspaceWindowController *workspace=JasoWorkspaceWindowController.new;
    @try {
        [workspace selectSection:@"activity"];
        [workspace updateActivity:Activity(@"session-A",@[@1,@2,@3],0) error:nil];
        NSTableView *table=[workspace valueForKey:@"activityTable"];
        SelectRow(workspace,1);
        Require([[workspace valueForKey:@"selectedActivitySequence"] isEqual:@2],@"Selection must capture the actual event identity");
        [workspace updateActivity:Activity(@"session-A",@[@2,@3,@4],1) error:nil];
        Require(table.selectedRow==0,@"Evicting an older row must keep the same selected event, now at row zero");
        Require([[workspace valueForKey:@"selectedActivitySequence"] isEqual:@2],@"Programmatic reload notifications must not replace the selected event identity");

        [workspace selectSection:@"history"];
        [workspace selectSection:@"activity"];
        table=[workspace valueForKey:@"activityTable"];
        Require(table.selectedRow==0,@"Leaving and returning to Activity must preserve a retained selected event");
        [workspace updateActivity:Activity(@"session-A",@[@3,@4,@5],7) error:nil];
        Require(table.selectedRow==-1,@"When the selected event is evicted, no other event may inherit selection");
        Require([workspace valueForKey:@"selectedActivitySequence"]==nil,@"Eviction must clear the saved event identity");
        NSTextField *retention=(id)Find(workspace.window.contentView,@"activity-retention");
        Require(retention&&[retention.stringValue containsString:@"7 older activity entries"],@"A bounded feed must display the dropped entry count");
        Require([retention.stringValue containsString:@"History"],@"The dropped caption must explain where completed changes remain available");

        SelectRow(workspace,1);
        [workspace updateActivity:Activity(@"session-B",@[@3,@4,@5],0) error:nil];
        Require(table.selectedRow==-1,@"Restarting a session must clear selection even when sequence numbers repeat");
        Require([workspace valueForKey:@"selectedActivitySequence"]==nil,@"A new session must not retain the old selection identity");
        Require(![retention.stringValue containsString:@"7 older"],@"A new session must not inherit the previous dropped count");
        Require(!workspace.window.visible,@"Activity fixtures must not show or activate a window");
    } @finally { [workspace close]; }
    printf("PASS activity selection across eviction, navigation and restart; dropped count caption\n");
}
static void TestZoomReadingState(void) {
    JasoSetContentZoom(1);
    JasoWorkspaceWindowController *workspace=JasoWorkspaceWindowController.new;
    @try {
        [workspace.window setContentSize:NSMakeSize(660,560)];
        [workspace selectSection:@"activity"];
        NSMutableArray *sequences=NSMutableArray.new;for(NSUInteger i=0;i<80;i++)[sequences addObject:@(i+1)];
        [workspace updateActivity:Activity(@"zoom-session",sequences,0) error:nil];
        SelectRow(workspace,12);
        [workspace.window.contentView layoutSubtreeIfNeeded];
        NSTableView *table=[workspace valueForKey:@"activityTable"];
        NSClipView *feed=table.enclosingScrollView.contentView;
        [feed scrollToPoint:NSMakePoint(0,NSMinY([table rectOfRow:10])+7)];
        NSScrollView *outer=[workspace valueForKey:@"scroll"];
        [outer.contentView scrollToPoint:NSMakePoint(0,40)];
        Require(feed.bounds.origin.y>0&&outer.contentView.bounds.origin.y>0,@"Zoom regression must begin with both activity scroll positions away from zero");
        for(NSNumber *zoom in @[@2,@1]) {
            NSPoint outerOrigin=outer.contentView.bounds.origin;
            CGFloat rowPosition=feed.bounds.origin.y/(table.rowHeight+table.intercellSpacing.height);
            JasoSetContentZoom(zoom.doubleValue);[workspace.window.contentView layoutSubtreeIfNeeded];
            outer=[workspace valueForKey:@"scroll"];table=[workspace valueForKey:@"activityTable"];feed=table.enclosingScrollView.contentView;
            Require(fabs(outer.contentView.bounds.origin.y-outerOrigin.y)<1,@"Zoom must preserve the current outer activity scroll position");
            CGFloat restored=feed.bounds.origin.y/(table.rowHeight+table.intercellSpacing.height);
            Require(fabs(restored-rowPosition)<.05,@"Zoom with Follow off must preserve the visible activity row and its fractional offset");
            Require(table.selectedRow==12&&![[workspace valueForKey:@"followActivity"] boolValue],@"Zoom must retain selected activity identity and Follow off");
        }
        NSMutableArray *records=NSMutableArray.new;for(NSUInteger i=0;i<15;i++)[records addObject:Record([NSString stringWithFormat:@"zoom-%lu",(unsigned long)i])];
        [workspace updateHistory:@{@"items":records,@"total":@15} error:nil];[workspace selectSection:@"history"];
        [workspace.window.contentView layoutSubtreeIfNeeded];
        NSSearchField *search=(id)Find(workspace.window.contentView,@"history-search");
        Require([workspace.window makeFirstResponder:search],@"History search must accept focus in the runtime fixture");
        NSTextView *editor=(id)search.currentEditor;
        Require(editor!=nil,@"History search focus must use the real field editor");
        editor.string=@"partially typed 검색";editor.selectedRange=NSMakeRange(3,6);
        __block NSUInteger queries=0;workspace.requestHandler=^(NSString *name,NSDictionary *parameters,JasoWorkspaceReply reply){queries++;};
        for(NSNumber *zoom in @[@2,@1]) {
            JasoSetContentZoom(zoom.doubleValue);[workspace.window.contentView layoutSubtreeIfNeeded];
            search=(id)Find(workspace.window.contentView,@"history-search");editor=(id)search.currentEditor;
            Require(editor&&workspace.window.firstResponder==editor,@"Zoom must restore History search focus into the current search control");
            Require([editor.string isEqual:@"partially typed 검색"]&&NSEqualRanges(editor.selectedRange,NSMakeRange(3,6)),[NSString stringWithFormat:@"Zoom must retain pending History search text and selection, got '%@' %@",editor.string,NSStringFromRange(editor.selectedRange)]);
            Require(queries==0,@"Zoom must not submit an unfinished search while rebuilding its field");
        }
        Require(!workspace.window.visible,@"Zoom state tests must leave the runtime window hidden");
    } @finally { workspace.requestHandler=nil;[workspace close];JasoSetContentZoom(1); }
    printf("PASS zoom preserves outer scroll, activity reading position and History editing focus\n");
}

static NSInteger SelectedActivityClick(NSTableView *table,SEL selector) { (void)selector;return table.selectedRow; }
static NSString *AllVisibleText(NSView *view) {
    NSMutableArray *parts=NSMutableArray.new;
    if([view isKindOfClass:NSTextField.class])[parts addObject:[(NSTextField *)view stringValue]];
    if([view isKindOfClass:NSButton.class])[parts addObject:[(NSButton *)view title]];
    for(NSView *child in view.subviews)[parts addObject:AllVisibleText(child)];
    return [parts componentsJoinedByString:@"\n"];
}
static NSDictionary *ActivityEvents(NSArray *events) {
    NSMutableDictionary *live=[Activity(@"causes-session",@[],0)[@"activity"] mutableCopy];live[@"events"]=events;
    return @{@"available":@YES,@"activity":live};
}
static void TestActivityCauses(void) {
    Method click=class_getInstanceMethod(NSTableView.class,@selector(clickedRow));
    IMP original=method_setImplementation(click,(IMP)SelectedActivityClick);
    HiddenWorkspace *workspace=HiddenWorkspace.new;
    NSString *path=@"/disposable-fixture/CloudStorage/permission-file";
    NSDictionary *permission=@{@"sequence":@1,@"kind":@"error",@"phase":@"metadata",@"at":@1000,@"path":path,@"errno":@13,@"reason":@"Permission denied (os error 13)"};
    NSDictionary *cloud=@{@"sequence":@2,@"kind":@"deferred",@"phase":@"metadata",@"at":@1000,@"path":@"/disposable-fixture/cloud-file",@"reason":@"dataless-file"};
    NSMutableDictionary *resolved=[permission mutableCopy];resolved[@"sequence"]=@3;resolved[@"resolved_at"]=@2000;resolved[@"resolution"]=@"checked";
    NSMutableDictionary *truncated=[permission mutableCopy];truncated[@"sequence"]=@4;truncated[@"path_truncated"]=@YES;
    NSDictionary *legacy=@{@"sequence":@5,@"kind":@"error",@"phase":@"metadata",@"at":@1000,@"path":path};
    NSArray *events=@[permission,cloud,resolved,truncated,legacy];
    @try {
        for(NSString *language in @[@"en",@"ko"]) {
            JasoSetLanguagePreference(language);BOOL ko=[language isEqual:@"ko"];
            for(NSNumber *scale in @[@1,@2]) {
                JasoSetContentZoom(scale.doubleValue);[workspace selectSection:@"activity"];[workspace reloadLocalization];
                [workspace setValue:@"all" forKey:@"activityFilter"];
                [workspace updateSnapshot:@{@"running":@YES,@"paused":@NO,@"apply":@YES,@"pending_recovery":@NO,@"baseline_complete":@YES,@"pending_jobs":@1,
                    @"directory_retry_count":@1,@"directory_retry_items":@[@{@"path":path,@"reason":@"Operation timed out (os error 60)",@"attempts":@1}]} error:nil updatedAt:NSDate.date];
                [workspace updateActivity:ActivityEvents(events) error:nil];
                [workspace.window.contentView layoutSubtreeIfNeeded];NSRect frame=workspace.window.frame;
                NSTableView *table=[workspace valueForKey:@"activityTable"];
                NSTextField *cell=(id)[workspace tableView:table viewForTableColumn:table.tableColumns.firstObject row:1];
                Require([cell.stringValue containsString:ko?@"클라우드에 보관 중":@"Stored in the cloud"],@"Activity row must render its saved cloud-only cause");
                Require(fabs(cell.font.pointSize-11*scale.doubleValue)<.01&&fabs(table.rowHeight-48*scale.doubleValue)<.01,@"The Activity cause change preserves row and text sizing at 100/200 percent");
                NSString *page=AllVisibleText([workspace valueForKey:@"body"]);
                Require([page containsString:ko?@"최근 활동":@"Recent activity"]&&[page containsString:ko?@"현재 대기 항목":@"Currently waiting"],@"Historical feed and current waiting items have separate honest headings");
                SelectRow(workspace,0);[workspace showActivityItem:nil];
                NSString *detail=AllVisibleText([workspace valueForKey:@"detailBody"]);
                Require([detail containsString:ko?@"접근 권한":@"Access was denied"],@"The detail preserves saved permission failure even when the current issue is a timeout");
                Require([detail containsString:ko?@"현재 상태":@"Current status"]&&[detail containsString:ko?@"응답 시간":@"timed out"],@"A newer exact-path current reason has its own current-status paragraph");
                Require(![detail containsString:ko?@"이번 실행에서 확인한 활동":@"This entry describes an observation"],@"An available saved cause must not be replaced by generic observation text");
                Require(Find([workspace valueForKey:@"detailBody"],@"activity-reveal")!=nil,@"A complete absolute historical path offers Finder inspection");
                [workspace closeDetail:nil];SelectRow(workspace,2);[workspace showActivityItem:nil];
                NSPanel *panel=[workspace valueForKey:@"detailPanel"];
                Require([panel.title containsString:ko?@"이후 검사 완료":@"Checked successfully later"],@"Resolved history opens with the actual later outcome");
                detail=AllVisibleText([workspace valueForKey:@"detailBody"]);
                Require([detail containsString:ko?@"접근 권한":@"Access was denied"]&&[detail containsString:ko?@"해결 시각":@"Resolved at"],@"Resolved details retain cause and actual resolution time");
                [workspace closeDetail:nil];SelectRow(workspace,3);[workspace showActivityItem:nil];
                detail=AllVisibleText([workspace valueForKey:@"detailBody"]);
                Require(Find([workspace valueForKey:@"detailBody"],@"activity-reveal")==nil&&![detail containsString:ko?@"현재 상태":@"Current status"],@"A truncated display path enables neither Finder action nor exact-path current matching");
                [workspace closeDetail:nil];SelectRow(workspace,4);[workspace showActivityItem:nil];
                detail=AllVisibleText([workspace valueForKey:@"detailBody"]);
                Require([detail containsString:ko?@"원인이 기록되지 않았습니다":@"cause was not recorded"],@"Current retry data must never fill a legacy event's unrecorded historical cause");
                [workspace closeDetail:nil];
                [workspace updateSnapshot:@{@"running":@YES,@"paused":@NO,@"apply":@YES,@"directory_retry_items":@[],@"rename_retry_items":@[]} error:nil updatedAt:NSDate.date];
                SelectRow(workspace,0);[workspace updateActivity:ActivityEvents(events) error:nil];
                Require(table.selectedRow==0&&[[workspace valueForKey:@"selectedActivitySequence"] isEqual:@1],@"Historical cause refresh preserves event selection identity");
                [workspace setValue:@"wait" forKey:@"activityFilter"];[workspace refreshActivityFields];
                NSArray *filtered=[workspace valueForKey:@"activityRows"];
                Require(filtered.count==4,@"The problem/deferral filter excludes only explicitly resolved history, never infers resolution from an empty retry sample");
                for(NSDictionary *row in filtered)Require(![row[@"sequence"] isEqual:@3],@"Resolved failures cannot remain in the problem filter");
                [workspace setValue:@"all" forKey:@"activityFilter"];[workspace refreshActivityFields];
                Require([[workspace valueForKey:@"activityRows"] count]==5,@"All Activity keeps resolved history inspectable");
                NSPopUpButton *filter=(id)Find(workspace.window.contentView,@"activity-filter");
                Require([filter.itemTitles containsObject:ko?@"오류·대기":@"Issues"],@"The feed filter labels historical issue observations without calling them all currently waiting");
                SelectRow(workspace,0);NSMutableDictionary *resolvedSelected=[resolved mutableCopy];resolvedSelected[@"sequence"]=@1;
                [workspace updateActivity:ActivityEvents(@[resolvedSelected,cloud,resolved,truncated,legacy]) error:nil];
                Require(table.selectedRow==0&&[[workspace valueForKey:@"selectedActivitySequence"] isEqual:@1],@"A later resolution updates the same selected historical event without changing its identity");
                cell=(id)[workspace tableView:table viewForTableColumn:table.tableColumns.firstObject row:0];
                Require([cell.stringValue containsString:ko?@"이후 검사 완료":@"Checked successfully later"],@"The selected row adopts the actual resolution when an observation is resolved");
                Require(NSEqualRects(frame,workspace.window.frame)&&!workspace.window.visible,@"Rendering cause and resolution preserves window dimensions without activating a fixture window");
            }
        }
    } @finally { [workspace closeDetail:nil];[workspace close];method_setImplementation(click,original);JasoSetLanguagePreference(@"en");JasoSetContentZoom(1); }
    puts("PASS saved Activity causes, resolution, separate current state, safe paths and filter/selection in EN/KO at 100/200 percent");
}

static void TestActivityPhases(void) {
    JasoWorkspaceWindowController *workspace=JasoWorkspaceWindowController.new;
    // Values emitted by activity.rs, service.rs and normalizer.rs, plus unknown
    // values that may arrive from a newer worker. Check visible controller output.
    NSArray *cases=@[
        @[@"waiting_for_events",@"idle",@"Ready for new files",@"새 파일을 확인할 준비가 됐습니다"],
        @[@"normalizing",@"processing",@"Checking a file name",@"파일 이름 확인 중"],
        @[@"starting",@"starting",@"Starting cleanup",@"정리를 시작하고 있습니다"],
        @[@"discovering_sources",@"waiting_metadata",@"Checking selected locations",@"선택한 위치 확인 중"],
        @[@"opening_directory",@"waiting_metadata",@"Opening folder metadata",@"폴더 정보 확인 중"],
        @[@"reading_metadata",@"waiting_metadata",@"Checking file metadata",@"파일 정보 확인 중"],
        @[@"enumerating",@"waiting_metadata",@"Reading folder metadata",@"폴더 목록 확인 중"],
        @[@"restoring",@"processing",@"Restoring the original name",@"원래 이름으로 되돌리는 중"],
        @[@"checking_storage",@"processing",@"Checking available disk space",@"디스크 여유 공간 확인 중"],
        @[@"updating_history",@"processing",@"Updating change history",@"변경 기록 갱신 중"],
        @[@"observation",@"processing",@"Checking a file",@"파일 확인 중"],
        @[@"low_storage",@"blocked",@"Cleanup is waiting for disk space",@"디스크 공간 확보를 기다리고 있습니다"],
        @[@"stopping",@"stopping",@"Stopping cleanup",@"정리를 중지하는 중"],
        @[@"stopped",@"stopped",@"Cleanup is stopped",@"정리가 중지되었습니다"],
        @[@"future_phase",@"processing",@"Activity details unavailable",@"활동 내용을 확인할 수 없습니다"],
        @[@"",@"processing",@"Activity details unavailable",@"활동 내용을 확인할 수 없습니다"],
        @[@"future_phase",@"waiting_metadata",@"Activity details unavailable",@"활동 내용을 확인할 수 없습니다",@12],
        @[@"discovering_sources",@"waiting_metadata",@"Checking selected locations",@"선택한 위치 확인 중",@12],
        @[@"reading_metadata",@"waiting_metadata",@"Waiting for this folder's metadata",@"폴더 정보 응답을 기다리고 있습니다",@12],
        @[@"future_phase",@"idle",@"Ready for new files",@"새 파일을 확인할 준비가 됐습니다"],
        @[@"normalizing",@"paused",@"Paused",@"일시 정지"],
        @[@"normalizing",@"stopped",@"Cleanup is stopped",@"정리가 중지되었습니다"]
    ];
    NSMutableArray *failures=NSMutableArray.new;
    @try {
        for(NSString *language in @[@"en",@"ko"]){
            JasoSetLanguagePreference(language);
            [workspace selectSection:@"activity"];
            [workspace reloadLocalization];
            NSUInteger labelIndex=[language isEqual:@"ko"]?3:2;
            for(NSArray *item in cases){
                NSMutableDictionary *live=[Activity(@"phase-fixture",@[],0)[@"activity"] mutableCopy];
                live[@"phase"]=item[0];live[@"state"]=item[1];
                if(item.count>4)live[@"phase_elapsed_seconds"]=item[4];
                live[@"item_path"]=NSNull.null;live[@"scope_path"]=NSNull.null;live[@"scope"]=NSNull.null;
                [workspace updateActivity:@{@"available":@YES,@"activity":live} error:nil];
                NSString *actual=[[workspace valueForKey:@"activityPhase"] stringValue];
                if(![actual isEqual:item[labelIndex]])[failures addObject:[NSString stringWithFormat:@"%@ %@/%@: expected '%@', got '%@'",language,item[1],item[0],item[labelIndex],actual]];
                if([@[@"idle",@"paused",@"stopped"] containsObject:item[1]])
                    Require([[workspace valueForKey:@"activityProgress"] isHidden],@"Inactive states must not animate folder processing");
            }
            // Feed entries must use their actual phase; their kind is 'started'.
            for(NSArray *eventCase in @[
                @[@"started",@"normalizing",@"Checking a file name",@"파일 이름 확인 중"],
                @[@"restored",@"processing",@"Original name restored",@"원래 이름으로 되돌림 완료"],
                @[@"deferred",@"metadata",@"Check deferred · cause not recorded",@"확인 보류 · 원인 기록 없음"],
                @[@"error",@"metadata",@"Check incomplete · cause not recorded",@"검사 미완료 · 원인 기록 없음"],
                @[@"started",@"future_phase",@"Activity details unavailable",@"활동 내용을 확인할 수 없습니다"]
            ]){
                NSMutableDictionary *live=[Activity(@"phase-fixture",@[],0)[@"activity"] mutableCopy];
                live[@"events"]=@[@{@"sequence":@1,@"kind":eventCase[0],@"phase":eventCase[1],@"at":@100,@"path":@"/disposable-fixture/item"}];
                [workspace updateActivity:@{@"available":@YES,@"activity":live} error:nil];
                NSTableView *table=[workspace valueForKey:@"activityTable"];
                NSTextField *cell=(id)[workspace tableView:table viewForTableColumn:table.tableColumns.firstObject row:0];
                NSString *expected=eventCase[labelIndex];
                if(![cell.stringValue containsString:expected])[failures addObject:[NSString stringWithFormat:@"%@ event %@/%@: expected '%@', got '%@'",language,eventCase[0],eventCase[1],expected,cell.stringValue]];
            }
            [workspace updateActivity:@{@"available":@NO,@"activity":NSNull.null} error:nil];
            NSString *unavailable=[language isEqual:@"ko"]?@"활동 내용을 확인할 수 없습니다":@"Activity details unavailable";
            if(![[[workspace valueForKey:@"activityPhase"] stringValue] isEqual:unavailable])
                [failures addObject:[language stringByAppendingString:@" unavailable activity must not claim a check is in progress"]];
        }
        Require(failures.count==0,[failures componentsJoinedByString:@"\n"]);
        Require(!workspace.window.visible,@"Phase fixtures must not show or activate a window");
    } @finally { [workspace close];JasoSetLanguagePreference(@"en"); }
    printf("PASS backend activity phase vocabulary, inactive headings and honest unknown labels in EN/KO\n");
}
// Keep the real detail panel and constraints; suppress only OS presentation.
static void SuppressBeginSheet(id window,SEL selector,NSWindow *sheet,void (^completion)(NSModalResponse)) {
    (void)window;(void)selector;(void)sheet;(void)completion;
}
static void SuppressEndSheet(id window,SEL selector,NSWindow *sheet) { (void)window;(void)selector;(void)sheet; }
static NSTextField *FindText(NSView *view,NSString *text) {
    if([view isKindOfClass:NSTextField.class]&&[[(NSTextField *)view stringValue] isEqual:text])return (id)view;
    for(NSView *child in view.subviews){NSTextField *found=FindText(child,text);if(found)return found;}
    return nil;
}
static void CheckFont(NSControl *control,CGFloat expected,NSString *context,NSMutableArray *failures) {
    if(!control||fabs(control.font.pointSize-expected)>.01)
        [failures addObject:[NSString stringWithFormat:@"%@: expected %.1f-point font, got %.1f",context,expected,control.font.pointSize]];
}
static void CheckDetailWidth(JasoWorkspaceWindowController *workspace,NSString *context) {
    NSPanel *panel=[workspace valueForKey:@"detailPanel"];
    [panel.contentView layoutSubtreeIfNeeded];
    [workspace.window.contentView layoutSubtreeIfNeeded];
    [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.02]];
    [panel.contentView layoutSubtreeIfNeeded];
    Require(panel.contentView.bounds.size.width<=581,[context stringByAppendingString:@": detail content must not enlarge its 580-point panel"]);
    Require(workspace.window.contentView.bounds.size.width<=861,[context stringByAppendingString:@": opening a detail must not enlarge the 860-point workspace"]);
    for(NSView *view in [(NSStackView *)[workspace valueForKey:@"detailBody"] arrangedSubviews]){
        NSRect frame=[view convertRect:view.bounds toView:panel.contentView];
        Require(NSMinX(frame)>=-1&&NSMaxX(frame)<=581,[context stringByAppendingString:@": detail controls must fit horizontally rather than clip outside the panel"]);
    }
}
static void TestDetailZoom(void) {
    Method begin=class_getInstanceMethod(NSWindow.class,@selector(beginSheet:completionHandler:));
    Method end=class_getInstanceMethod(NSWindow.class,@selector(endSheet:));
    IMP originalBegin=method_setImplementation(begin,(IMP)SuppressBeginSheet);
    IMP originalEnd=method_setImplementation(end,(IMP)SuppressEndSheet);
    Method click=class_getInstanceMethod(NSTableView.class,@selector(clickedRow));
    IMP originalClick=method_setImplementation(click,(IMP)SelectedActivityClick);
    JasoWorkspaceWindowController *workspace=JasoWorkspaceWindowController.new;
    NSString *longName=[@"한글 보고서-" stringByPaddingToLength:200 withString:@"가족 공동 연구 👩🏽‍💻 " startingAtIndex:0];
    NSString *longPath=[@"/disposable-fixture/A long folder name/" stringByAppendingString:longName];
    NSDictionary *record=@{@"id":@"detail-item",@"revision":@"detail-revision",@"old_path":longPath,@"new_path":longPath,
        @"old_name":[longName decomposedStringWithCanonicalMapping],@"new_name":longName,@"timestamp":@"2026-09-12T12:00:00Z",@"result":@"renamed",@"kind":@"file",@"restored":@NO};
    NSString *previewMessage=@"The original name is available and the recorded item matches.";
    NSMutableArray *failures=NSMutableArray.new;
    workspace.requestHandler=^(NSString *name,NSDictionary *parameters,JasoWorkspaceReply reply){
        if([name isEqual:@"history-preview"])reply(@{@"can_restore":@YES,@"reason":@"available",@"allowed":@YES,@"message":previewMessage,@"item":record},nil);
        else if([name isEqual:@"history-restore"])reply(Result(parameters,@"queued"),nil);
        else if([name isEqual:@"history"])reply(@{@"items":@[],@"total":@0,@"today_count":@0,@"limit":@50,@"offset":@0},nil);
        else if([name isEqual:@"activity"])reply(ActivityEvents(@[]),nil);
        else Require(NO,[@"Unexpected detail fixture request: " stringByAppendingString:name]);
    };
    @try {
        for(NSString *language in @[@"en",@"ko"]){
            JasoSetLanguagePreference(language);JasoSetContentZoom(1);
            [workspace updateSnapshot:@{@"running":@YES} error:nil updatedAt:NSDate.date];
            NSButton *historySender=[NSClassFromString(@"WorkspaceButton") new];[historySender setValue:record forKey:@"record"];
            [workspace showHistoryItem:historySender];
            CGFloat buttonSize=[(NSButton *)Find([workspace valueForKey:@"detailBody"],@"history-restore") font].pointSize;
            Require(buttonSize>0,@"The unzoomed history action supplies a real font baseline");
            [workspace closeDetail:nil];JasoSetContentZoom(2);
            [workspace.window setContentSize:NSMakeSize(860,660)];
            [workspace showHistoryItem:historySender];
            NSView *body=[workspace valueForKey:@"detailBody"];
            NSString *heading=[language isEqual:@"ko"]?@"이름 변경 상세":@"Rename details";
            CheckFont(FindText(body,heading),42,[language stringByAppendingString:@" history heading at 200%"],failures);
            CheckFont(FindText(body,longName),28,[language stringByAppendingString:@" history name at 200%"],failures);
            CheckFont((id)Find(body,@"history-before-name"),28,[language stringByAppendingString:@" separated history name at 200%"],failures);
            CheckFont(FindText(body,longPath),28,[language stringByAppendingString:@" history path at 200%"],failures);
            NSButton *restore=(id)Find(body,@"history-restore");
            CheckFont(restore,buttonSize*2,[language stringByAppendingString:@" restore action at 200%"],failures);
            CheckDetailWidth(workspace,@"history details");

            [workspace reviewRestore:restore];DrainReplies();
            NSString *previewText=[language isEqual:@"ko"]?@"원래 이름으로 되돌릴 수 있습니다.":previewMessage;
            CheckFont(FindText(body,previewText),26,[language stringByAppendingString:@" asynchronous preview at 200%"],failures);
            NSButton *confirm=(id)Find(body,@"history-confirm-restore");
            CheckFont(confirm,buttonSize*2,[language stringByAppendingString:@" asynchronous confirm action at 200%"],failures);
            CheckFont(FindText(body,longName),28,[language stringByAppendingString:@" existing name must not scale twice"],failures);
            CheckDetailWidth(workspace,@"restore preview");

            [workspace updateSnapshot:@{@"running":@NO} error:nil updatedAt:NSDate.date];
            [workspace confirmRestore:confirm];DrainReplies();
            NSDictionary *request=[workspace valueForKey:@"restoreRequest"];
            CheckFont((id)Find(body,@"restore-result"),26,[language stringByAppendingString:@" queued result at 200%"],failures);
            CheckFont((id)Find(body,@"history-start"),buttonSize*2,[language stringByAppendingString:@" queued start action at 200%"],failures);
            CheckDetailWidth(workspace,@"queued restore result");
            [workspace showRestoreResult:Result(request,@"restored") error:nil request:request];DrainReplies();
            CheckFont((id)Find(body,@"restore-result"),26,[language stringByAppendingString:@" terminal result at 200%"],failures);
            CheckFont(FindText(body,longName),28,[language stringByAppendingString:@" terminal updates must not scale existing text twice"],failures);
            CheckDetailWidth(workspace,@"terminal restore result");
            [workspace closeDetail:nil];

            NSString *issueDetail=@"Open this location in Finder and check access permissions before trying again.";
            NSButton *issueSender=[NSClassFromString(@"WorkspaceButton") new];
            [issueSender setValue:@{@"title":@"Check access permissions",@"path":longPath,@"detail":issueDetail,@"requiresAction":@YES} forKey:@"record"];
            [workspace showIssue:issueSender];body=[workspace valueForKey:@"detailBody"];
            CheckFont(FindText(body,longPath),24,[language stringByAppendingString:@" issue path at 200%"],failures);
            CheckFont(FindText(body,issueDetail),26,[language stringByAppendingString:@" issue explanation at 200%"],failures);
            CheckFont((id)Find(body,@"issue-folders"),buttonSize*2,[language stringByAppendingString:@" issue action at 200%"],failures);
            CheckDetailWidth(workspace,@"issue details");[workspace closeDetail:nil];
            [workspace selectSection:@"activity"];DrainReplies();
            NSDictionary *activity=@{@"sequence":@1,@"kind":@"error",@"phase":@"metadata",@"at":@1000,@"path":longPath,
                @"errno":@13,@"reason":@"Permission denied (os error 13)",@"occurrences":@2,@"first_at":@900,@"resolved_at":@2000,@"resolution":@"checked"};
            [workspace updateActivity:ActivityEvents(@[activity]) error:nil];SelectRow(workspace,0);[workspace showActivityItem:nil];
            body=[workspace valueForKey:@"detailBody"];
            NSDictionary *display=JasoActivityPresentation(activity,[language isEqual:@"ko"]);
            CheckFont(FindText(body,display[@"title"]),42,[language stringByAppendingString:@" resolved Activity heading at 200%"],failures);
            CheckFont(FindText(body,display[@"detail"]),26,[language stringByAppendingString:@" cause and resolution at 200%"],failures);
            CheckFont(FindText(body,longPath),26,[language stringByAppendingString:@" Activity path at 200%"],failures);
            CheckDetailWidth(workspace,@"resolved Activity details");[workspace closeDetail:nil];
        }
        Require(failures.count==0,[failures componentsJoinedByString:@"\n"]);
        Require(!workspace.window.visible,@"Detail fixtures must not show or activate the workspace");
    } @finally {
        workspace.requestHandler=nil;[workspace close];
        method_setImplementation(begin,originalBegin);method_setImplementation(end,originalEnd);method_setImplementation(click,originalClick);
        JasoSetContentZoom(1);JasoSetLanguagePreference(@"en");
    }
    printf("PASS real detail layout and EN/KO 200%% fonts for Activity causes/resolutions, history, issues, preview and restore results\n");
}

// Observe real AppKit work; the fixture never substitutes controller behavior.
static NSUInteger FullReloads, PartialReloads, LabelCreations, PageRenders, OffMainRenders;
static IMP OriginalReload, OriginalPartialReload, OriginalLabel, OriginalRender;
static void CountReload(id table,SEL selector) { FullReloads++;((void(*)(id,SEL))OriginalReload)(table,selector); }
static void CountPartialReload(id table,SEL selector,NSIndexSet *rows,NSIndexSet *columns) { PartialReloads+=rows.count;((void(*)(id,SEL,id,id))OriginalPartialReload)(table,selector,rows,columns); }
static id CountLabel(id type,SEL selector,NSString *value) { LabelCreations++;return ((id(*)(id,SEL,id))OriginalLabel)(type,selector,value); }
static void CountRender(id workspace,SEL selector) { if(!NSThread.isMainThread){OffMainRenders++;return;}PageRenders++;((void(*)(id,SEL))OriginalRender)(workspace,selector); }
static void ObserveUI(BOOL start) {
    Method reload=class_getInstanceMethod(NSTableView.class,@selector(reloadData));
    Method partial=class_getInstanceMethod(NSTableView.class,@selector(reloadDataForRowIndexes:columnIndexes:));
    Method label=class_getClassMethod(NSTextField.class,@selector(wrappingLabelWithString:));
    Method render=class_getInstanceMethod(JasoWorkspaceWindowController.class,@selector(render));
    if(start){
        FullReloads=PartialReloads=LabelCreations=PageRenders=OffMainRenders=0;
        OriginalReload=method_setImplementation(reload,(IMP)CountReload);
        OriginalPartialReload=method_setImplementation(partial,(IMP)CountPartialReload);
        OriginalLabel=method_setImplementation(label,(IMP)CountLabel);
        OriginalRender=method_setImplementation(render,(IMP)CountRender);
    } else {
        method_setImplementation(reload,OriginalReload);method_setImplementation(partial,OriginalPartialReload);
        method_setImplementation(label,OriginalLabel);method_setImplementation(render,OriginalRender);
    }
}
static NSDictionary *AdvancedClock(NSDictionary *response,NSUInteger tick) {
    NSMutableDictionary *live=[response[@"activity"] mutableCopy];
    live[@"snapshot_generated_at"]=@(1000+tick);live[@"last_progress_age_seconds"]=@(tick);
    live[@"phase_elapsed_seconds"]=@(tick);
    return @{@"available":@YES,@"activity":[live copy]};
}
static void TestIncrementalPresentation(void) {
    JasoWorkspaceWindowController *workspace=JasoWorkspaceWindowController.new;
    NSDictionary *snapshot=@{@"running":@YES,@"paused":@NO,@"apply":@YES,@"pending_recovery":@NO,@"baseline_complete":@YES,@"pending_jobs":@0,
        @"directory_retry_items":@[@{@"path":@"/disposable-fixture/wait",@"reason":@"Permission denied (os error 13)",@"attempts":@1}]};
    [workspace updateSnapshot:snapshot error:nil updatedAt:NSDate.date];
    [workspace selectSection:@"activity"];
    NSDictionary *activity=Activity(@"incremental",@[@1,@2,@3],0);
    [workspace updateActivity:activity error:nil];[workspace.window.contentView layoutSubtreeIfNeeded];
    NSTableView *table=[workspace valueForKey:@"activityTable"];
    NSView *issue=[(NSStackView *)[workspace valueForKey:@"activityIssues"] arrangedSubviews].lastObject;
    SelectRow(workspace,1);
    ObserveUI(YES);
    @try {
        for(NSUInteger tick=1;tick<=4;tick++)[workspace updateActivity:AdvancedClock(activity,tick) error:nil];
        Require(FullReloads==0&&PartialReloads==0,@"Advancing only generated timestamps and age must not reload Activity rows");
        Require(LabelCreations==0,@"An unchanged activity response must not recreate labels or current issue controls");
        Require([(NSStackView *)[workspace valueForKey:@"activityIssues"] arrangedSubviews].lastObject==issue,@"An unchanged issue must retain its real control");
        Require(table.selectedRow==1,@"Skipping unchanged rows must preserve the selected event");
        NSMutableDictionary *live=[activity[@"activity"] mutableCopy];
        NSMutableArray *events=[live[@"events"] mutableCopy];NSMutableDictionary *event=[events[1] mutableCopy];
        event[@"kind"]=@"error";event[@"errno"]=@13;event[@"occurrences"]=@2;events[1]=event;live[@"events"]=events;
        [workspace updateActivity:@{@"available":@YES,@"activity":live} error:nil];
        Require(PartialReloads>0||FullReloads>0,@"A changed event with the same sequence must update its existing row");
        Require([[[workspace valueForKey:@"activityRows"] objectAtIndex:1][@"occurrences"] isEqual:@2],@"Repeated observations must retain their updated occurrence count");
        NSTextField *cell=(id)[workspace tableView:table viewForTableColumn:table.tableColumns.firstObject row:1];
        Require([cell.stringValue containsString:@"Access was denied"],@"A same-sequence cause change must reach the visible cell");
        NSUInteger beforeMutation=FullReloads+PartialReloads;
        event[@"occurrences"]=@3;
        Require([[[workspace valueForKey:@"activityRows"] objectAtIndex:1][@"occurrences"] isEqual:@2],@"A mutable caller payload must not alter the retained presentation snapshot before update");
        [workspace updateActivity:@{@"available":@YES,@"activity":live} error:nil];
        Require(FullReloads+PartialReloads>beforeMutation,@"An in-place nested event edit must invalidate its displayed row on the next update");
        event=[event mutableCopy];event[@"resolved_at"]=@200;event[@"resolution"]=@"checked";events=[events mutableCopy];events[1]=event;live=[live mutableCopy];live[@"events"]=events;
        [workspace updateActivity:@{@"available":@YES,@"activity":live} error:nil];
        cell=(id)[workspace tableView:table viewForTableColumn:table.tableColumns.firstObject row:1];
        Require([cell.stringValue containsString:@"Checked successfully later"]&&table.selectedRow==1,@"Resolution must update the selected row without changing its identity");
        NSUInteger reloaded=FullReloads+PartialReloads;
        live=[live mutableCopy];live[@"counters"]=@{@"processed":@99,@"renamed":@8};
        [workspace updateActivity:@{@"available":@YES,@"activity":live} error:nil];
        Require([[[workspace valueForKey:@"activityNumbers"] stringValue] containsString:@"99"]&&FullReloads+PartialReloads==reloaded,@"Counter changes update the header without reloading unchanged events");
        [workspace updateActivity:nil error:@"Fixture disconnected"];
        Require([[workspace valueForKey:@"activityRows"] count]==0&&[[workspace valueForKey:@"activityProgress"] isHidden],@"A read error must clear available rows and stop the indicator");
        [workspace updateActivity:activity error:nil];
        Require([[workspace valueForKey:@"activityRows"] count]==3,@"Recovery must restore rows even when the worker returns the earlier content");
        [workspace selectSection:@"status"];
        NSView *page=[workspace valueForKey:@"scroll"];
        NSUInteger renders=PageRenders;
        [workspace updateSnapshot:snapshot error:nil updatedAt:[NSDate dateWithTimeIntervalSinceNow:1]];
        Require([workspace valueForKey:@"scroll"]==page&&PageRenders==renders,@"An unchanged status refresh must retain its page and controls");
        NSMutableDictionary *stopped=[snapshot mutableCopy];stopped[@"running"]=@NO;
        [workspace updateSnapshot:stopped error:nil updatedAt:NSDate.date];
        Require([Find(workspace.window.contentView,@"workspace-status-title") isKindOfClass:NSTextField.class],@"A status transition must keep a visible current status heading");
        [workspace selectSection:@"activity"];
        Require([[workspace valueForKey:@"activityRows"] count]==3,@"Returning to Activity must initialize a new table from the retained content");
        NSUInteger beforeZone=FullReloads+PartialReloads;
        [NSNotificationCenter.defaultCenter postNotificationName:NSSystemTimeZoneDidChangeNotification object:nil];
        Require(FullReloads+PartialReloads>beforeZone,@"A time-zone change must invalidate displayed Activity times");
    } @finally {ObserveUI(NO);[workspace close];}
    puts("PASS unchanged presentation, same-sequence updates, counters, errors, navigation and time-zone invalidation");
}
static void TestFormattingReadingState(void) {
    JasoWorkspaceWindowController *workspace=JasoWorkspaceWindowController.new;
    [workspace.window setContentSize:NSMakeSize(660,560)];[workspace selectSection:@"activity"];
    NSMutableArray *sequences=NSMutableArray.new;for(NSUInteger i=1;i<=80;i++)[sequences addObject:@(i)];
    [workspace updateActivity:Activity(@"formatting",sequences,0) error:nil];SelectRow(workspace,12);
    [workspace.window.contentView layoutSubtreeIfNeeded];
    NSTableView *table=[workspace valueForKey:@"activityTable"];
    NSClipView *feed=table.enclosingScrollView.contentView;NSScrollView *outer=[workspace valueForKey:@"scroll"];
    [feed scrollToPoint:NSMakePoint(0,NSMinY([table rectOfRow:10])+7)];[outer.contentView scrollToPoint:NSMakePoint(0,40)];
    NSPoint feedOrigin=feed.bounds.origin,outerOrigin=outer.contentView.bounds.origin;
    Require(feedOrigin.y>0&&outerOrigin.y>0,@"Formatting regression needs both reading positions away from zero");
    ObserveUI(YES);
    @try {
        [NSNotificationCenter.defaultCenter postNotificationName:NSSystemTimeZoneDidChangeNotification object:nil];
        table=[workspace valueForKey:@"activityTable"];feed=table.enclosingScrollView.contentView;outer=[workspace valueForKey:@"scroll"];
        BOOL positionKept=fabs(feed.bounds.origin.y-feedOrigin.y)<1&&fabs(outer.contentView.bounds.origin.y-outerOrigin.y)<1&&table.selectedRow==12;
        dispatch_semaphore_t posted=dispatch_semaphore_create(0);
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY,0),^{
            [NSNotificationCenter.defaultCenter postNotificationName:NSCurrentLocaleDidChangeNotification object:nil];dispatch_semaphore_signal(posted);
        });
        NSDate *deadline=[NSDate dateWithTimeIntervalSinceNow:2];
        while(dispatch_semaphore_wait(posted,DISPATCH_TIME_NOW)!=0&&deadline.timeIntervalSinceNow>0)[NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.005]];
        DrainReplies();
        Require(positionKept&&OffMainRenders==0,[NSString stringWithFormat:@"Formatting must preserve both reading positions and render on main (position=%d, off-main=%lu)",positionKept,(unsigned long)OffMainRenders]);
    } @finally {ObserveUI(NO);[workspace close];}
    puts("PASS formatting preserves reading position and routes background notifications to main");
}
static void TestActivityPublication(void) {
    JasoWorkspaceWindowController *main=JasoWorkspaceWindowController.new,*detached=JasoWorkspaceWindowController.new;
    [main setValue:detached forKey:@"activityWindow"];
    __block JasoWorkspaceReply pending=nil;
    main.requestHandler=^(NSString *name,NSDictionary *parameters,JasoWorkspaceReply reply){(void)parameters;Require([name isEqual:@"activity"],@"Publication fixture only requests Activity");pending=reply;};
    [main selectSection:@"activity"];[detached selectSection:@"activity"];
    @try {
        [main updateActivity:Activity(@"published-A",@[@1,@2],0) error:nil];
        Require([[detached valueForKey:@"activityRows"] count]==2,@"A fresh shared result must immediately publish to the detached Activity view");
        SelectRow(detached,1);
        [main updateActivity:Activity(@"published-B",@[@1,@2],0) error:nil];
        Require([(NSTableView *)[detached valueForKey:@"activityTable"] selectedRow]==-1,@"Published session restarts must clear detached selection");
        [main setValue:@YES forKey:@"closed"];
        [main updateActivity:nil error:@"Disconnected fixture"];
        Require([[detached valueForKey:@"activityRows"] count]==0,@"A closed main window must still forward query failure to its detached view");
        [main setValue:@NO forKey:@"closed"];
        Require(pending!=nil,@"The ordering fixture must hold a real controller request completion");
        pending(Activity(@"older-completion",@[@1],0),nil);
        [main updateActivity:Activity(@"newer-publication",@[@1,@2,@3],0) error:nil];
        DrainReplies();
        Require([[[main valueForKey:@"activity"] objectForKey:@"activity"][@"session_id"] isEqual:@"newer-publication"]&&[[detached valueForKey:@"activityRows"] count]==3,@"An already-main completion must not queue an older result behind a newer shared publication");
    } @finally {main.requestHandler=nil;[main setValue:nil forKey:@"activityWindow"];[main close];[detached close];}
    puts("PASS immediate shared Activity publication and detached session/error updates");
}
static double CPUSeconds(struct rusage usage) { return usage.ru_utime.tv_sec+usage.ru_utime.tv_usec/1e6+usage.ru_stime.tv_sec+usage.ru_stime.tv_usec/1e6; }
static void BenchmarkPresentation(BOOL active) {
    JasoWorkspaceWindowController *activity=JasoWorkspaceWindowController.new,*status=JasoWorkspaceWindowController.new;
    NSMutableArray *sequences=NSMutableArray.new;for(NSUInteger i=1;i<=200;i++)[sequences addObject:@(i)];
    NSDictionary *response=Activity(@"benchmark",sequences,0);
    NSDictionary *snapshot=@{@"running":@YES,@"paused":@NO,@"apply":@YES,@"pending_recovery":@NO,@"baseline_complete":@YES,@"pending_jobs":@0,
        @"directory_retry_items":@[@{@"path":@"/disposable-fixture/wait",@"reason":@"Permission denied (os error 13)",@"attempts":@1}]};
    [activity selectSection:@"activity"];[activity updateSnapshot:snapshot error:nil updatedAt:NSDate.date];[activity updateActivity:response error:nil];
    [status updateSnapshot:snapshot error:nil updatedAt:NSDate.date];
    [activity.window.contentView layoutSubtreeIfNeeded];[status.window.contentView layoutSubtreeIfNeeded];
    ObserveUI(YES);struct rusage before,after;getrusage(RUSAGE_SELF,&before);double started=NSProcessInfo.processInfo.systemUptime;
    for(NSUInteger tick=1;tick<=300;tick++){@autoreleasepool {
        if(active){
            NSMutableDictionary *live=[response[@"activity"] mutableCopy];live[@"counters"]=@{@"processed":@(tick),@"renamed":@(tick/10)};
            if(tick%10==0){
                NSMutableArray *events=[live[@"events"] mutableCopy];NSMutableDictionary *event=[events.lastObject mutableCopy];
                event[@"kind"]=@"error";event[@"errno"]=@13;event[@"occurrences"]=@(tick/10);event[@"at"]=@(100+tick);
                if(tick%20==0){event[@"resolved_at"]=@(101+tick);event[@"resolution"]=@"checked";}
                else {[event removeObjectForKey:@"resolved_at"];[event removeObjectForKey:@"resolution"];}
                events[events.count-1]=[event copy];live[@"events"]=[events copy];
            }
            if(tick>=150)live[@"session_id"]=@"benchmark-restarted";
            response=@{@"available":@YES,@"activity":[live copy]};
        }
        [activity updateActivity:AdvancedClock(response,tick) error:nil];
        if(tick%5==0){[activity updateSnapshot:snapshot error:nil updatedAt:NSDate.date];[status updateSnapshot:snapshot error:nil updatedAt:NSDate.date];}
        [activity.window.contentView layoutSubtreeIfNeeded];[status.window.contentView layoutSubtreeIfNeeded];
    }}
    double wall=NSProcessInfo.processInfo.systemUptime-started;getrusage(RUSAGE_SELF,&after);ObserveUI(NO);
    printf("{\"fixture\":\"%s\",\"ticks\":300,\"events\":200,\"wall_seconds\":%.6f,\"cpu_seconds\":%.6f,\"label_factory_calls\":%lu,\"table_full_reloads\":%lu,\"table_partial_rows\":%lu,\"page_renders\":%lu,\"peak_rss_bytes\":%ld}\n",active?"changing-activity-status":"unchanged-activity-status",wall,CPUSeconds(after)-CPUSeconds(before),(unsigned long)LabelCreations,(unsigned long)FullReloads,(unsigned long)PartialReloads,(unsigned long)PageRenders,after.ru_maxrss);
    [activity close];[status close];
}

int main(int argc,const char **argv) {
    @autoreleasepool {
        NSString *suite=[@"jaso-workspace-runtime-" stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults=[[NSUserDefaults alloc] initWithSuiteName:suite];
        Method method=class_getClassMethod(NSUserDefaults.class,@selector(standardUserDefaults));
        IMP original=method_setImplementation(method,(IMP)IsolatedDefaults);
        int status=0;
        @try {
            [NSApplication sharedApplication];
            [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
            JasoSetLanguagePreference(@"en");
            if(argc==2&&(strcmp(argv[1],"--benchmark")==0||strcmp(argv[1],"--benchmark-active")==0)){BenchmarkPresentation(strcmp(argv[1],"--benchmark-active")==0);return 0;}
            NSMutableArray *performanceFailures=NSMutableArray.new;
            for(void (^test)(void) in @[^ {TestActivityPublication();},^ {TestIncrementalPresentation();},^ {TestFormattingReadingState();}]){
                @try {test();} @catch(NSException *failure){[performanceFailures addObject:failure.reason];}
            }
            Require(performanceFailures.count==0,[performanceFailures componentsJoinedByString:@"\n"]);
            TestRestoreAttribution();
            TestNameComparison();
            TestHistoryRowFocus();
            TestActivitySelection();
            TestZoomReadingState();
            TestActivityCauses();
            TestActivityPhases();
            TestDetailZoom();
        } @catch(NSException *failure) { fprintf(stderr,"FAIL %s\n",failure.reason.UTF8String);status=1; }
        @finally { method_setImplementation(method,original);[TestDefaults removePersistentDomainForName:suite]; }
        return status;
    }
}
