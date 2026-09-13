#import "StatusWindow.h"
#import "StatusPresentation.h"
#import "Localization.h"
#import "ContentZoom.h"

static NSString *SL(NSString *en, NSString *ko) {
    return JasoText(en, ko);
}
static NSColor *ToneColor(NSString *tone) {
    if ([tone isEqual:@"good"]) return NSColor.systemGreenColor;
    if ([tone isEqual:@"working"]) return NSColor.systemTealColor;
    if ([tone isEqual:@"warning"]) return NSColor.systemOrangeColor;
    return NSColor.secondaryLabelColor;
}
static BOOL RevealablePath(id path) {
    return [path isKindOfClass:NSString.class] && [path isAbsolutePath] &&
        [path rangeOfString:[NSString stringWithFormat:@"%C", (unichar)0]].location == NSNotFound;
}
@interface JasoIssueRevealButton : NSButton
@property (copy) NSString *path;
@end
@implementation JasoIssueRevealButton
@end
static NSTextField *Text(NSString *value, CGFloat size, NSFontWeight weight, NSColor *color) {
    NSTextField *field = [NSTextField wrappingLabelWithString:value ?: @""];
    field.font = [NSFont systemFontOfSize:size weight:weight];
    field.textColor = color ?: NSColor.labelColor;
    field.selectable = YES;
    field.translatesAutoresizingMaskIntoConstraints = NO;
    [field setContentCompressionResistancePriority:NSLayoutPriorityRequired forOrientation:NSLayoutConstraintOrientationVertical];
    return field;
}
static NSStackView *Stack(NSArray<NSView *> *views, BOOL vertical, CGFloat spacing) {
    NSStackView *stack = [NSStackView stackViewWithViews:views];
    stack.orientation = vertical ? NSUserInterfaceLayoutOrientationVertical : NSUserInterfaceLayoutOrientationHorizontal;
    stack.alignment = vertical ? NSLayoutAttributeLeading : NSLayoutAttributeTop;
    stack.spacing = spacing;
    stack.translatesAutoresizingMaskIntoConstraints = NO;
    return stack;
}
static void Pin(NSView *child, NSView *parent, CGFloat inset) {
    [NSLayoutConstraint activateConstraints:@[
        [child.leadingAnchor constraintEqualToAnchor:parent.leadingAnchor constant:inset],
        [child.trailingAnchor constraintEqualToAnchor:parent.trailingAnchor constant:-inset],
        [child.topAnchor constraintEqualToAnchor:parent.topAnchor constant:inset],
        [child.bottomAnchor constraintEqualToAnchor:parent.bottomAnchor constant:-inset]
    ]];
}
@interface JasoStatusCard : NSView
@end
@implementation JasoStatusCard
- (void)drawRect:(NSRect)dirtyRect {
    [NSColor.controlBackgroundColor setFill];
    NSBezierPath *path = [NSBezierPath bezierPathWithRoundedRect:NSInsetRect(self.bounds, .5, .5) xRadius:12 yRadius:12];
    [path fill];
    [NSColor.separatorColor setStroke]; path.lineWidth = .5; [path stroke];
}
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; self.needsDisplay = YES; }
@end
@interface JasoStatusBackground : NSView
@end
@implementation JasoStatusBackground
- (void)drawRect:(NSRect)dirtyRect { [NSColor.windowBackgroundColor setFill]; NSRectFill(self.bounds); }
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; self.needsDisplay = YES; }
@end
@interface JasoStatusWindow : NSWindow
@property (copy) void (^visibilityChanged)(void);
@end
@implementation JasoStatusWindow
- (void)orderWindow:(NSWindowOrderingMode)place relativeTo:(NSInteger)otherWindowNumber {
    [super orderWindow:place relativeTo:otherWindowNumber];
    if (self.visibilityChanged) self.visibilityChanged();
}
@end
static NSView *Card(NSView *content, CGFloat inset) {
    NSView *card = [JasoStatusCard new]; card.translatesAutoresizingMaskIntoConstraints = NO;
    [card addSubview:content]; Pin(content, card, inset); return card;
}
static void FullWidth(NSStackView *stack, NSView *view) {
    [stack addArrangedSubview:view];
    [view.widthAnchor constraintEqualToAnchor:stack.widthAnchor constant:-(stack.edgeInsets.left + stack.edgeInsets.right)].active = YES;
}

@interface JasoStatusWindowController ()
@property NSStackView *body;
@property NSScrollView *scroll;
@property NSTextField *updatedLabel;
@property NSButton *refreshButton;
@property NSButton *primaryButton;
@property NSButton *diagnosticsButton;
@property NSButton *settingsButton;
@property NSArray<NSButton *> *maintenanceButtons;
@property BOOL workerRunning;
@property NSTimer *refreshTimer;
@property NSString *primaryAction;
@property BOOL commandEnabled;
@property BOOL busy;
@property BOOL automaticRetriesExpanded;
@property NSDictionary *latestSnapshot;
@property NSString *latestError;
@property NSDate *latestDate;
@end

@implementation JasoStatusWindowController
- (instancetype)init {
    JasoStatusWindow *window = [[JasoStatusWindow alloc] initWithContentRect:NSMakeRect(0, 0, 760, 700)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable
        backing:NSBackingStoreBuffered defer:NO];
    if (!(self = [super initWithWindow:window])) return nil;
    window.title = SL(@"Jaso NFC · Cleanup status", @"Jaso NFC · 파일명 정리 상태");
    window.minSize = NSMakeSize(680, 520);
    window.releasedWhenClosed = NO;
    window.delegate = self;
    __weak JasoStatusWindowController *weakSelf = self;
    window.visibilityChanged = ^{ [weakSelf syncRefreshTimer]; };
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(applicationVisibilityChanged:) name:NSApplicationDidHideNotification object:NSApp];
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(applicationVisibilityChanged:) name:NSApplicationDidUnhideNotification object:NSApp];
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(contentZoomChanged:) name:JasoContentZoomDidChangeNotification object:nil];
    [window center];
    window.contentView = [[JasoStatusBackground alloc] initWithFrame:window.contentView.bounds];
    [window.contentView.widthAnchor constraintGreaterThanOrEqualToConstant:680].active = YES;
    // Keep reflow inside the window when intrinsic text sizes change.
    [window.contentView.widthAnchor constraintEqualToAnchor:((NSLayoutGuide *)window.contentLayoutGuide).widthAnchor].active = YES;
    NSView *content = window.contentView;
    self.scroll = [NSScrollView new]; self.scroll.translatesAutoresizingMaskIntoConstraints = NO;
    self.scroll.hasVerticalScroller = YES; self.scroll.autohidesScrollers = YES; self.scroll.drawsBackground = NO;
    self.body = Stack(@[], YES, 20); self.body.edgeInsets = NSEdgeInsetsMake(24, 24, 24, 24);
    self.scroll.documentView = self.body;
    [content addSubview:self.scroll];
    self.updatedLabel = Text(SL(@"Checking status…", @"상태 확인 중…"), 11, NSFontWeightRegular, NSColor.secondaryLabelColor);
    self.updatedLabel.maximumNumberOfLines = 2;
    self.refreshButton = [NSButton buttonWithTitle:SL(@"Refresh", @"새로고침") target:self action:@selector(refresh:)];
    self.diagnosticsButton = [NSButton buttonWithTitle:SL(@"Copy diagnostics", @"진단 정보 복사") target:self action:@selector(copyDiagnostics:)];
    self.diagnosticsButton.toolTip = SL(@"Copy the current status and file paths for a support request.", @"지원 요청에 사용할 현재 상태와 파일 경로를 복사합니다.");
    NSButton *settings = [NSButton buttonWithTitle:SL(@"Settings…", @"설정…") target:self action:@selector(showSettings:)];
    self.settingsButton = settings;
    self.settingsButton.identifier = @"status-settings";
    self.diagnosticsButton.identifier = @"status-diagnostics";
    self.refreshButton.identifier = @"status-refresh";
    NSStackView *actions = Stack(@[settings, self.diagnosticsButton, self.refreshButton], NO, 12);
    actions.alignment = NSLayoutAttributeCenterY;
    NSStackView *footer = Stack(@[self.updatedLabel, actions], YES, 8);
    [self.updatedLabel.widthAnchor constraintEqualToAnchor:footer.widthAnchor].active = YES;
    [self.updatedLabel setContentHuggingPriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];
    [content addSubview:footer];
    [NSLayoutConstraint activateConstraints:@[
        [self.scroll.topAnchor constraintEqualToAnchor:content.topAnchor],
        [self.scroll.leadingAnchor constraintEqualToAnchor:content.leadingAnchor],
        [self.scroll.trailingAnchor constraintEqualToAnchor:content.trailingAnchor],
        [self.scroll.bottomAnchor constraintEqualToAnchor:footer.topAnchor constant:-12],
        [footer.leadingAnchor constraintEqualToAnchor:content.leadingAnchor constant:24],
        [footer.trailingAnchor constraintEqualToAnchor:content.trailingAnchor constant:-24],
        [footer.bottomAnchor constraintEqualToAnchor:content.bottomAnchor constant:-16],
        [self.body.widthAnchor constraintEqualToAnchor:self.scroll.contentView.widthAnchor],
        [self.body.topAnchor constraintEqualToAnchor:self.scroll.contentView.topAnchor],
        [self.body.leadingAnchor constraintEqualToAnchor:self.scroll.contentView.leadingAnchor]
    ]];
    [self updateSnapshot:nil error:nil updatedAt:nil];
    return self;
}
- (BOOL)refreshingAutomatically { return self.refreshTimer != nil; }
- (CGFloat)contentZoom { return JasoContentZoom(); }
- (void)zoomIn:(id)sender { JasoSetContentZoom(self.contentZoom + .1); }
- (void)zoomOut:(id)sender { JasoSetContentZoom(self.contentZoom - .1); }
- (void)resetZoom:(id)sender { JasoSetContentZoom(1); }
- (BOOL)validateUserInterfaceItem:(id<NSValidatedUserInterfaceItem>)item {
    return JasoValidateContentZoomAction(self.window, item.action);
}
- (void)contentZoomChanged:(NSNotification *)notification {
    [self updateSnapshot:self.latestSnapshot error:self.latestError updatedAt:self.latestDate];
}
- (void)reloadLocalization {
    self.window.title = SL(@"Jaso NFC · Cleanup status", @"Jaso NFC · 파일명 정리 상태");
    self.settingsButton.title = SL(@"Settings…", @"설정…");
    self.diagnosticsButton.title = SL(@"Copy diagnostics", @"진단 정보 복사");
    self.diagnosticsButton.toolTip = SL(@"Copy the current status and file paths for a support request.", @"지원 요청에 사용할 현재 상태와 파일 경로를 복사합니다.");
    [self updateSnapshot:self.latestSnapshot error:self.latestError updatedAt:self.latestDate];
}
- (void)syncRefreshTimer {
    BOOL visible = self.window.visible && !self.window.miniaturized && !NSApp.hidden;
    if (visible && !self.refreshTimer) {
        __weak JasoStatusWindowController *weakSelf = self;
        self.refreshTimer = [NSTimer scheduledTimerWithTimeInterval:5 repeats:YES block:^(NSTimer *timer) {
            JasoStatusWindowController *controller = weakSelf;
            if (!controller) { [timer invalidate]; return; }
            if (controller.window.visible && !controller.busy && controller.refreshHandler) controller.refreshHandler();
        }];
        self.refreshTimer.tolerance = 1;
    } else if (!visible) {
        [self.refreshTimer invalidate]; self.refreshTimer = nil;
    }
}
- (void)showWindow:(id)sender {
    [super showWindow:sender];
    [NSApp activateIgnoringOtherApps:YES];
    [self.window makeKeyAndOrderFront:nil];
    [self syncRefreshTimer];
}
- (void)windowDidChangeOcclusionState:(NSNotification *)notification { [self syncRefreshTimer]; }
- (void)applicationVisibilityChanged:(NSNotification *)notification { [self syncRefreshTimer]; }
- (void)windowDidMiniaturize:(NSNotification *)notification { [self syncRefreshTimer]; }
- (void)windowDidDeminiaturize:(NSNotification *)notification { [self syncRefreshTimer]; [self refresh:nil]; }
- (void)windowWillClose:(NSNotification *)notification {
    [self.refreshTimer invalidate]; self.refreshTimer = nil;
    if (self.closeHandler) self.closeHandler();
}
- (void)dealloc { [_refreshTimer invalidate]; [NSNotificationCenter.defaultCenter removeObserver:self]; }
- (void)refresh:(id)sender { if (!self.busy && self.refreshHandler) self.refreshHandler(); }
- (void)performPrimary:(id)sender { if (!self.busy && self.commandEnabled && self.actionHandler) self.actionHandler(self.primaryAction); }
- (void)showPermissions:(id)sender { if (self.actionHandler) self.actionHandler(@"permissions"); }
- (void)openHistory:(id)sender { if (self.actionHandler) self.actionHandler(@"history"); }
- (void)showSettings:(id)sender { if (self.actionHandler) self.actionHandler(@"settings"); }
- (void)copyDiagnostics:(id)sender { if (self.diagnosticsHandler) self.diagnosticsHandler(); }
- (void)maintenanceAction:(NSButton *)sender {
    if (sender.enabled && self.actionHandler) self.actionHandler(sender.identifier);
}
- (void)revealIssue:(JasoIssueRevealButton *)sender {
    if (RevealablePath(sender.path) && self.pathHandler) self.pathHandler(sender.path);
}
- (void)toggleAutomaticRetries:(NSButton *)sender {
    self.automaticRetriesExpanded = sender.state == NSControlStateValueOn;
    [self updateSnapshot:self.latestSnapshot error:self.latestError updatedAt:self.latestDate];
}
- (void)appendIssues:(NSArray<NSDictionary *> *)issues toStack:(NSStackView *)stack {
    NSUInteger displayed = 0;
    for (NSDictionary *issue in issues) {
        if (displayed++ == 16) break;
        NSString *title = [issue[@"title"] isKindOfClass:NSString.class] ? issue[@"title"] : @"";
        NSString *pathValue = [issue[@"path"] isKindOfClass:NSString.class] ? issue[@"path"] : @"";
        NSString *detail = [issue[@"detail"] isKindOfClass:NSString.class] ? issue[@"detail"] : @"";
        NSStackView *row = Stack(@[], YES, 7);
        NSString *filename = pathValue.lastPathComponent;
        if (filename.length) FullWidth(row, Text(filename, 14, NSFontWeightSemibold, NSColor.labelColor));
        FullWidth(row, Text(title, 12, NSFontWeightSemibold, ToneColor(issue[@"tone"])));
        if (pathValue.length) {
            NSTextField *path = Text(pathValue, 11, NSFontWeightRegular, NSColor.secondaryLabelColor);
            path.identifier = @"issue-path";
            path.lineBreakMode = NSLineBreakByCharWrapping;
            path.toolTip = pathValue;
            FullWidth(row, path);
        }
        if (detail.length) FullWidth(row, Text(detail, 12, NSFontWeightRegular, NSColor.labelColor));
        NSString *actionTitle = [issue[@"actionTitle"] isKindOfClass:NSString.class] ? issue[@"actionTitle"] : @"";
        if ([issue[@"action"] isEqual:@"reveal"] && RevealablePath(pathValue) && actionTitle.length) {
            JasoIssueRevealButton *reveal = [JasoIssueRevealButton buttonWithTitle:actionTitle target:self action:@selector(revealIssue:)];
            reveal.identifier = @"issue-reveal";
            reveal.path = pathValue;
            reveal.toolTip = pathValue;
            reveal.accessibilityHelp = pathValue;
            [row addArrangedSubview:reveal];
        }
        FullWidth(stack, Card(row, 16));
    }
}
- (void)setRefreshing:(BOOL)refreshing {
    self.busy = refreshing;
    self.refreshButton.enabled = !refreshing;
    self.refreshButton.title = refreshing ? SL(@"Checking…", @"확인 중…") : SL(@"Refresh", @"새로고침");
    self.primaryButton.enabled = !refreshing && self.commandEnabled;
    for (NSButton *button in self.maintenanceButtons) {
        BOOL needsWorker = [@[@"stop", @"reconcile"] containsObject:button.identifier];
        button.enabled = [button.identifier isEqual:@"history"] || (!refreshing && (!needsWorker || self.workerRunning));
    }
}
- (void)updateSnapshot:(NSDictionary *)snapshot error:(NSString *)error updatedAt:(NSDate *)date {
    NSResponder *focused = self.window.firstResponder;
    BOOL restoreRetryFocus = [focused isKindOfClass:NSButton.class] && [((NSButton *)focused).identifier isEqual:@"automatic-retry-disclosure"];
    NSButton *automaticDisclosure = nil;
    self.latestSnapshot = snapshot;
    self.latestError = error;
    self.latestDate = date;
    self.workerRunning = !error && [snapshot[@"running"] isKindOfClass:NSNumber.class] && [snapshot[@"running"] boolValue];
    NSDictionary *presentation = JasoStatusPresentation(snapshot, error, JasoUsesKorean());
    NSPoint previousOrigin = self.scroll.contentView.bounds.origin;
    for (NSView *view in self.body.arrangedSubviews.copy) { [self.body removeArrangedSubview:view]; [view removeFromSuperview]; }

    NSTextField *eyebrow = Text(SL(@"JASO NFC  /  CLEANUP STATUS", @"JASO NFC  /  파일명 정리 상태"), 11, NSFontWeightSemibold, NSColor.secondaryLabelColor);
    FullWidth(self.body, eyebrow);
    NSColor *tone = ToneColor(presentation[@"tone"]);
    NSImageView *icon = [NSImageView imageViewWithImage:[NSImage imageWithSystemSymbolName:presentation[@"symbol"] accessibilityDescription:nil] ?: [NSImage imageWithSystemSymbolName:@"folder" accessibilityDescription:nil]];
    icon.contentTintColor = tone; icon.symbolConfiguration = [NSImageSymbolConfiguration configurationWithPointSize:30 weight:NSFontWeightRegular];
    [icon.widthAnchor constraintEqualToConstant:40].active = YES; [icon.heightAnchor constraintEqualToConstant:40].active = YES;
    NSStackView *heading = Stack(@[Text(presentation[@"title"], 25, NSFontWeightSemibold, nil), Text(presentation[@"subtitle"], 13, NSFontWeightRegular, NSColor.secondaryLabelColor)], YES, 8);
    for (NSView *view in heading.arrangedSubviews) [view.widthAnchor constraintEqualToAnchor:heading.widthAnchor].active = YES;
    NSStackView *hero = Stack(@[icon, heading], NO, 14);
    FullWidth(self.body, hero);
    self.primaryAction = presentation[@"primaryAction"] ?: @"";
    self.commandEnabled = self.primaryAction.length > 0;
    self.primaryButton = [NSButton buttonWithTitle:presentation[@"primaryTitle"] ?: @"" target:self action:@selector(performPrimary:)];
    self.primaryButton.bezelColor = NSColor.controlAccentColor;
    self.primaryButton.enabled = self.commandEnabled && !self.busy;
    if (self.commandEnabled) [self.body addArrangedSubview:self.primaryButton];

    NSMutableArray *cards = [NSMutableArray array];
    for (NSDictionary *metric in presentation[@"metrics"]) {
        NSStackView *values = Stack(@[Text(metric[@"label"], 12, NSFontWeightMedium, NSColor.secondaryLabelColor),
            Text(metric[@"value"], 28, NSFontWeightSemibold, nil), Text(metric[@"note"], 11, NSFontWeightRegular, NSColor.secondaryLabelColor)], YES, 8);
        for (NSView *view in values.arrangedSubviews) [view.widthAnchor constraintEqualToAnchor:values.widthAnchor].active = YES;
        [cards addObject:Card(values, 16)];
    }
    BOOL stacked = self.contentZoom > 1.2;
    NSStackView *metrics = Stack(cards, stacked, 12);
    if (stacked) {
        for (NSView *card in cards) [card.widthAnchor constraintEqualToAnchor:metrics.widthAnchor].active = YES;
    } else {
        metrics.distribution = NSStackViewDistributionFillEqually;
        metrics.alignment = NSLayoutAttributeHeight;
    }
    FullWidth(self.body, metrics);
    FullWidth(self.body, Text(presentation[@"progressNote"], 12, NSFontWeightRegular, NSColor.secondaryLabelColor));

    NSArray *issues = [presentation[@"issues"] isKindOfClass:NSArray.class] ? presentation[@"issues"] : @[];
    NSMutableArray *actionIssues = [NSMutableArray array], *automaticIssues = [NSMutableArray array];
    for (id value in issues) {
        if (![value isKindOfClass:NSDictionary.class]) continue;
        NSDictionary *issue = value;
        BOOL requiresAction = [issue[@"requiresAction"] isKindOfClass:NSNumber.class] && [issue[@"requiresAction"] boolValue];
        [(requiresAction ? actionIssues : automaticIssues) addObject:issue];
    }
    if (actionIssues.count) {
        FullWidth(self.body, Text(SL(@"Needs your attention", @"확인이 필요한 항목"), 15, NSFontWeightSemibold, nil));
        NSString *detail = [presentation[@"actionIssueSummary"] isKindOfClass:NSString.class] ? presentation[@"actionIssueSummary"] : @"";
        if (detail.length) {
            NSTextField *summary = Text(detail, 12, NSFontWeightRegular, NSColor.secondaryLabelColor);
            summary.identifier = @"action-issue-summary";
            FullWidth(self.body, summary);
        }
        [self appendIssues:actionIssues toStack:self.body];
    }

    NSArray *notices = presentation[@"notices"];
    if (notices.count) {
        FullWidth(self.body, Text(SL(@"Things to know", @"확인할 내용"), 15, NSFontWeightSemibold, nil));
        NSStackView *noticeRows = Stack(@[], YES, 14);
        for (NSDictionary *notice in notices) {
            NSStackView *row = Stack(@[Text(notice[@"title"], 13, NSFontWeightSemibold, ToneColor(notice[@"tone"])), Text(notice[@"detail"], 12, NSFontWeightRegular, NSColor.secondaryLabelColor)], YES, 5);
            for (NSView *view in row.arrangedSubviews) [view.widthAnchor constraintEqualToAnchor:row.widthAnchor].active = YES;
            FullWidth(noticeRows, row);
        }
        NSButton *permissions = [NSButton buttonWithTitle:SL(@"Permissions guide", @"접근 권한 안내") target:self action:@selector(showPermissions:)];
        [noticeRows addArrangedSubview:permissions];
        FullWidth(self.body, Card(noticeRows, 16));
    }

    FullWidth(self.body, Text(SL(@"Watched locations", @"감시 위치"), 15, NSFontWeightSemibold, nil));
    NSStackView *locations = Stack(@[], YES, 14);
    NSArray *roots = presentation[@"locations"];
    if (!roots.count) FullWidth(locations, Text(SL(@"Start cleanup to load the watched locations.", @"작업을 시작하면 감시 위치를 확인할 수 있습니다."), 12, NSFontWeightRegular, NSColor.secondaryLabelColor));
    for (NSDictionary *root in roots) {
        NSTextField *name = Text(root[@"title"], 13, NSFontWeightSemibold, nil);
        NSTextField *state = Text(root[@"state"], 11, NSFontWeightMedium, ToneColor(root[@"tone"]));
        [state setContentHuggingPriority:NSLayoutPriorityRequired forOrientation:NSLayoutConstraintOrientationHorizontal];
        NSStackView *line = Stack(@[name, state], stacked, stacked ? 4 : 12);
        if (stacked) {
            [name.widthAnchor constraintEqualToAnchor:line.widthAnchor].active = YES;
            [state.widthAnchor constraintEqualToAnchor:line.widthAnchor].active = YES;
        } else line.alignment = NSLayoutAttributeFirstBaseline;
        NSTextField *path = Text(root[@"path"], 11, NSFontWeightRegular, NSColor.secondaryLabelColor);
        path.lineBreakMode = NSLineBreakByCharWrapping;
        NSStackView *row = Stack(@[line, path], YES, 4);
        if ([root[@"detail"] length]) [row addArrangedSubview:Text(root[@"detail"], 12, NSFontWeightRegular, NSColor.labelColor)];
        for (NSView *view in row.arrangedSubviews) [view.widthAnchor constraintEqualToAnchor:row.widthAnchor].active = YES;
        FullWidth(locations, row);
    }
    FullWidth(self.body, Card(locations, 16));
    NSString *automaticSummary = [presentation[@"automaticRetrySummary"] isKindOfClass:NSString.class] ? presentation[@"automaticRetrySummary"] : @"";
    if (automaticSummary.length) {
        NSStackView *automaticGroup = Stack(@[], YES, 14);
        automaticGroup.identifier = @"automatic-retry-group";
        NSButton *disclosure = [NSButton buttonWithTitle:automaticSummary target:self action:@selector(toggleAutomaticRetries:)];
        automaticDisclosure = disclosure;
        disclosure.identifier = @"automatic-retry-disclosure";
        [disclosure setButtonType:NSButtonTypePushOnPushOff];
        disclosure.bordered = NO;
        disclosure.alignment = NSTextAlignmentLeft;
        disclosure.font = [NSFont systemFontOfSize:13 weight:NSFontWeightMedium];
        disclosure.contentTintColor = NSColor.secondaryLabelColor;
        disclosure.image = [NSImage imageWithSystemSymbolName:self.automaticRetriesExpanded ? @"chevron.down" : @"chevron.right" accessibilityDescription:nil];
        disclosure.imagePosition = NSImageLeft;
        disclosure.imageHugsTitle = YES;
        disclosure.state = self.automaticRetriesExpanded ? NSControlStateValueOn : NSControlStateValueOff;
        disclosure.accessibilityHelp = SL(@"Show or hide the items scheduled for another automatic attempt.", @"자동 재시도 항목의 상세 정보를 펼치거나 접습니다.");
        FullWidth(automaticGroup, disclosure);
        if (self.automaticRetriesExpanded) {
            NSString *detail = [presentation[@"automaticRetryDetail"] isKindOfClass:NSString.class] ? presentation[@"automaticRetryDetail"] : @"";
            if (detail.length) {
                NSTextField *summary = Text(detail, 12, NSFontWeightRegular, NSColor.secondaryLabelColor);
                summary.identifier = @"automatic-retry-detail";
                FullWidth(automaticGroup, summary);
            }
            [self appendIssues:automaticIssues toStack:automaticGroup];
        }
        FullWidth(self.body, automaticGroup);
    }
    FullWidth(self.body, Text(SL(@"Maintenance", @"작업 관리"), 15, NSFontWeightSemibold, nil));
    NSStackView *maintenance = Stack(@[], YES, 10);
    FullWidth(maintenance, Text(SL(@"Recheck folders, restart cleanup after changing permissions, or open recovery history. Your login startup preference stays saved when you stop cleanup.", @"폴더를 다시 확인하거나 권한 변경 후 작업을 다시 시작하고, 복구 기록을 열어 보세요. 작업을 중지하면 로그인 실행 선택은 저장된 상태로 유지합니다."), 12, NSFontWeightRegular, NSColor.secondaryLabelColor));
    NSMutableArray *buttons = [NSMutableArray array];
    NSArray *titles = @[SL(@"Restart worker", @"작업 다시 시작"), SL(@"Stop worker", @"백그라운드 작업 중지"), SL(@"Recheck all watched roots", @"전체 감시 영역 다시 확인"), SL(@"Open recovery history", @"복구 기록 폴더 열기")];
    NSArray *actions = @[@"restart", @"stop", @"reconcile", @"history"];
    for (NSUInteger index = 0; index < actions.count; index++) {
        NSButton *button = [NSButton buttonWithTitle:titles[index] target:self action:@selector(maintenanceAction:)];
        button.identifier = actions[index]; [buttons addObject:button];
        [maintenance addArrangedSubview:button];
    }
    self.maintenanceButtons = buttons;
    FullWidth(self.body, Card(maintenance, 16));
    FullWidth(self.body, Text(SL(@"Choose the folders to clean up in Settings → Manage folders….", @"설정 → ‘정리할 폴더…’에서 정리할 위치를 선택하세요."), 11, NSFontWeightRegular, NSColor.tertiaryLabelColor));

    self.diagnosticsButton.enabled = snapshot != nil;
    [self setRefreshing:self.busy];
    if (date) {
        NSDateFormatter *formatter = [NSDateFormatter new];
        formatter.locale = [NSLocale localeWithLocaleIdentifier:JasoUsesKorean() ? @"ko_KR" : @"en_US"];
        formatter.timeStyle = NSDateFormatterMediumStyle; formatter.dateStyle = NSDateFormatterNoStyle;
        self.updatedLabel.stringValue = [NSString stringWithFormat:error ? SL(@"Last successful check %@ · refresh failed", @"마지막 확인 %@ · 새로고침 실패") : SL(@"Updated %@ · refreshes while visible", @"%@ 확인 · 창이 보일 때 자동 갱신"), [formatter stringFromDate:date]];
    } else self.updatedLabel.stringValue = error ? SL(@"Status unavailable · try Refresh", @"상태 확인 불가 · 새로고침으로 재시도") : SL(@"Checking status…", @"상태 확인 중…");
    JasoApplyContentZoom(self.window.contentView);
    [self.window.contentView layoutSubtreeIfNeeded];
    CGFloat maximumY = MAX(0, self.body.frame.size.height - self.scroll.contentView.bounds.size.height);
    [self.scroll.contentView scrollToPoint:NSMakePoint(0, MIN(previousOrigin.y, maximumY))];
    [self.scroll reflectScrolledClipView:self.scroll.contentView];
    if (restoreRetryFocus && automaticDisclosure) [self.window makeFirstResponder:automaticDisclosure];
}
@end
