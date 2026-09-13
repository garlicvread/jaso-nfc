#import "WorkspaceWindow.h"
#import "StatusPresentation.h"
#import "NamePresentation.h"
#import "Localization.h"
#import "ContentZoom.h"

static NSString *W(NSString *en, NSString *ko) { return JasoText(en,ko); }
static NSString *S(id value) { return [value isKindOfClass:NSString.class] ? value : @""; }
static NSDictionary *D(id value) { return [value isKindOfClass:NSDictionary.class] ? value : @{}; }
static NSArray *A(id value) { return [value isKindOfClass:NSArray.class] ? value : @[]; }
static NSNumber *N(id value) { return [value isKindOfClass:NSNumber.class] ? value : nil; }
static NSString *Count(id value) { return N(value) ? [NSNumberFormatter localizedStringFromNumber:value numberStyle:NSNumberFormatterDecimalStyle] : @"—"; }
static NSString *Time(id value) {
    if (S(value).length) {
        NSISO8601DateFormatter *iso=NSISO8601DateFormatter.new;
        NSDate *date=[iso dateFromString:S(value)];
        if(!date) { NSDateFormatter *local=NSDateFormatter.new;local.locale=[NSLocale localeWithLocaleIdentifier:@"en_US_POSIX"];local.dateFormat=@"yyyy-MM-dd HH:mm:ss";date=[local dateFromString:[S(value) stringByReplacingOccurrencesOfString:@"T" withString:@" "]]; }
        if(!date)return S(value);
        value=@(date.timeIntervalSince1970);
    }
    if (!N(value)) return @"—";
    NSDateFormatter *format=NSDateFormatter.new; format.dateStyle=NSDateFormatterShortStyle; format.timeStyle=NSDateFormatterMediumStyle;
    return [format stringFromDate:[NSDate dateWithTimeIntervalSince1970:[value doubleValue]]];
}
NSDictionary *JasoWorkspaceStatus(NSDictionary *snapshot, NSString *error, BOOL korean) {
    NSMutableDictionary *result=[JasoStatusPresentation(snapshot,error,korean) mutableCopy];
    if (!error.length && N(snapshot[@"pending_recovery"]) && [N(snapshot[@"running"]) boolValue] && N(snapshot[@"paused"]) && ![snapshot[@"paused"] boolValue] && [N(snapshot[@"apply"]) boolValue] && ![N(snapshot[@"pending_recovery"]) boolValue]) {
        result[@"title"]=korean?@"자동 정리가 켜져 있습니다":@"Automatic cleanup is on";
        result[@"tone"]=@"good"; result[@"symbol"]=@"checkmark.circle";
        NSDictionary *live=D(snapshot[@"activity"]);NSString *phase=S(live[@"phase"]);
        if([phase isEqual:@"idle"] || ([N(snapshot[@"baseline_complete"]) boolValue] && N(snapshot[@"pending_jobs"]) && [snapshot[@"pending_jobs"] unsignedLongLongValue]==0))result[@"subtitle"]=korean?@"새 파일이 추가되면 파일 이름을 확인합니다.":@"New files are checked as they arrive.";
        else if([S(live[@"state"]) isEqual:@"waiting_metadata"])result[@"subtitle"]=korean?@"현재 폴더의 파일 정보를 확인하고 있습니다.":@"Checking file metadata in the current folder.";
        else if(N(snapshot[@"baseline_complete"])&&![snapshot[@"baseline_complete"] boolValue])result[@"subtitle"]=korean?@"선택한 폴더의 파일 목록을 만들고 있습니다.":@"Building a file list for your selected folders.";
        else result[@"subtitle"]=korean?@"선택한 폴더의 파일 이름을 확인하고 있습니다.":@"Checking file names in your selected folders.";
    }
    if([D(D(snapshot[@"activity"])[@"issue"])[@"code"] isEqual:@"low_storage"]){result[@"title"]=korean?@"저장공간을 확인해 주세요":@"Storage needs attention";result[@"subtitle"]=korean?@"작업 기록을 저장할 공간이 부족해 정리를 잠시 멈췄습니다. 설정에서 저장공간을 확인하세요.":@"Cleanup is waiting for space to save its records. Check Storage in Settings.";result[@"tone"]=@"warning";}
    if ([result[@"primaryAction"] isEqual:@"resume"]) result[@"primaryTitle"]=korean?@"재개":@"Resume";
    if ([result[@"primaryAction"] isEqual:@"pause"]) result[@"primaryTitle"]=korean?@"일시 정지":@"Pause";
    NSMutableArray *issues=[A(result[@"issues"]) mutableCopy];NSMutableSet *seen=NSMutableSet.new;for(NSDictionary *issue in issues)if(S(issue[@"path"]).length)[seen addObject:issue[@"path"]];
    for(NSDictionary *location in A(result[@"locations"]))if([N(location[@"requiresAction"]) boolValue]&&![seen containsObject:S(location[@"path"])] ){[issues addObject:location];[seen addObject:S(location[@"path"])];}
    result[@"issues"]=issues;return result;
}
static NSString *RestoreMessage(id value) {
    NSString *message=S(value);if(!JasoUsesKorean())return message;
    NSDictionary *translations=@{
      @"The original name is available and the recorded item matches.":@"원래 이름으로 되돌릴 수 있습니다.",
      @"Turn on filename changes before restoring an item.":@"폴더에서 자동 정리를 시작한 뒤 이 이름을 되돌릴 수 있습니다.",
      @"History changed. Refresh this operation before restoring.":@"변경 기록이 갱신됐습니다. 이 항목을 다시 열어 확인하세요.",
      @"This operation has already been restored.":@"이미 원래 이름으로 되돌린 항목입니다.",
      @"This record has no confirmed, identifiable rename to restore.":@"이 항목에는 되돌릴 수 있는 이름 변경 기록이 없습니다.",
      @"A pending operation must finish recovery before restoring.":@"진행 중인 이름 변경의 복구를 마친 뒤 되돌릴 수 있습니다.",
      @"The recorded paths are not safe directory entry names.":@"기록된 경로를 확인할 수 없습니다. 진단 정보를 확인하세요.",
      @"The cloud provider identity cannot be verified for a safe restore.":@"이 클라우드 항목의 원본을 확인할 수 없습니다. 동기화 앱에서 파일 위치를 확인하세요.",
      @"This record has no verified file identity.":@"이전 기록으로는 같은 파일인지 확인할 수 없습니다.",
      @"The renamed item is no longer at its recorded path.":@"파일이 이동됐거나 이름이 다시 바뀌었습니다. 현재 위치를 확인하세요.",
      @"Another item occupies the recorded path.":@"기록된 위치에 다른 파일이 있습니다. Finder에서 확인하세요.",
      @"Download and verify this cloud item before restoring its name.":@"이 항목은 클라우드에 있습니다. 동기화 앱에서 파일 상태를 확인하세요.",
      @"The original name is occupied by another item.":@"같은 폴더에 원래 이름을 쓰는 다른 항목이 있습니다. 이름 충돌을 해결한 뒤 다시 시도하세요.",
      @"A later rename depends on this item or directory. Restore is unavailable.":@"이 파일이나 하위 폴더에 이후 이름 변경 기록이 있습니다. 해당 변경을 먼저 확인하세요.",
      @"The original destination cannot be verified.":@"원래 이름을 사용할 수 있는지 확인하지 못했습니다. 폴더 접근 상태를 확인하세요."
    };return translations[message]?:message;
}
static NSTextField *Label(NSString *text, CGFloat size, NSFontWeight weight) {
    NSTextField *label=[NSTextField wrappingLabelWithString:text?:@""];
    label.font=[NSFont systemFontOfSize:size weight:weight]; label.selectable=YES;
    label.translatesAutoresizingMaskIntoConstraints=NO;
    [label setContentCompressionResistancePriority:NSLayoutPriorityRequired forOrientation:NSLayoutConstraintOrientationVertical];
    return label;
}
static NSStackView *Stack(NSArray *views, BOOL vertical, CGFloat spacing) {
    NSStackView *stack=[NSStackView stackViewWithViews:views]; stack.translatesAutoresizingMaskIntoConstraints=NO;
    stack.orientation=vertical?NSUserInterfaceLayoutOrientationVertical:NSUserInterfaceLayoutOrientationHorizontal;
    stack.alignment=vertical?NSLayoutAttributeLeading:NSLayoutAttributeCenterY; stack.spacing=spacing; return stack;
}
static void Pin(NSView *view, NSView *parent, CGFloat inset) {
    [NSLayoutConstraint activateConstraints:@[[view.leadingAnchor constraintEqualToAnchor:parent.leadingAnchor constant:inset],[view.trailingAnchor constraintEqualToAnchor:parent.trailingAnchor constant:-inset],[view.topAnchor constraintEqualToAnchor:parent.topAnchor constant:inset],[view.bottomAnchor constraintEqualToAnchor:parent.bottomAnchor constant:-inset]]];
}
static void Add(NSStackView *stack, NSView *view) {
    [stack addArrangedSubview:view]; [view.widthAnchor constraintEqualToAnchor:stack.widthAnchor constant:-(stack.edgeInsets.left+stack.edgeInsets.right)].active=YES;
}
static NSView *ViewWithIdentifier(NSView *view,NSString *identifier) {
    if(!identifier.length)return nil;
    if([view.identifier isEqual:identifier])return view;
    for(NSView *child in view.subviews){NSView *found=ViewWithIdentifier(child,identifier);if(found)return found;}
    return nil;
}
@interface WorkspaceBackground : NSView @end
@implementation WorkspaceBackground
- (void)drawRect:(NSRect)rect { [NSColor.windowBackgroundColor setFill];NSRectFill(rect); }
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance];self.needsDisplay=YES; }
@end
@interface WorkspaceClip : NSClipView @end
@implementation WorkspaceClip
- (BOOL)isFlipped { return YES; }
@end
@interface WorkspaceNavigationButton : NSButton @end
@implementation WorkspaceNavigationButton
- (void)drawRect:(NSRect)rect { if(self.state==NSControlStateValueOn){[[NSColor.controlAccentColor colorWithAlphaComponent:.16] setFill];[[NSBezierPath bezierPathWithRoundedRect:self.bounds xRadius:7 yRadius:7] fill];}[super drawRect:rect]; }
@end
@interface WorkspaceCard : NSView @end
@implementation WorkspaceCard
- (void)drawRect:(NSRect)rect {
    NSBezierPath *shape=[NSBezierPath bezierPathWithRoundedRect:NSInsetRect(self.bounds,.5,.5) xRadius:10 yRadius:10];
    [NSColor.controlBackgroundColor setFill]; [shape fill]; [NSColor.separatorColor setStroke]; shape.lineWidth=.5; [shape stroke];
}
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; self.needsDisplay=YES; }
@end
static NSView *Card(NSView *view) {
    WorkspaceCard *card=WorkspaceCard.new; card.translatesAutoresizingMaskIntoConstraints=NO; [card addSubview:view]; Pin(view,card,16); return card;
}
@interface WorkspaceButton : NSButton
@property NSDictionary *record;
@end
@implementation WorkspaceButton @end

@interface JasoWorkspaceWindowController ()
@property NSStackView *navigation;
@property NSView *pageHost;
@property NSScrollView *scroll;
@property NSStackView *body;
@property NSMutableDictionary<NSString *,NSView *> *embedded;
@property NSMutableDictionary<NSString *,NSValue *> *scrollPositions;
@property NSMutableDictionary<NSString *,NSView *> *focusPositions;
@property (readwrite,copy) NSString *selectedSection;
@property NSDictionary *snapshot;
@property NSString *snapshotError;
@property NSDate *snapshotDate;
@property NSDictionary *activity;
@property NSString *activityError;
@property NSDictionary *history;
@property NSString *historyError;
@property NSString *historySearch;
@property NSString *historyDate;
@property NSString *historyResult;
@property NSDatePicker *historyDatePicker;
@property NSButton *historyDateToggle;
@property NSUInteger historyOffset;
@property NSString *activityFilter;
@property BOOL followActivity;
@property BOOL restoringActivitySelection;
@property NSButton *followButton;
@property NSString *selectedActivitySession;
@property NSNumber *selectedActivitySequence;
@property NSTextField *activityRetention;
@property NSStackView *activityIssues;
@property NSTextField *heroTitle;
@property NSTextField *heroDescription;
@property NSTextField *activityLocation;
@property NSTextField *activityPhase;
@property NSTextField *activityNumbers;
@property NSTextField *activityTime;
@property NSProgressIndicator *activityProgress;
@property NSTableView *activityTable;
@property NSArray *activityRows;
@property NSTimer *timer;
@property NSUInteger tick;
@property BOOL busy;
@property NSMutableSet *pendingRequests;
@property NSPanel *detailPanel;
@property NSStackView *detailBody;
@property NSDictionary *selectedRecord;
@property NSView *detailReturnFocus;
@property NSDictionary *restoreRequest;
@property NSPanel *restorePanel;
@property JasoWorkspaceWindowController *activityWindow;
@property BOOL activityOnly;
@property BOOL closed;
@property NSMutableArray<NSStackView *> *adaptiveRows;
@property BOOL restoringZoomState;
@end

@implementation JasoWorkspaceWindowController
- (instancetype)init {
    NSWindow *window=[[NSWindow alloc] initWithContentRect:NSMakeRect(0,0,660,660) styleMask:NSWindowStyleMaskTitled|NSWindowStyleMaskClosable|NSWindowStyleMaskMiniaturizable|NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
    if (!(self=[super initWithWindow:window])) return nil;
    window.contentView=[[WorkspaceBackground alloc] initWithFrame:window.contentView.bounds];window.title=@"Jaso NFC"; window.minSize=NSMakeSize(620,560); window.releasedWhenClosed=NO; window.delegate=self; [window center];
    _embedded=NSMutableDictionary.new; _scrollPositions=NSMutableDictionary.new; _focusPositions=NSMutableDictionary.new; _pendingRequests=NSMutableSet.new;
    _selectedSection=@"status"; _activityFilter=@"all"; _historySearch=@""; _followActivity=YES;
    NSVisualEffectView *sidebar=NSVisualEffectView.new; sidebar.material=NSVisualEffectMaterialSidebar; sidebar.blendingMode=NSVisualEffectBlendingModeBehindWindow; sidebar.translatesAutoresizingMaskIntoConstraints=NO;
    self.navigation=Stack(@[],YES,10); self.navigation.edgeInsets=NSEdgeInsetsMake(28,16,24,16); [sidebar addSubview:self.navigation];
    [self.navigation.leadingAnchor constraintEqualToAnchor:sidebar.leadingAnchor].active=YES; [self.navigation.trailingAnchor constraintEqualToAnchor:sidebar.trailingAnchor].active=YES; [self.navigation.topAnchor constraintEqualToAnchor:sidebar.topAnchor].active=YES;
    self.pageHost=NSView.new; self.pageHost.translatesAutoresizingMaskIntoConstraints=NO; self.pageHost.clipsToBounds=YES;
    [window.contentView addSubview:sidebar]; [window.contentView addSubview:self.pageHost];
    [NSLayoutConstraint activateConstraints:@[[sidebar.leadingAnchor constraintEqualToAnchor:window.contentView.leadingAnchor],[sidebar.topAnchor constraintEqualToAnchor:window.contentView.topAnchor],[sidebar.bottomAnchor constraintEqualToAnchor:window.contentView.bottomAnchor],[sidebar.widthAnchor constraintEqualToConstant:152],[self.pageHost.leadingAnchor constraintEqualToAnchor:sidebar.trailingAnchor],[self.pageHost.topAnchor constraintEqualToAnchor:window.contentView.topAnchor],[self.pageHost.trailingAnchor constraintEqualToAnchor:window.contentView.trailingAnchor],[self.pageHost.bottomAnchor constraintEqualToAnchor:window.contentView.bottomAnchor]]];
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(visibilityChanged:) name:NSApplicationDidHideNotification object:nil];
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(visibilityChanged:) name:NSApplicationDidUnhideNotification object:nil];
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(zoomChanged:) name:JasoContentZoomDidChangeNotification object:nil];
    [self rebuildNavigation]; [self render]; return self;
}
- (NSButton *)button:(NSString *)title action:(SEL)action identifier:(NSString *)identifier {
    NSButton *button=[NSButton buttonWithTitle:title target:self action:action]; button.identifier=identifier; button.accessibilityLabel=title; return button;
}
- (void)rebuildNavigation {
    for(NSView *view in self.navigation.arrangedSubviews.copy){[self.navigation removeArrangedSubview:view];[view removeFromSuperview];}
    Add(self.navigation,Label(@"Jaso NFC",21,NSFontWeightMedium));
    NSTextField *caption=Label(W(@"Korean file names",@"한글 파일 이름 정리"),11,NSFontWeightRegular); caption.textColor=NSColor.secondaryLabelColor; Add(self.navigation,caption);
    NSArray *names=@[W(@"Status",@"상태"),W(@"Activity",@"활동"),W(@"Folders",@"폴더"),W(@"History",@"변경 기록"),W(@"Settings",@"설정")];
    NSArray *keys=@[@"status",@"activity",@"folders",@"history",@"settings"];
    for(NSUInteger i=0;i<keys.count;i++){
        NSButton *button=[WorkspaceNavigationButton buttonWithTitle:names[i] target:self action:@selector(navigate:)];button.identifier=[@"nav-" stringByAppendingString:keys[i]];button.accessibilityLabel=names[i];button.bordered=NO;
        button.buttonType=NSButtonTypeToggle; button.bezelStyle=NSBezelStyleRecessed; button.alignment=NSTextAlignmentLeft; button.state=[keys[i] isEqual:self.selectedSection];
        [button.heightAnchor constraintGreaterThanOrEqualToConstant:36].active=YES; Add(self.navigation,button);
    }
    JasoApplyContentZoom(self.navigation);
}
- (void)navigate:(NSButton *)sender { [self selectSection:[sender.identifier substringFromIndex:4]]; }
- (void)selectSection:(NSString *)section {
    if (![@[@"status",@"activity",@"folders",@"history",@"settings"] containsObject:section]) return;
    if ([self.selectedSection isEqual:section]) return;
    if (self.detailPanel) [self closeDetail:nil];
    if(self.scroll)self.scrollPositions[self.selectedSection]=[NSValue valueWithPoint:self.scroll.contentView.bounds.origin];
    NSResponder *focus=self.window.firstResponder;
    if([focus isKindOfClass:NSView.class])self.focusPositions[self.selectedSection]=(NSView *)focus;
    if([self.selectedSection isEqual:@"folders"]&&self.actionHandler)self.actionHandler(@"cancel-preview");
    self.selectedSection=section; [self rebuildNavigation]; [self render];
    if([section isEqual:@"folders"]||[section isEqual:@"settings"]){if(self.actionHandler)self.actionHandler(section);}
    else if([section isEqual:@"history"])[self loadHistory];
    else if([section isEqual:@"activity"])[self loadActivity];
}
- (void)embedView:(NSView *)view inSection:(NSString *)section {
    if(!view)return; self.embedded[section]=view;
    if([self.selectedSection isEqual:section])[self render];
}
- (void)render {
    for(NSView *view in self.pageHost.subviews.copy)[view removeFromSuperview];
    self.adaptiveRows=NSMutableArray.new;
    self.activityTable=nil;self.heroTitle=nil;self.heroDescription=nil;self.activityLocation=nil;self.scroll=nil;
    NSView *embedded=self.embedded[self.selectedSection];
    if(embedded){embedded.translatesAutoresizingMaskIntoConstraints=NO;[self.pageHost addSubview:embedded];Pin(embedded,self.pageHost,0);return;}
    self.scroll=NSScrollView.new;self.scroll.contentView=WorkspaceClip.new;self.scroll.translatesAutoresizingMaskIntoConstraints=NO;self.scroll.hasVerticalScroller=YES;self.scroll.autohidesScrollers=YES;self.scroll.drawsBackground=NO;
    self.body=Stack(@[],YES,20);self.body.edgeInsets=NSEdgeInsetsMake(30,28,30,28);self.scroll.documentView=self.body;
    [self.pageHost addSubview:self.scroll];Pin(self.scroll,self.pageHost,0);
    [NSLayoutConstraint activateConstraints:@[[self.body.widthAnchor constraintEqualToAnchor:self.scroll.contentView.widthAnchor],[self.body.leadingAnchor constraintEqualToAnchor:self.scroll.contentView.leadingAnchor],[self.body.topAnchor constraintEqualToAnchor:self.scroll.contentView.topAnchor]]];
    if([self.selectedSection isEqual:@"status"])[self renderStatus];
    else if([self.selectedSection isEqual:@"activity"])[self renderActivity];
    else if([self.selectedSection isEqual:@"history"])[self renderHistory];
    else Add(self.body,Label([self.selectedSection isEqual:@"folders"]?W(@"Loading folders…",@"폴더를 불러오고 있습니다…"):W(@"Loading settings…",@"설정을 불러오고 있습니다…"),16,NSFontWeightRegular));
    JasoApplyContentZoom(self.body);[self updateAdaptiveRows];[self.pageHost layoutSubtreeIfNeeded];
    NSValue *position=self.scrollPositions[self.selectedSection];if(position){[self.scroll.contentView scrollToPoint:position.pointValue];[self.scroll reflectScrolledClipView:self.scroll.contentView];}
    NSView *focus=self.focusPositions[self.selectedSection];if(focus.window==self.window)[self.window makeFirstResponder:focus];
}
- (void)addActionRow:(NSArray<NSView *> *)views spacing:(CGFloat)spacing {
    NSStackView *row=Stack(views,NO,spacing);
    // Let the window reach the requested width before the row reflows.
    // Keep every action present while its previous horizontal layout yields.
    [row setClippingResistancePriority:NSLayoutPriorityDragThatCannotResizeWindow forOrientation:NSLayoutConstraintOrientationHorizontal];
    for(NSView *view in views)[row setVisibilityPriority:NSStackViewVisibilityPriorityMustHold forView:view];
    [self.adaptiveRows addObject:row];Add(self.body,row);
}
- (void)updateAdaptiveRows {
    // Reserve the viewport's actual scroller gutter before laying out a row.
    // The clip view can still have the previous frame during window resizing.
    NSSize frame=NSMakeSize(MAX(0,self.window.contentView.bounds.size.width-(self.activityOnly?0:152)),self.window.contentView.bounds.size.height);
    NSSize viewport=[NSScrollView contentSizeForFrameSize:frame horizontalScrollerClass:nil verticalScrollerClass:self.scroll.hasVerticalScroller?self.scroll.verticalScroller.class:nil borderType:self.scroll.borderType controlSize:self.scroll.verticalScroller.controlSize scrollerStyle:self.scroll.scrollerStyle];
    CGFloat available=viewport.width-self.body.edgeInsets.left-self.body.edgeInsets.right;
    for(NSStackView *row in self.adaptiveRows){
        CGFloat needed=row.spacing*MAX(0,(NSInteger)row.arrangedSubviews.count-1);
        for(NSView *view in row.arrangedSubviews){
            CGFloat width=view.fittingSize.width;
            if([view isKindOfClass:NSControl.class])width=MAX(width,((NSControl *)view).cell.cellSize.width);
            needed+=width;
        }
        BOOL vertical=needed>available;
        row.orientation=vertical?NSUserInterfaceLayoutOrientationVertical:NSUserInterfaceLayoutOrientationHorizontal;
        row.alignment=vertical?NSLayoutAttributeLeading:NSLayoutAttributeCenterY;
    }
}
- (void)renderStatus {
    NSDictionary *presentation=JasoWorkspaceStatus(self.snapshot,self.snapshotError,JasoUsesKorean());
    Add(self.body,Label(W(@"Status",@"상태"),12,NSFontWeightMedium));
    self.heroTitle=Label(presentation[@"title"],27,NSFontWeightSemibold);self.heroTitle.identifier=@"workspace-status-title";Add(self.body,self.heroTitle);
    self.heroDescription=Label(presentation[@"subtitle"],13,NSFontWeightRegular);self.heroDescription.textColor=NSColor.secondaryLabelColor;Add(self.body,self.heroDescription);
    NSMutableArray *buttons=[NSMutableArray arrayWithObject:[self button:W(@"View activity",@"활동 보기") action:@selector(showActivity:) identifier:@"status-activity"]];
    if(S(presentation[@"primaryAction"]).length)[buttons addObject:[self button:presentation[@"primaryTitle"] action:@selector(primary:) identifier:presentation[@"primaryAction"]]];
    [buttons addObject:[self button:W(@"Refresh",@"새로고침") action:@selector(refreshActivity:) identifier:@"status-refresh"]];
    [self addActionRow:buttons spacing:10];
    NSDictionary *live=D(self.activity[@"activity"]);
    if([N(self.activity[@"available"]) boolValue]&&S(live[@"scope_path"]).length){
        NSStackView *location=Stack(@[Label(W(@"Current folder",@"현재 확인하는 폴더"),12,NSFontWeightMedium),Label([S(live[@"scope_path"]) lastPathComponent],18,NSFontWeightSemibold),Label(S(live[@"scope_path"]),12,NSFontWeightRegular)],YES,8);Add(self.body,Card(location));
    }
    NSStackView *stats=Stack(@[],YES,10);
    Add(stats,Label([NSString stringWithFormat:W(@"Renamed today: %@",@"오늘 이름을 정리한 파일: %@"),Count(self.history[@"today_count"]?:self.snapshot[@"today_renamed"])],19,NSFontWeightMedium));
    [stats addArrangedSubview:[self button:W(@"View history",@"변경 기록 보기") action:@selector(showHistory:) identifier:@"status-history"]];
    Add(self.body,Card(stats));
    NSMutableArray *actionIssues=NSMutableArray.new;
    for(NSDictionary *issue in A(presentation[@"issues"]))if([N(issue[@"requiresAction"]) boolValue])[actionIssues addObject:issue];
    if(actionIssues.count){
        Add(self.body,Label(W(@"Needs attention",@"확인이 필요한 항목"),16,NSFontWeightSemibold));
        for(NSDictionary *issue in [actionIssues subarrayWithRange:NSMakeRange(0,MIN(3,actionIssues.count))]){
            WorkspaceButton *button=[WorkspaceButton buttonWithTitle:[NSString stringWithFormat:@"%@  ›",S(issue[@"path"]).lastPathComponent.length?S(issue[@"path"]).lastPathComponent:S(issue[@"title"])] target:self action:@selector(showIssue:)];button.record=issue;button.alignment=NSTextAlignmentLeft;button.lineBreakMode=NSLineBreakByTruncatingMiddle;button.toolTip=S(issue[@"path"]);
            [button setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];Add(self.body,button);
        }
    }
    NSArray *records=A(self.history[@"items"]);if(records.count){
        Add(self.body,Label(W(@"Recent changes",@"최근 변경"),16,NSFontWeightSemibold));
        for(NSDictionary *record in [records subarrayWithRange:NSMakeRange(0,MIN(3,records.count))])[self addHistoryRow:record];
    }
    if(S(presentation[@"automaticRetrySummary"]).length){NSTextField *note=Label(presentation[@"automaticRetrySummary"],11,NSFontWeightRegular);note.textColor=NSColor.secondaryLabelColor;Add(self.body,note);}
    [self.body addArrangedSubview:[self button:W(@"Manage folders",@"폴더 관리") action:@selector(showFolders:) identifier:@"status-folders"]];
}
- (NSString *)phaseDescription:(NSString *)phase {
    return S(JasoActivityPresentation(@{@"phase":phase?:@""},JasoUsesKorean())[@"title"]);
}
- (void)renderActivity {
    NSMutableArray *headerViews=[NSMutableArray arrayWithObject:Label(W(@"Activity",@"활동"),24,NSFontWeightSemibold)];
    if(!self.activityOnly)[headerViews addObject:[self button:W(@"Open in separate window",@"별도 창으로 보기") action:@selector(separateActivity:) identifier:@"activity-window"]];
    [headerViews addObject:[self button:W(@"Refresh",@"새로고침") action:@selector(refreshActivity:) identifier:@"activity-refresh"]];
    [self addActionRow:headerViews spacing:12];
    self.activityPhase=Label(@"",15,NSFontWeightMedium);self.activityLocation=Label(@"",13,NSFontWeightRegular);self.activityLocation.identifier=@"activity-location";
    self.activityProgress=NSProgressIndicator.new;self.activityProgress.translatesAutoresizingMaskIntoConstraints=NO;self.activityProgress.style=NSProgressIndicatorStyleBar;self.activityProgress.identifier=@"activity-progress";
    self.activityNumbers=Label(@"",12,NSFontWeightRegular);self.activityTime=Label(@"",11,NSFontWeightRegular);self.activityTime.textColor=NSColor.secondaryLabelColor;
    NSStackView *current=Stack(@[],YES,10);for(NSView *view in @[self.activityPhase,self.activityLocation,self.activityProgress,self.activityNumbers,self.activityTime])Add(current,view);Add(self.body,Card(current));
    Add(self.body,Label(W(@"Recent activity",@"최근 활동"),14,NSFontWeightSemibold));
    NSPopUpButton *filter=[[NSPopUpButton alloc] initWithFrame:NSZeroRect pullsDown:NO];filter.autoenablesItems=NO;filter.target=self;filter.action=@selector(filterActivity:);filter.identifier=@"activity-filter";filter.accessibilityLabel=W(@"Activity filter",@"활동 필터");
    NSArray *keys=@[@"all",@"rename",@"wait"];NSArray *labels=@[W(@"All",@"전체"),W(@"Renames",@"이름 변경"),W(@"Issues",@"오류·대기")];
    for(NSUInteger i=0;i<keys.count;i++){[filter addItemWithTitle:labels[i]];filter.lastItem.representedObject=keys[i];if([keys[i] isEqual:self.activityFilter])[filter selectItem:filter.lastItem];}
    NSButton *follow=[NSButton checkboxWithTitle:W(@"Follow new activity",@"새 활동 따라가기") target:self action:@selector(toggleFollow:)];follow.state=self.followActivity;self.followButton=follow;
    [self addActionRow:@[filter,follow] spacing:16];
    self.activityTable=NSTableView.new;self.activityTable.headerView=nil;self.activityTable.delegate=self;self.activityTable.dataSource=self;self.activityTable.rowHeight=48*self.contentZoom;self.activityTable.usesAlternatingRowBackgroundColors=YES;self.activityTable.target=self;self.activityTable.doubleAction=@selector(showActivityItem:);
    NSTableColumn *column=[[NSTableColumn alloc] initWithIdentifier:@"activity"];column.resizingMask=NSTableColumnAutoresizingMask;[self.activityTable addTableColumn:column];self.activityTable.columnAutoresizingStyle=NSTableViewUniformColumnAutoresizingStyle;
    NSScrollView *feed=NSScrollView.new;feed.translatesAutoresizingMaskIntoConstraints=NO;feed.hasVerticalScroller=YES;feed.documentView=self.activityTable;[feed.heightAnchor constraintEqualToConstant:280].active=YES;Add(self.body,feed);
    self.activityIssues=Stack(@[],YES,10);Add(self.body,self.activityIssues);
    self.activityRetention=Label(@"",11,NSFontWeightRegular);self.activityRetention.identifier=@"activity-retention";Add(self.body,self.activityRetention);
    [self refreshActivityFields];
}
- (void)refreshActivityIssues {
    for(NSView *view in self.activityIssues.arrangedSubviews.copy){[self.activityIssues removeArrangedSubview:view];[view removeFromSuperview];}
    NSArray *issues=A(JasoWorkspaceStatus(self.snapshot,self.snapshotError,JasoUsesKorean())[@"issues"]);
    if(issues.count)Add(self.activityIssues,Label(W(@"Currently waiting",@"현재 대기 항목"),14,NSFontWeightSemibold));
    for(NSDictionary *issue in issues){WorkspaceButton *button=[WorkspaceButton buttonWithTitle:[NSString stringWithFormat:@"%@ — %@",S(issue[@"path"]).lastPathComponent,S(issue[@"title"])] target:self action:@selector(showIssue:)];button.record=issue;button.alignment=NSTextAlignmentLeft;button.lineBreakMode=NSLineBreakByTruncatingMiddle;button.toolTip=S(issue[@"path"]);[button setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];Add(self.activityIssues,button);}
    JasoApplyContentZoom(self.activityIssues);
}
- (void)refreshActivityFields {
    [self refreshActivityIssues];
    if(!self.activityLocation)return;
    BOOL available=[N(self.activity[@"available"]) boolValue]&&!self.activityError.length;
    NSDictionary *live=available?D(self.activity[@"activity"]):@{};NSString *state=S(live[@"state"]),*phase=S(live[@"phase"]);
    // An inactive worker state is authoritative even if its last phase remains.
    NSString *headingPhase=[@[@"idle",@"paused",@"stopping",@"stopped"] containsObject:state]?state:phase;
    self.activityPhase.stringValue=[self phaseDescription:available?headingPhase:@""];
    if([state isEqual:@"blocked"]&&[D(live[@"issue"])[@"code"] isEqual:@"low_storage"])self.activityPhase.stringValue=W(@"Cleanup is waiting for disk space",@"디스크 공간 확보를 기다리고 있습니다");
    NSString *location=S(live[@"item_path"]);if(!location.length)location=S(live[@"scope_path"]);
    self.activityLocation.stringValue=available?(location.length?location:W(@"No folder is being processed right now.",@"현재 처리 중인 폴더가 없습니다.")):W(@"Live activity is unavailable. Refresh to check the worker connection.",@"현재 작업 정보를 받지 못했습니다. 새로고침으로 연결 상태를 확인하세요.");
    NSDictionary *scope=D(live[@"scope"]),*counters=D(live[@"counters"]);NSNumber *total=N(scope[@"total"]),*done=N(scope[@"processed"]);
    self.activityProgress.indeterminate=!total;self.activityProgress.hidden=!available||[state isEqual:@"blocked"]||[@[@"idle",@"paused",@"stopped"] containsObject:state];
    if(total){self.activityProgress.minValue=0;self.activityProgress.maxValue=MAX(1,total.doubleValue);self.activityProgress.doubleValue=MIN(done.doubleValue,total.doubleValue);}
    if(self.activityProgress.hidden)[self.activityProgress stopAnimation:nil];else [self.activityProgress startAnimation:nil];
    self.activityNumbers.stringValue=[NSString stringWithFormat:W(@"This session: %@ checks completed · %@ names changed",@"이번 실행: 검사 완료 %@건 · 이름 변경 %@건"),Count(counters[@"processed"]),Count(counters[@"renamed"])];
    if(available&&[N(live[@"phase_elapsed_seconds"]) doubleValue]>=10&&[state isEqual:@"waiting_metadata"]&&[@[@"opening_directory",@"reading_metadata",@"enumerating"] containsObject:phase])self.activityPhase.stringValue=W(@"Waiting for this folder's metadata",@"폴더 정보 응답을 기다리고 있습니다");
    self.activityTime.stringValue=[NSString stringWithFormat:W(@"Last progress: %@",@"마지막 진행: %@"),Time(live[@"last_progress_at"])];
    NSMutableArray *rows=NSMutableArray.new;for(id value in A(live[@"events"])){
        NSDictionary *row=D(value);if(!row.count)continue;
        NSString *kind=S(row[@"kind"]);
        if([self.activityFilter isEqual:@"rename"]&&![kind containsString:@"renam"])continue;
        if([self.activityFilter isEqual:@"wait"]){
            NSDictionary *presentation=JasoActivityPresentation(row,JasoUsesKorean());
            if(![presentation[@"historicalIssue"] boolValue]||[presentation[@"resolved"] boolValue])continue;
        }
        [rows addObject:row];
    }
    NSPoint origin=self.activityTable.enclosingScrollView.contentView.bounds.origin;NSInteger selected=-1;
    if(![self.selectedActivitySession isEqual:S(live[@"session_id"])])self.selectedActivitySequence=nil;
    if(self.selectedActivitySequence){for(NSUInteger i=0;i<rows.count;i++)if([rows[i][@"sequence"] isEqual:self.selectedActivitySequence]){selected=i;break;}if(selected<0)self.selectedActivitySequence=nil;}
    self.activityRetention.stringValue=[N(live[@"dropped_events"]) unsignedLongLongValue]>0?[NSString stringWithFormat:W(@"%@ older activity entries have left this view. Completed name changes remain in History.",@"이전 활동 %@건은 이 목록에서 생략되었습니다. 완료된 이름 변경은 변경 기록에서 확인할 수 있습니다."),Count(live[@"dropped_events"])]:W(@"Recent activity appears here. Completed name changes are saved in History.",@"최근 활동을 표시합니다. 완료된 이름 변경은 변경 기록에 저장됩니다.");
    self.restoringActivitySelection=YES;self.activityRows=rows;[self.activityTable reloadData];if(selected>=0&&selected<(NSInteger)rows.count)[self.activityTable selectRowIndexes:[NSIndexSet indexSetWithIndex:selected] byExtendingSelection:NO];if(selected<0)[self.activityTable deselectAll:nil];self.restoringActivitySelection=NO;
    if(self.followActivity&&rows.count)[self.activityTable scrollRowToVisible:rows.count-1];else[self.activityTable.enclosingScrollView.contentView scrollToPoint:origin];
}
- (NSInteger)numberOfRowsInTableView:(NSTableView *)tableView { return self.activityRows.count; }
- (NSView *)tableView:(NSTableView *)tableView viewForTableColumn:(NSTableColumn *)column row:(NSInteger)row {
    NSDictionary *event=D(self.activityRows[row]),*presentation=JasoActivityPresentation(event,JasoUsesKorean());
    id time=[presentation[@"resolved"] boolValue]?presentation[@"resolvedAt"]:(event[@"timestamp"]?:event[@"at"]);
    NSTextField *label=Label([NSString stringWithFormat:@"%@  %@\n%@",Time(time),S(presentation[@"title"]),S(event[@"item_path"]?:event[@"path"]?:event[@"scope_path"])],11*self.contentZoom,NSFontWeightRegular);label.lineBreakMode=NSLineBreakByTruncatingMiddle;label.maximumNumberOfLines=2;label.toolTip=label.stringValue;return label;
}
- (void)tableViewSelectionDidChange:(NSNotification *)notification { if(!self.restoringActivitySelection){self.followActivity=NO;self.followButton.state=NSControlStateValueOff;NSInteger row=self.activityTable.selectedRow;self.selectedActivitySequence=row>=0&&row<(NSInteger)self.activityRows.count?N(self.activityRows[row][@"sequence"]):nil;self.selectedActivitySession=S(D(self.activity[@"activity"])[@"session_id"]);} }
- (void)filterActivity:(NSPopUpButton *)sender { self.activityFilter=sender.selectedItem.representedObject;[self refreshActivityFields]; }
- (void)toggleFollow:(NSButton *)sender { self.followActivity=sender.state==NSControlStateValueOn;[self refreshActivityFields]; }
- (void)renderHistory {
    Add(self.body,Label(W(@"History",@"변경 기록"),24,NSFontWeightSemibold));
    NSSearchField *search=NSSearchField.new;search.translatesAutoresizingMaskIntoConstraints=NO;search.placeholderString=W(@"Search file names and folders",@"파일 이름이나 폴더 검색");search.stringValue=self.historySearch;search.delegate=self;search.identifier=@"history-search";search.accessibilityLabel=search.placeholderString;Add(self.body,search);
    NSPopUpButton *result=[[NSPopUpButton alloc] initWithFrame:NSZeroRect pullsDown:NO];result.autoenablesItems=NO;result.target=self;result.action=@selector(historyFilter:);result.accessibilityLabel=W(@"Change result",@"변경 결과");
    NSArray *keys=@[@"",@"renamed",@"restored"];NSArray *names=@[W(@"All results",@"모든 결과"),W(@"Renamed",@"이름 변경"),W(@"Restored",@"되돌림")];
    for(NSUInteger i=0;i<keys.count;i++){[result addItemWithTitle:names[i]];result.lastItem.representedObject=keys[i];if([keys[i] isEqual:self.historyResult?:@""])[result selectItem:result.lastItem];}
    self.historyDateToggle=[NSButton checkboxWithTitle:W(@"Date",@"날짜") target:self action:@selector(historyDateChanged:)];self.historyDateToggle.state=self.historyDate.length>0;
    self.historyDatePicker=NSDatePicker.new;self.historyDatePicker.datePickerElements=NSDatePickerElementFlagYearMonthDay;self.historyDatePicker.datePickerStyle=NSDatePickerStyleTextFieldAndStepper;self.historyDatePicker.target=self;self.historyDatePicker.action=@selector(historyDateChanged:);self.historyDatePicker.enabled=self.historyDate.length>0;self.historyDatePicker.dateValue=NSDate.date;
    if(self.historyDate.length){NSDateFormatter *date=NSDateFormatter.new;date.dateFormat=@"yyyy-MM-dd";self.historyDatePicker.dateValue=[date dateFromString:self.historyDate]?:NSDate.date;}
    [self addActionRow:@[result,self.historyDateToggle,self.historyDatePicker,[self button:W(@"Refresh",@"새로고침") action:@selector(historyRefresh:) identifier:@"history-refresh"]] spacing:12];
    if(self.historyError.length)Add(self.body,Label(self.historyError,12,NSFontWeightRegular));
    NSArray *records=A(self.history[@"items"]);
    if(!records.count){NSTextField *empty=Label(self.history?W(@"No name changes match this search.",@"표시할 이름 변경 기록이 없습니다."):W(@"Loading history…",@"변경 기록을 불러오고 있습니다…"),14,NSFontWeightRegular);empty.identifier=@"history-empty";Add(self.body,empty);}
    for(NSDictionary *record in records)[self addHistoryRow:record];
    NSButton *previous=[self button:W(@"Previous",@"이전") action:@selector(historyPrevious:) identifier:@"history-previous"];previous.enabled=self.historyOffset>0;
    NSButton *next=[self button:W(@"Next",@"다음") action:@selector(historyNext:) identifier:@"history-next"];next.enabled=N(self.history[@"total"]).unsignedIntegerValue>self.historyOffset+records.count;
    [self addActionRow:@[previous,Label([NSString stringWithFormat:W(@"%@ changes",@"변경 %@건"),Count(self.history[@"total"])],12,NSFontWeightRegular),next] spacing:12];
}
- (void)addHistoryRow:(NSDictionary *)record {
    NSDictionary *comparison=JasoNamePresentation(record);NSStackView *row=Stack(@[],YES,6);
    for(NSArray *side in @[@[W(@"Before",@"변경 전"),comparison[@"before"],@"before"],@[W(@"After",@"변경 후"),comparison[@"after"],@"after"]]){
        WorkspaceButton *button=[WorkspaceButton buttonWithTitle:[NSString stringWithFormat:@"%@: %@",side[0],side[1]] target:self action:@selector(showHistoryItem:)];button.record=record;button.identifier=JasoHistoryRowIdentifier(record,side[2]);button.alignment=NSTextAlignmentLeft;button.lineBreakMode=NSLineBreakByTruncatingMiddle;button.toolTip=S(record[@"new_path"]);
        [button setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];Add(row,button);
    }
    NSDictionary *names=@{@"restored":W(@"Restored",@"되돌림 완료"),@"reverted":W(@"Restored",@"되돌림 완료"),@"renamed":W(@"Renamed",@"이름 변경 완료"),@"failed":W(@"Could not rename",@"이름 변경 실패"),@"recovery_required":W(@"Recovery needed",@"복구 확인 필요")};NSString *result=names[S(record[@"result"]) ]?:W(@"Result unavailable",@"결과 확인 필요");
    Add(row,Label([NSString stringWithFormat:@"%@ · %@\n%@",Time(record[@"timestamp"]),result,S(record[@"new_path"]).stringByDeletingLastPathComponent],11,NSFontWeightRegular));Add(self.body,Card(row));
}
- (void)controlTextDidEndEditing:(NSNotification *)notification {
    if(self.restoringZoomState)return;
    if(![((NSView *)notification.object).identifier isEqual:@"history-search"])return;
    NSString *value=[notification.object stringValue];if([self.historySearch isEqual:value])return;self.historySearch=value;self.historyOffset=0;[self loadHistory];
}
- (void)historyFilter:(NSPopUpButton *)sender { self.historyResult=sender.selectedItem.representedObject;self.historyOffset=0;[self loadHistory]; }
- (void)historyDateChanged:(id)sender { self.historyDatePicker.enabled=self.historyDateToggle.state==NSControlStateValueOn;NSDateFormatter *date=NSDateFormatter.new;date.dateFormat=@"yyyy-MM-dd";self.historyDate=self.historyDatePicker.enabled?[date stringFromDate:self.historyDatePicker.dateValue]:nil;self.historyOffset=0;[self loadHistory]; }
- (void)historyRefresh:(id)sender { [self loadHistory]; }
- (void)refreshActivity:(id)sender { [self loadActivity];if(self.refreshHandler)self.refreshHandler(); }
- (void)showActivityItem:(id)sender {
    NSInteger row=self.activityTable.clickedRow;if(row<0||row>=(NSInteger)self.activityRows.count)return;
    NSDictionary *event=D(self.activityRows[row]),*presentation=JasoActivityPresentation(event,JasoUsesKorean());
    NSString *path=S(presentation[@"path"]);BOOL safePath=[presentation[@"pathActionAllowed"] boolValue];
    [self openDetail:S(presentation[@"title"])];Add(self.detailBody,Label(path,13,NSFontWeightRegular));
    Add(self.detailBody,Label([NSString stringWithFormat:W(@"Recorded at: %@",@"기록 시각: %@"),Time(event[@"at"])],12,NSFontWeightRegular));
    Add(self.detailBody,Label(S(presentation[@"detail"]),13,NSFontWeightRegular));
    // Current retry samples are partial. They can add current context for an
    // exact full path, but can neither rewrite nor resolve an older observation.
    NSDictionary *match=nil;
    if(safePath)for(NSDictionary *issue in A(JasoWorkspaceStatus(self.snapshot,self.snapshotError,JasoUsesKorean())[@"issues"]))
        if([[S(issue[@"path"]) dataUsingEncoding:NSUTF8StringEncoding] isEqual:[path dataUsingEncoding:NSUTF8StringEncoding]]){match=issue;break;}
    if(match){
        Add(self.detailBody,Label(W(@"Current status",@"현재 상태"),14,NSFontWeightSemibold));
        Add(self.detailBody,Label([NSString stringWithFormat:@"%@\n%@",S(match[@"title"]),S(match[@"detail"])],13,NSFontWeightRegular));
    }
    if(safePath){
        WorkspaceButton *reveal=[WorkspaceButton buttonWithTitle:W(@"Show in Finder",@"Finder에서 위치 보기") target:self action:@selector(revealRecord:)];reveal.record=@{@"path":path};reveal.identifier=@"activity-reveal";[self.detailBody addArrangedSubview:reveal];
    }
    [self.detailBody addArrangedSubview:[self button:W(@"Manage folders",@"폴더 관리") action:@selector(showFolders:) identifier:@"activity-manage-folder"]];JasoApplyContentZoom(self.detailBody);
}
- (void)historyPrevious:(id)sender { self.historyOffset=self.historyOffset>=50?self.historyOffset-50:0;[self loadHistory]; }
- (void)historyNext:(id)sender { self.historyOffset+=50;[self loadHistory]; }
- (void)request:(NSString *)name parameters:(NSDictionary *)parameters reply:(JasoWorkspaceReply)reply {
    if(!self.requestHandler||self.closed||[self.pendingRequests containsObject:name])return;
    [self.pendingRequests addObject:name];__weak typeof(self) weakSelf=self;
    self.requestHandler(name,parameters,^(NSDictionary *result,NSString *error){dispatch_async(dispatch_get_main_queue(),^{typeof(self) strongSelf=weakSelf;if(!strongSelf)return;[strongSelf.pendingRequests removeObject:name];if(!strongSelf.closed)reply(result,error);});});
}
- (void)loadActivity { __weak typeof(self) weakSelf=self;[self request:@"activity" parameters:@{} reply:^(NSDictionary *value,NSString *error){[weakSelf updateActivity:value error:error];}]; }
- (void)loadHistory {
    NSMutableDictionary *query=[@{@"search":self.historySearch,@"offset":@(self.historyOffset),@"limit":@50} mutableCopy];
    if(self.historyDate.length)query[@"date"]=self.historyDate;if(self.historyResult.length)query[@"result"]=self.historyResult;
    __weak typeof(self) weakSelf=self;
    [self request:@"history" parameters:query reply:^(NSDictionary *result,NSString *error){
        if(![weakSelf.historySearch isEqual:query[@"search"]]||weakSelf.historyOffset!=[query[@"offset"] unsignedIntegerValue]||![weakSelf.historyDate?:@"" isEqual:query[@"date"]?:@""]||![weakSelf.historyResult?:@"" isEqual:query[@"result"]?:@""]){[weakSelf loadHistory];return;}[weakSelf updateHistory:result error:error];
    }];
}
- (void)updateSnapshot:(NSDictionary *)snapshot error:(NSString *)error updatedAt:(NSDate *)date {
    self.snapshot=snapshot;self.snapshotError=error;self.snapshotDate=date;
    if([self.selectedSection isEqual:@"status"]){if(self.scroll)self.scrollPositions[@"status"]=[NSValue valueWithPoint:self.scroll.contentView.bounds.origin];[self render];}
    if([self.selectedSection isEqual:@"activity"])[self refreshActivityIssues];
    [self.activityWindow updateSnapshot:snapshot error:error updatedAt:date];
}
- (void)updateActivity:(NSDictionary *)response error:(NSString *)error { self.activity=response;self.activityError=error;[self refreshActivityFields]; }
- (void)updateHistory:(NSDictionary *)response error:(NSString *)error {
    self.history=response;self.historyError=error;if([@[@"history",@"status"] containsObject:self.selectedSection]){if(self.scroll)self.scrollPositions[self.selectedSection]=[NSValue valueWithPoint:self.scroll.contentView.bounds.origin];[self render];}
}
- (void)setRefreshing:(BOOL)refreshing { self.busy=refreshing; }
- (void)primary:(NSButton *)sender { if(!self.busy&&self.actionHandler)self.actionHandler(sender.identifier); }
- (void)showActivity:(id)sender { [self selectSection:@"activity"]; }
- (void)showHistory:(id)sender { [self selectSection:@"history"]; }
- (void)showFolders:(id)sender { [self selectSection:@"folders"]; }
- (void)showStorage:(id)sender {
    [self openDetail:W(@"Storage",@"저장공간")];
    NSTextField *loading=Label(W(@"Measuring Jaso data…",@"Jaso 데이터 용량을 확인하고 있습니다…"),13,NSFontWeightRegular);Add(self.detailBody,loading);
    NSPanel *panel=self.detailPanel;__weak typeof(self) weakSelf=self;
    [self request:@"storage" parameters:@{} reply:^(NSDictionary *result,NSString *error){
        if(weakSelf.detailPanel!=panel)return;
        loading.stringValue=error?:W(@"Storage used by Jaso, grouped by purpose.",@"Jaso가 사용하는 공간을 항목별로 확인하세요.");
        if(error)return;
        NSNumber *size=N(result[@"allocated_bytes"]);Add(weakSelf.detailBody,Label([NSString stringWithFormat:W(@"Jaso data: %@",@"Jaso 데이터: %@"),size?[NSByteCountFormatter stringFromByteCount:size.longLongValue countStyle:NSByteCountFormatterCountStyleFile]:W(@"Unavailable",@"확인할 수 없음")],18,NSFontWeightSemibold));
        NSDictionary *categoryNames=@{@"index":W(@"File index",@"파일 목록"),@"history":W(@"History search data",@"변경 기록 검색 데이터"),@"logs":W(@"Change records and diagnostics",@"변경 기록 및 진단 로그"),@"backups":W(@"Settings backups",@"설정 백업"),@"releases":W(@"Saved app versions",@"보관 중인 앱 버전"),@"other":W(@"Other app data",@"기타 앱 데이터")};
        for(NSDictionary *category in A(result[@"categories"])) {
            NSNumber *bytes=N(category[@"allocated_bytes"]);
            Add(weakSelf.detailBody,Label([NSString stringWithFormat:@"%@: %@",categoryNames[S(category[@"role"])]?:S(category[@"role"]),bytes?[NSByteCountFormatter stringFromByteCount:bytes.longLongValue countStyle:NSByteCountFormatterCountStyleFile]:W(@"Unavailable",@"확인할 수 없음")],13,NSFontWeightRegular));
        }
        for(NSDictionary *volume in A(result[@"volumes"])) {
            NSNumber *free=N(volume[@"available_bytes"]);NSString *paths=[A(volume[@"paths"]) componentsJoinedByString:@"\n"];
            Add(weakSelf.detailBody,Label([NSString stringWithFormat:W(@"%@\nAvailable space: %@",@"%@\n사용 가능한 공간: %@"),paths,free?[NSByteCountFormatter stringFromByteCount:free.longLongValue countStyle:NSByteCountFormatterCountStyleFile]:W(@"Unavailable",@"확인할 수 없음")],13,NSFontWeightRegular));
        }
        NSString *status=S(result[@"status"]);if([@[@"warning",@"critical"] containsObject:status])Add(weakSelf.detailBody,Label([status isEqual:@"critical"]?W(@"Free up disk space to continue cleanup. Your change history is kept for recovery.",@"디스크 공간을 확보하면 정리를 이어갈 수 있습니다. 변경 기록은 복구를 위해 보관됩니다."):W(@"Disk space is running low. Review storage in System Settings.",@"디스크 여유 공간이 적습니다. 시스템 설정에서 저장공간을 확인하세요."),13,NSFontWeightMedium));
        [weakSelf.detailBody addArrangedSubview:[weakSelf button:W(@"Open storage settings",@"저장공간 설정 열기") action:@selector(openStorageSettings:) identifier:@"storage-settings"]];
        JasoApplyContentZoom(weakSelf.detailBody);
    }];
}
- (void)openStorageSettings:(id)sender { if(self.actionHandler)self.actionHandler(@"storage-settings"); }
- (void)showIssue:(WorkspaceButton *)sender {
    [self openDetail:S(sender.record[@"title"])];Add(self.detailBody,Label(S(sender.record[@"path"]),12,NSFontWeightRegular));Add(self.detailBody,Label(S(sender.record[@"detail"]),13,NSFontWeightRegular));
    WorkspaceButton *reveal=[WorkspaceButton buttonWithTitle:W(@"Show in Finder",@"Finder에서 위치 보기") target:self action:@selector(revealRecord:)];reveal.record=@{@"path":S(sender.record[@"path"])};[self.detailBody addArrangedSubview:reveal];
    [self.detailBody addArrangedSubview:[self button:W(@"Manage folders",@"폴더 관리") action:@selector(showFolders:) identifier:@"issue-folders"]];
    JasoApplyContentZoom(self.detailBody);
}
- (void)openDetail:(NSString *)title {
    if(self.detailPanel)[self closeDetail:nil];self.detailReturnFocus=[self.window.firstResponder isKindOfClass:NSView.class]?(NSView *)self.window.firstResponder:nil;
    self.detailPanel=[[NSPanel alloc] initWithContentRect:NSMakeRect(0,0,580,450) styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];self.detailPanel.title=title;
    NSScrollView *scroll=NSScrollView.new;scroll.contentView=WorkspaceClip.new;scroll.translatesAutoresizingMaskIntoConstraints=NO;scroll.hasVerticalScroller=YES;
    self.detailBody=Stack(@[],YES,16);self.detailBody.edgeInsets=NSEdgeInsetsMake(24,24,24,24);scroll.documentView=self.detailBody;[self.detailPanel.contentView addSubview:scroll];Pin(scroll,self.detailPanel.contentView,0);
    [self.detailBody.widthAnchor constraintEqualToAnchor:scroll.contentView.widthAnchor].active=YES;
    [self.detailBody.leadingAnchor constraintEqualToAnchor:scroll.contentView.leadingAnchor].active=YES;[self.detailBody.topAnchor constraintEqualToAnchor:scroll.contentView.topAnchor].active=YES;
    NSButton *close=[self button:W(@"Close",@"닫기") action:@selector(closeDetail:) identifier:@"detail-close"];close.keyEquivalent=@"\e";[self.detailBody addArrangedSubview:close];Add(self.detailBody,Label(title,21,NSFontWeightSemibold));
    [self.window beginSheet:self.detailPanel completionHandler:nil];
}
- (void)closeDetail:(id)sender { if(self.detailPanel){[self.window endSheet:self.detailPanel];[self.detailPanel orderOut:nil];self.detailPanel=nil;self.detailBody=nil;}if(self.detailReturnFocus.window==self.window)[self.window makeFirstResponder:self.detailReturnFocus];self.detailReturnFocus=nil; }
- (void)showHistoryItem:(WorkspaceButton *)sender {
    self.selectedRecord=sender.record;[self openDetail:W(@"Rename details",@"이름 변경 상세")];
    NSDictionary *comparison=JasoNamePresentation(sender.record);
    if([comparison[@"separated"] boolValue])Add(self.detailBody,Label(W(@"Compare separated letters and combined syllables.",@"분리된 글자와 합쳐진 글자를 비교합니다."),13,NSFontWeightRegular));
    for(NSArray *side in @[@[W(@"Before",@"변경 전"),comparison[@"before"],@"before",@"old_name",W(@"Copy original name",@"원래 이름 복사")],@[W(@"After",@"변경 후"),comparison[@"after"],@"after",@"new_name",W(@"Copy changed name",@"변경한 이름 복사")]]){
        Add(self.detailBody,Label(side[0],11,NSFontWeightMedium));NSTextField *name=Label(side[1],14,NSFontWeightRegular);name.identifier=[NSString stringWithFormat:@"history-%@-name",side[2]];name.selectable=NO;Add(self.detailBody,name);
        WorkspaceButton *copy=[WorkspaceButton buttonWithTitle:side[4] target:self action:@selector(copyHistoryName:)];copy.record=@{@"name":S(sender.record[side[3]])};copy.identifier=[NSString stringWithFormat:@"history-copy-%@",side[2]];[self.detailBody addArrangedSubview:copy];
    }
    for(NSArray *pair in @[@[W(@"Location",@"위치"),S(sender.record[@"new_path"])],@[W(@"Completed",@"완료 시각"),Time(sender.record[@"timestamp"])]]){Add(self.detailBody,Label(pair[0],11,NSFontWeightMedium));Add(self.detailBody,Label(pair[1],14,NSFontWeightRegular));}
    if([S(sender.record[@"result"]) isEqual:@"renamed"]&&![N(sender.record[@"restored"]) boolValue]){
        if(N(self.snapshot[@"running"])&&![self.snapshot[@"running"] boolValue]){Add(self.detailBody,Label(W(@"Start cleanup to restore this original name.",@"원래 이름으로 되돌리려면 작업을 시작하세요."),13,NSFontWeightRegular));[self.detailBody addArrangedSubview:[self button:W(@"Start cleanup",@"작업 시작") action:@selector(startForRestore:) identifier:@"history-start"]];}
        else { NSButton *restore=[self button:W(@"Restore original name",@"원래 이름으로 되돌리기") action:@selector(reviewRestore:) identifier:@"history-restore"];restore.enabled=self.restoreRequest==nil;[self.detailBody addArrangedSubview:restore];if(self.restoreRequest)Add(self.detailBody,Label(W(@"Another restore is being processed. Check its result before restoring this item.",@"다른 항목의 되돌리기를 처리하고 있습니다. 결과를 확인한 뒤 이 항목을 되돌릴 수 있습니다."),13,NSFontWeightRegular)); }
    }
    WorkspaceButton *reveal=[WorkspaceButton buttonWithTitle:W(@"Show in Finder",@"Finder에서 위치 보기") target:self action:@selector(revealRecord:)];reveal.record=@{@"path":S(sender.record[@"new_path"])};reveal.identifier=@"history-reveal";[self.detailBody addArrangedSubview:reveal];
    JasoApplyContentZoom(self.detailBody);
}
- (void)copyHistoryName:(WorkspaceButton *)sender {
    NSPasteboard *pasteboard=NSPasteboard.generalPasteboard;[pasteboard clearContents];[pasteboard setString:S(sender.record[@"name"]) forType:NSPasteboardTypeString];
}
- (void)revealRecord:(WorkspaceButton *)sender { NSString *path=S(sender.record[@"path"]);if(path.isAbsolutePath&&self.pathHandler)self.pathHandler(path); }
- (void)startForRestore:(id)sender { [self closeDetail:nil];[self selectSection:@"status"];if(self.actionHandler)self.actionHandler(@"start"); }
- (void)reviewRestore:(NSButton *)sender {
    sender.enabled=NO;NSDictionary *record=self.selectedRecord;__weak typeof(self) weakSelf=self;
    [self request:@"history-preview" parameters:@{@"id":S(record[@"id"]),@"revision":S(record[@"revision"])} reply:^(NSDictionary *result,NSString *error){
        if(!weakSelf.detailPanel||weakSelf.selectedRecord!=record)return;
        Add(weakSelf.detailBody,Label(error?:RestoreMessage(result[@"message"]),13,NSFontWeightRegular));
        if(!error&&[N(result[@"allowed"]) boolValue])[weakSelf.detailBody addArrangedSubview:[weakSelf button:W(@"Confirm restore",@"되돌리기") action:@selector(confirmRestore:) identifier:@"history-confirm-restore"]];
        JasoApplyContentZoom(weakSelf.detailBody);
    }];
}
- (void)confirmRestore:(NSButton *)sender {
    if(self.restoreRequest)return;sender.enabled=NO;NSDictionary *parameters=@{@"request_id":NSUUID.UUID.UUIDString,@"operation_id":S(self.selectedRecord[@"id"]),@"revision":S(self.selectedRecord[@"revision"])};self.restoreRequest=parameters;self.restorePanel=self.detailPanel;
    __weak typeof(self) weakSelf=self;[self request:@"history-restore" parameters:parameters reply:^(NSDictionary *result,NSString *error){[weakSelf showRestoreResult:result error:error request:parameters];}];
}
- (void)showRestoreResult:(NSDictionary *)result error:(NSString *)error request:(NSDictionary *)request {
    if(![self.restoreRequest[@"request_id"] isEqual:request[@"request_id"]])return;
    if(result&&(![result[@"request_id"] isEqual:request[@"request_id"]]||![result[@"operation_id"] isEqual:request[@"operation_id"]]))return;
    NSString *state=S(result[@"state"]);BOOL terminal=[@[@"restored",@"rejected",@"recovery_required"] containsObject:state];
    if(self.detailPanel&&self.detailPanel==self.restorePanel&&[self.selectedRecord[@"id"] isEqual:request[@"operation_id"]]){NSString *message=error?:([state isEqual:@"restored"]?W(@"The original name was restored.",@"원래 이름으로 되돌렸습니다."):[state isEqual:@"queued"]?W(@"Your restore request is queued. The result will appear here.",@"되돌리기 요청을 접수했습니다. 처리 결과를 여기에 표시합니다."):RestoreMessage(result[@"message"]));
        NSView *old=nil;for(NSView *view in self.detailBody.arrangedSubviews)if([view.identifier isEqual:@"restore-result"])old=view;if(old){[self.detailBody removeArrangedSubview:old];[old removeFromSuperview];}BOOL waitingForStart=[state isEqual:@"queued"]&&N(self.snapshot[@"running"])&&![self.snapshot[@"running"] boolValue];
        if(waitingForStart)message=W(@"Cleanup is stopped. Start it to process this restore request.",@"작업이 중지되어 있습니다. 작업을 시작하면 이 되돌리기 요청을 처리합니다.");
        NSTextField *label=Label(message,13,NSFontWeightRegular);label.identifier=@"restore-result";Add(self.detailBody,label);
        NSButton *start=nil;for(NSView *view in self.detailBody.arrangedSubviews)if([view.identifier isEqual:@"history-start"]&&[view isKindOfClass:NSButton.class])start=(id)view;
        if(waitingForStart&&!start)[self.detailBody addArrangedSubview:[self button:W(@"Start cleanup",@"작업 시작") action:@selector(startForRestore:) identifier:@"history-start"]];else if(start&&!waitingForStart){[self.detailBody removeArrangedSubview:start];[start removeFromSuperview];}
        JasoApplyContentZoom(self.detailBody);
    }
    if(terminal){self.restoreRequest=nil;self.restorePanel=nil;}
    if([state isEqual:@"restored"])[self loadHistory];
}
- (void)separateActivity:(id)sender {
    if(self.activityOnly)return;
    if(!self.activityWindow){self.activityWindow=JasoWorkspaceWindowController.new;self.activityWindow.activityOnly=YES;self.activityWindow.navigation.superview.hidden=YES;for(NSLayoutConstraint *constraint in self.activityWindow.window.contentView.constraints)if(constraint.firstItem==self.activityWindow.navigation.superview&&constraint.firstAttribute==NSLayoutAttributeWidth)constraint.constant=0;self.activityWindow.requestHandler=self.requestHandler;self.activityWindow.actionHandler=self.actionHandler;self.activityWindow.pathHandler=self.pathHandler;[self.activityWindow selectSection:@"activity"];}
    [self.activityWindow updateSnapshot:self.snapshot error:self.snapshotError updatedAt:self.snapshotDate];[self.activityWindow showWindow:nil];
}
- (void)poll {
    if(!self.window.visible||self.window.miniaturized||NSApp.hidden||self.closed)return;
    self.tick++;if(self.tick%5==0&&self.refreshHandler)self.refreshHandler();
    if([@[@"status",@"activity"] containsObject:self.selectedSection])[self loadActivity];
    if([self.selectedSection isEqual:@"status"]&&(!self.history||self.tick%20==0))[self loadHistory];
    if(self.restoreRequest){NSDictionary *parameters=self.restoreRequest;__weak typeof(self) weakSelf=self;[self request:@"history-result" parameters:parameters reply:^(NSDictionary *result,NSString *error){[weakSelf showRestoreResult:result error:error request:parameters];}];}
}
- (BOOL)refreshingAutomatically { return self.timer!=nil; }
- (void)syncTimer {
    BOOL active=self.window.visible&&!self.window.miniaturized&&!NSApp.hidden&&!self.closed;
    if(active&&!self.timer){__weak typeof(self) weakSelf=self;self.timer=[NSTimer scheduledTimerWithTimeInterval:1 repeats:YES block:^(NSTimer *timer){[weakSelf poll];}];self.timer.tolerance=.2;}
    else if(!active){[self.timer invalidate];self.timer=nil;}
}
- (void)showWindow:(id)sender { self.closed=NO;[super showWindow:sender];[NSApp activateIgnoringOtherApps:YES];[self.window makeKeyAndOrderFront:nil];[self syncTimer];[self poll]; }
- (void)windowDidChangeOcclusionState:(NSNotification *)notification { [self syncTimer]; }
- (void)windowDidMiniaturize:(NSNotification *)notification { [self syncTimer]; }
- (void)windowDidDeminiaturize:(NSNotification *)notification { [self syncTimer]; }
- (void)windowDidResize:(NSNotification *)notification { [self updateAdaptiveRows]; }
- (void)visibilityChanged:(NSNotification *)notification { [self syncTimer]; }
- (void)windowWillClose:(NSNotification *)notification { self.closed=YES;[self.timer invalidate];self.timer=nil;[self closeDetail:nil];if(self.actionHandler)self.actionHandler(@"cancel-preview");if(self.closeHandler)self.closeHandler(); }
- (void)reloadLocalization { [self rebuildNavigation];[self render];[self.activityWindow reloadLocalization]; }
- (CGFloat)contentZoom { return JasoContentZoom(); }
- (void)zoomIn:(id)sender { JasoSetContentZoom(self.contentZoom+.1); }
- (void)zoomOut:(id)sender { JasoSetContentZoom(self.contentZoom-.1); }
- (void)resetZoom:(id)sender { JasoSetContentZoom(1); }
- (void)zoomChanged:(NSNotification *)notification {
    NSValue *outerPosition=self.scroll?[NSValue valueWithPoint:self.scroll.contentView.bounds.origin]:nil;
    if(outerPosition)self.scrollPositions[self.selectedSection]=outerPosition;
    BOOL restoreFeed=self.activityTable&&!self.followActivity;
    NSPoint feedOrigin=self.activityTable.enclosingScrollView.contentView.bounds.origin;
    CGFloat rowPosition=restoreFeed?feedOrigin.y/(self.activityTable.rowHeight+self.activityTable.intercellSpacing.height):0;
    NSResponder *responder=self.window.firstResponder;
    NSView *focused=[responder isKindOfClass:NSView.class]?(NSView *)responder:nil;
    NSTextView *editor=[responder isKindOfClass:NSTextView.class]&&[(NSTextView *)responder isFieldEditor]?(NSTextView *)responder:nil;
    if(editor&&[(id)editor.delegate isKindOfClass:NSView.class])focused=(NSView *)editor.delegate;
    NSString *identifier=focused.identifier;
    NSString *editingText=[editor.string copy];NSRange selection=editor?editor.selectedRange:NSMakeRange(NSNotFound,0);
    self.restoringZoomState=YES;
    @try {
        [self rebuildNavigation];[self render];
        NSView *current=ViewWithIdentifier(self.window.contentView,identifier);
        if(current){
            if(editingText&&[current isKindOfClass:NSTextField.class])[(NSTextField *)current setStringValue:editingText];
            [self.window makeFirstResponder:current];
            if(editingText&&[current isKindOfClass:NSTextField.class]){
                NSTextView *newEditor=(NSTextView *)[(NSTextField *)current currentEditor];
                if(newEditor){newEditor.string=editingText;NSUInteger start=MIN(selection.location,editingText.length);newEditor.selectedRange=NSMakeRange(start,MIN(selection.length,editingText.length-start));}
            }
        }
        [self.pageHost layoutSubtreeIfNeeded];
        if(restoreFeed&&self.activityTable){
            NSScrollView *feed=self.activityTable.enclosingScrollView;
            feedOrigin.y=rowPosition*(self.activityTable.rowHeight+self.activityTable.intercellSpacing.height);
            [feed.contentView scrollToPoint:feedOrigin];[feed reflectScrolledClipView:feed.contentView];
        }
        if(outerPosition&&self.scroll){[self.scroll.contentView scrollToPoint:outerPosition.pointValue];[self.scroll reflectScrolledClipView:self.scroll.contentView];}
    } @finally {self.restoringZoomState=NO;}
}
- (BOOL)validateUserInterfaceItem:(id<NSValidatedUserInterfaceItem>)item { return JasoValidateContentZoomAction(self.window,item.action); }
- (void)dealloc { [_timer invalidate];[NSNotificationCenter.defaultCenter removeObserver:self]; }
@end
