#import <AppKit/AppKit.h>
#import "../macos/WorkspaceWindow.h"
#import "../macos/SetupWindow.h"
#import "../macos/SettingsWindow.h"
#import "../macos/ContentZoom.h"
#import "../macos/Localization.h"
#import <objc/runtime.h>

static NSUserDefaults *TestDefaults;
static id IsolatedDefaults(id self, SEL selector) { return TestDefaults; }
static NSScrollerStyle TestScrollerStyle = NSScrollerStyleLegacy;
static NSScrollerStyle PreferredScrollerStyle(id self, SEL selector) { return TestScrollerStyle; }
static void Require(BOOL condition, NSString *message) {
    if (!condition) @throw [NSException exceptionWithName:@"LayoutFailure" reason:message userInfo:nil];
}
static NSView *Find(NSView *view, NSString *identifier) {
    if ([view.identifier isEqual:identifier]) return view;
    for (NSView *child in view.subviews) { NSView *found=Find(child,identifier); if(found)return found; }
    return nil;
}
static void Settle(NSWindow *window) {
    [window.contentView layoutSubtreeIfNeeded];
    [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:.08]];
    [window.contentView layoutSubtreeIfNeeded];
}
static void CheckControls(NSView *view, NSView *root) {
    if(!view.hiddenOrHasHiddenAncestor && ([view isKindOfClass:NSButton.class]||[view isKindOfClass:NSTextField.class])) {
        NSRect frame=[view convertRect:view.bounds toView:root];
        Require(NSMinX(frame)>=-1&&NSMaxX(frame)<=root.bounds.size.width+1,[NSString stringWithFormat:@"Control %@ extends outside the chosen window",view.identifier?:NSStringFromClass(view.class)]);
        if([@[@"activity-window",@"activity-refresh",@"history-refresh",@"setup-preview",@"setup-cancel",@"setup-save",@"setup-start"] containsObject:view.identifier])Require(((NSButton *)view).cell.cellSize.width<=view.bounds.size.width+1,[NSString stringWithFormat:@"Action %@ is clipped",view.identifier]);
    }
    for(NSView *child in view.subviews)CheckControls(child,root);
}
static void Capture(NSWindow *window, NSString *path) {
    NSView *view=window.contentView;
    NSBitmapImageRep *bitmap=[view bitmapImageRepForCachingDisplayInRect:view.bounds];
    [view cacheDisplayInRect:view.bounds toBitmapImageRep:bitmap];
    Require([[bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}] writeToFile:path atomically:YES],@"Layout capture failed");
}
int main(int argc,const char **argv) {
    @autoreleasepool {
        [NSApplication sharedApplication];[NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];[NSApp finishLaunching];
        NSString *suite=[@"jaso-workspace-layout-" stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults=[[NSUserDefaults alloc] initWithSuiteName:suite];
        Method method=class_getClassMethod(NSUserDefaults.class,@selector(standardUserDefaults));
        IMP original=method_setImplementation(method,(IMP)IsolatedDefaults);
        Method scrollerMethod=class_getClassMethod(NSScroller.class,@selector(preferredScrollerStyle));
        IMP originalScroller=method_setImplementation(scrollerMethod,(IMP)PreferredScrollerStyle);
        @try {
            for(NSNumber *style in @[@(NSScrollerStyleLegacy),@(NSScrollerStyleOverlay)]){
            TestScrollerStyle=style.integerValue;
            JasoWorkspaceWindowController *workspace=JasoWorkspaceWindowController.new;
            JasoSetupWindowController *setup=JasoSetupWindowController.new;
            JasoSettingsWindowController *settings=JasoSettingsWindowController.new;
            Require(workspace.window.contentView.bounds.size.width>=602 && workspace.window.contentView.bounds.size.width<=688,@"The default workspace must be 20–30 percent narrower than the previous 860-point window");
            NSDictionary *config=@{@"scope":@"all-user-files",@"roots":@[],@"excludes":@[@"/fixture/A folder with a long name/Excluded documents"],@"apply":@YES};
            NSArray *drives=@[@{@"uuid":@"one",@"mount":@"/Volumes/A long remembered drive name",@"included":@YES,@"connected":@NO},@{@"uuid":@"two",@"mount":@"/Volumes/Second drive",@"included":@YES,@"connected":@NO}];
            [setup updateConfiguration:@{@"config":config,@"revision":@"layout",@"drive_inventory":drives} error:nil];
            [workspace embedView:[setup embeddedContentViewForWindow:workspace.window] inSection:@"folders"];
            [workspace embedView:[settings embeddedContentViewForWindow:workspace.window] inSection:@"settings"];
            NSString *issuePath=[@"/fixture/" stringByAppendingString:[@"Long folder name " stringByPaddingToLength:180 withString:@"and supporting documents " startingAtIndex:0]];
            [workspace updateSnapshot:@{@"running":@YES,@"paused":@NO,@"apply":@YES,@"pending_recovery":@NO,@"baseline_complete":@YES,@"pending_jobs":@0,@"today_renamed":@0,@"directory_retry_count":@1,@"directory_retry_items":@[@{@"path":issuePath,@"reason":@"Permission denied (os error 13)",@"attempts":@1}]} error:nil updatedAt:NSDate.date];
            NSString *longName=@"2026년 공동 연구개발사업 최종 결과 보고서 및 참여기관 확인서와 부속 서류.pdf";
            NSDictionary *record=@{@"operation_id":@"layout-record",@"old_name":[longName decomposedStringWithCanonicalMapping],@"new_name":longName,@"new_path":[@"/fixture/Shared documents/Research and development/" stringByAppendingString:longName],@"timestamp":@1789212000,@"result":@"renamed"};
            [workspace updateHistory:@{@"items":@[record],@"total":@1,@"today_count":@1} error:nil];
            [workspace.window orderFront:nil];
            NSString *folder=argc>1?@(argv[1]):nil;
            if(folder)[NSFileManager.defaultManager createDirectoryAtPath:folder withIntermediateDirectories:YES attributes:nil error:nil];
            for(NSString *language in @[@"en",@"ko"]){
                JasoSetLanguagePreference(language);[workspace reloadLocalization];[setup reloadLocalization];[settings reloadLocalization];
                for(NSNumber *scale in @[@1,@2]){
                    JasoSetContentZoom(scale.doubleValue);
                    for(NSNumber *width in @[@620,@660,@860]){
                    [workspace.window setContentSize:NSMakeSize(width.doubleValue,660)];
                    for(NSString *section in @[@"status",@"activity",@"history",@"folders",@"settings",@"folders"]){
                        [workspace selectSection:section];Settle(workspace.window);
                        if([section isEqual:@"activity"]){
                            [workspace updateActivity:@{@"available":@YES,@"activity":@{@"session_id":@"layout-activity",@"state":@"idle",@"phase":@"waiting_for_events",@"events":@[
                                @{@"sequence":@1,@"at":@1789212000,@"kind":@"deferred",@"phase":@"metadata",@"path":@"/fixture/Cloud documents/분기 보고서.pdf",@"reason":@"dataless-file",@"occurrences":@12,@"first_at":@1789211900},
                                @{@"sequence":@2,@"at":@1789212100,@"kind":@"error",@"phase":@"metadata",@"path":@"/fixture/Shared documents/회의록.docx",@"reason":@"Permission denied (os error 13)",@"errno":@13},
                                @{@"sequence":@3,@"at":@1789212200,@"kind":@"error",@"phase":@"enumerating",@"path":@"/fixture/Cloud documents/지난 보고서.pdf",@"reason":@"Operation timed out (os error 60)",@"errno":@60,@"resolved_at":@1789212300,@"resolution":@"checked"}
                            ]}} error:nil];Settle(workspace.window);
                            NSStackView *issues=[workspace valueForKey:@"activityIssues"];
                            for(NSView *view in issues.arrangedSubviews)if([view isKindOfClass:NSButton.class])Require(((NSButton *)view).font.pointSize>=12*scale.doubleValue,@"Refreshed issue actions must preserve the selected text size");
                        }
                        NSScrollView *pageScroll=[workspace valueForKey:@"scroll"];
                        if(pageScroll)Require(pageScroll.scrollerStyle==TestScrollerStyle,@"The fixture must exercise the requested scroller style");
                        Require(workspace.window.contentView.bounds.size.width<=width.doubleValue+1,[NSString stringWithFormat:@"%@ expanded the window beyond %@ points (%@, %@x, scroller %@): %.0f",section,width,language,scale,style,workspace.window.contentView.bounds.size.width]);
                        CheckControls(workspace.window.contentView,workspace.window.contentView);
                        NSView *nav=Find(workspace.window.contentView,@"nav-status");
                        NSRect navFrame=[nav convertRect:nav.bounds toView:workspace.window.contentView];
                        Require(nav&&!nav.hiddenOrHasHiddenAncestor&&NSMinX(navFrame)>=0&&NSMaxX(navFrame)<=152,@"Sidebar navigation must stay inside its compact reserved column");
                        NSView *page=[workspace valueForKey:@"pageHost"];
                        Require(page.clipsToBounds,@"Page drawing must be clipped to protect the adjacent sidebar");
                        NSView *heading=Find(page,[section isEqual:@"folders"]?@"setup-heading":@"settings-heading");
                        if(heading){NSRect frame=[heading convertRect:heading.bounds toView:workspace.window.contentView];Require(NSMinX(frame)>=NSMaxX(navFrame),@"Embedded content must remain to the right of navigation");}
                        if(folder)Capture(workspace.window,[folder stringByAppendingPathComponent:[NSString stringWithFormat:@"%@-%@-%@-%@-%@.png",section,language,scale,width,style]]);
                        if(scale.doubleValue==1 && width.intValue==660){
                            NSRect originalFrame=workspace.window.frame;
                            for(NSNumber *zoomOnly in @[@2,@1]){
                                JasoSetContentZoom(zoomOnly.doubleValue);Settle(workspace.window);
                                Require(NSEqualRects(originalFrame,workspace.window.frame),[NSString stringWithFormat:@"Changing only text size must preserve the %@ window frame (%@)",section,language]);
                                CheckControls(workspace.window.contentView,workspace.window.contentView);
                            }
                        }
                        if([section isEqual:@"activity"] && scale.doubleValue==2 && width.intValue==660){
                            NSStackView *resizeActions=(NSStackView *)Find(workspace.window.contentView,@"activity-window").superview;
                            // Exercise the native control minimum that must not lock
                            // the window to a previous horizontal arrangement.
                            for(NSControl *control in resizeActions.arrangedSubviews)[control.widthAnchor constraintGreaterThanOrEqualToConstant:control.cell.cellSize.width].active=YES;
                            for(NSNumber *resizedWidth in @[@860,@620,@660]){
                                [workspace.window setContentSize:NSMakeSize(resizedWidth.doubleValue,660)];Settle(workspace.window);
                                if(resizedWidth.intValue==860)Require(resizeActions.fittingSize.width>412,@"The resize fixture must put pressure on the narrower viewport");
                                Require(workspace.window.contentView.bounds.size.width<=resizedWidth.doubleValue+1,[NSString stringWithFormat:@"Resizing the visible activity page must retain the requested width: requested=%@ actual=%.1f language=%@ zoom=%@ scroller=%@",resizedWidth,workspace.window.contentView.bounds.size.width,language,scale,style]);
                                for(NSView *control in resizeActions.arrangedSubviews)Require(control.superview==resizeActions&&!control.hiddenOrHasHiddenAncestor,@"Activity actions must stay visible while their row reflows");
                                CheckControls(workspace.window.contentView,workspace.window.contentView);
                                NSStackView *actions=(NSStackView *)Find(workspace.window.contentView,@"activity-window").superview;
                                if(resizedWidth.intValue==860)Require(actions.orientation==NSUserInterfaceLayoutOrientationHorizontal,@"Activity actions return to one row when the window has room");
                            }
                        }
                    }
                    }
                }
            }
            [workspace close];
            }
            puts("PASS compact workspace with real embedded Folders/Settings in both languages at 100/200 percent and legacy/overlay scrollbars");
        } @catch(NSException *error){fprintf(stderr,"FAIL %s\n",error.reason.UTF8String);return 1;}
        @finally {method_setImplementation(scrollerMethod,originalScroller);method_setImplementation(method,original);[TestDefaults removePersistentDomainForName:suite];}
    }
    return 0;
}
