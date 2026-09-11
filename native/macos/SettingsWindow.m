#import "SettingsWindow.h"
#import "Localization.h"
#import "ContentZoom.h"

static NSTextField *SettingsText(CGFloat size, NSFontWeight weight, NSColor *color) {
    NSTextField *field = [NSTextField wrappingLabelWithString:@""];
    field.font = [NSFont systemFontOfSize:size weight:weight];
    field.textColor = color;
    field.translatesAutoresizingMaskIntoConstraints = NO;
    [field setContentCompressionResistancePriority:NSLayoutPriorityRequired forOrientation:NSLayoutConstraintOrientationVertical];
    return field;
}
static NSStackView *SettingsStack(void) {
    NSStackView *stack = [NSStackView new];
    stack.orientation = NSUserInterfaceLayoutOrientationVertical;
    stack.alignment = NSLayoutAttributeLeading;
    stack.spacing = 10;
    stack.translatesAutoresizingMaskIntoConstraints = NO;
    return stack;
}
static void SettingsFullWidth(NSStackView *stack, NSView *view) {
    [stack addArrangedSubview:view];
    [view.widthAnchor constraintEqualToAnchor:stack.widthAnchor constant:-(stack.edgeInsets.left + stack.edgeInsets.right)].active = YES;
}
@interface JasoSettingsGroup : NSView
@end
@implementation JasoSettingsGroup
- (void)drawRect:(NSRect)dirtyRect {
    NSBezierPath *outline = [NSBezierPath bezierPathWithRoundedRect:NSInsetRect(self.bounds, .5, .5) xRadius:10 yRadius:10];
    [NSColor.controlBackgroundColor setFill]; [outline fill];
    [NSColor.separatorColor setStroke]; outline.lineWidth = .5; [outline stroke];
}
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; self.needsDisplay = YES; }
@end
static NSView *SettingsGroup(NSStackView *stack) {
    NSView *group = [JasoSettingsGroup new]; group.translatesAutoresizingMaskIntoConstraints = NO;
    [group addSubview:stack];
    [NSLayoutConstraint activateConstraints:@[
        [stack.topAnchor constraintEqualToAnchor:group.topAnchor constant:16],
        [stack.bottomAnchor constraintEqualToAnchor:group.bottomAnchor constant:-16],
        [stack.leadingAnchor constraintEqualToAnchor:group.leadingAnchor constant:16],
        [stack.trailingAnchor constraintEqualToAnchor:group.trailingAnchor constant:-16]
    ]];
    return group;
}
@interface JasoSettingsBackground : NSView
@end
@implementation JasoSettingsBackground
- (void)drawRect:(NSRect)dirtyRect { [NSColor.windowBackgroundColor setFill]; NSRectFill(dirtyRect); }
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; self.needsDisplay = YES; }
@end

@interface JasoSettingsWindowController ()
@property NSScrollView *scroll;
@property NSStackView *body;
@property NSTextField *headingLabel;
@property NSTextField *foldersLabel;
@property NSTextField *foldersGuide;
@property NSButton *foldersButton;
@property NSTextField *languageLabel;
@property NSTextField *explanationLabel;
@property NSPopUpButton *languagePicker;
@property NSTextField *appearanceLabel;
@property NSTextField *styleLabel;
@property NSPopUpButton *stylePicker;
@property NSButton *animationToggle;
@property NSTextField *animationGuide;
@property NSTextField *startupLabel;
@property NSButton *startupToggle;
@property NSTextField *startupStatus;
@property NSNumber *startupEnabled;
@property NSString *startupError;
@property BOOL startupBusy;
@property NSTextField *permissionsLabel;
@property NSTextField *loginGuide;
@property NSTextField *diskLabel;
@property NSTextField *diskGuide;
@property NSButton *loginButton;
@property NSButton *diskButton;
@property NSButton *revealButton;
@end

@implementation JasoSettingsWindowController
- (instancetype)init {
    NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 560, 640)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskResizable
        backing:NSBackingStoreBuffered defer:NO];
    if (!(self = [super initWithWindow:window])) return nil;
    window.releasedWhenClosed = NO; window.delegate = self; window.minSize = NSMakeSize(560, 520); [window center];
    [NSUserDefaults.standardUserDefaults registerDefaults:@{@"animateMark":@YES, @"markStyle":@1}];
    window.contentView = [[JasoSettingsBackground alloc] initWithFrame:window.contentView.bounds];
    [window.contentView.widthAnchor constraintGreaterThanOrEqualToConstant:560].active = YES;
    [window.contentView.widthAnchor constraintEqualToAnchor:((NSLayoutGuide *)window.contentLayoutGuide).widthAnchor].active = YES;
    self.scroll = [NSScrollView new]; self.scroll.translatesAutoresizingMaskIntoConstraints = NO;
    self.scroll.hasVerticalScroller = YES; self.scroll.autohidesScrollers = YES; self.scroll.drawsBackground = NO;
    [window.contentView addSubview:self.scroll];
    self.body = SettingsStack(); self.body.spacing = 20; self.body.edgeInsets = NSEdgeInsetsMake(24, 28, 24, 28);
    self.scroll.documentView = self.body;
    [NSLayoutConstraint activateConstraints:@[
        [self.scroll.topAnchor constraintEqualToAnchor:window.contentView.topAnchor],
        [self.scroll.bottomAnchor constraintEqualToAnchor:window.contentView.bottomAnchor],
        [self.scroll.leadingAnchor constraintEqualToAnchor:window.contentView.leadingAnchor],
        [self.scroll.trailingAnchor constraintEqualToAnchor:window.contentView.trailingAnchor],
        [self.body.widthAnchor constraintEqualToAnchor:self.scroll.contentView.widthAnchor],
        [self.body.leadingAnchor constraintEqualToAnchor:self.scroll.contentView.leadingAnchor],
        [self.body.topAnchor constraintEqualToAnchor:self.scroll.contentView.topAnchor]
    ]];
    self.headingLabel = SettingsText(22, NSFontWeightSemibold, NSColor.labelColor);
    self.headingLabel.identifier = @"settings-heading"; SettingsFullWidth(self.body, self.headingLabel);
    NSStackView *folders = SettingsStack();
    self.foldersLabel = SettingsText(15, NSFontWeightSemibold, NSColor.labelColor);
    self.foldersGuide = SettingsText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    self.foldersButton = [NSButton buttonWithTitle:@"" target:self action:@selector(permissionsAction:)];
    self.foldersButton.identifier = @"setup-folders";
    for (NSView *view in @[self.foldersLabel, self.foldersGuide, self.foldersButton]) SettingsFullWidth(folders, view);
    SettingsFullWidth(self.body, SettingsGroup(folders));
    NSStackView *language = SettingsStack();
    self.languageLabel = SettingsText(13, NSFontWeightMedium, NSColor.labelColor); self.languageLabel.identifier = @"interface-language-label";
    self.languagePicker = [[NSPopUpButton alloc] initWithFrame:NSZeroRect pullsDown:NO];
    self.languagePicker.identifier = @"interface-language-picker"; self.languagePicker.target = self; self.languagePicker.action = @selector(changeLanguage:);
    // Language choices do not use the window's zoom-command validation.
    self.languagePicker.autoenablesItems = NO;
    for (NSString *value in @[@"system", @"ko", @"en"]) { [self.languagePicker addItemWithTitle:value]; self.languagePicker.lastItem.representedObject = value; }
    self.explanationLabel = SettingsText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); self.explanationLabel.identifier = @"interface-language-explanation";
    for (NSView *view in @[self.languageLabel, self.languagePicker, self.explanationLabel]) SettingsFullWidth(language, view);
    SettingsFullWidth(self.body, SettingsGroup(language));

    self.appearanceLabel = SettingsText(15, NSFontWeightSemibold, NSColor.labelColor); SettingsFullWidth(self.body, self.appearanceLabel);
    NSStackView *appearance = SettingsStack();
    self.styleLabel = SettingsText(13, NSFontWeightMedium, NSColor.labelColor);
    self.stylePicker = [[NSPopUpButton alloc] initWithFrame:NSZeroRect pullsDown:NO];
    self.stylePicker.identifier = @"mark-style"; self.stylePicker.target = self; self.stylePicker.action = @selector(changeStyle:); self.stylePicker.autoenablesItems = NO;
    for (NSInteger i = 0; i < 3; i++) { [self.stylePicker addItemWithTitle:[NSString stringWithFormat:@"%ld", (long)i]]; self.stylePicker.lastItem.tag = i; }
    self.animationToggle = [NSButton checkboxWithTitle:@"" target:self action:@selector(changeAnimation:)]; self.animationToggle.identifier = @"animate-mark";
    self.animationGuide = SettingsText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    for (NSView *view in @[self.styleLabel, self.stylePicker, self.animationToggle, self.animationGuide]) SettingsFullWidth(appearance, view);
    SettingsFullWidth(self.body, SettingsGroup(appearance));

    self.startupLabel = SettingsText(15, NSFontWeightSemibold, NSColor.labelColor); SettingsFullWidth(self.body, self.startupLabel);
    NSStackView *startup = SettingsStack();
    self.startupToggle = [NSButton checkboxWithTitle:@"" target:self action:@selector(changeStartup:)]; self.startupToggle.identifier = @"run-at-login"; self.startupToggle.allowsMixedState = YES;
    self.startupStatus = SettingsText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); self.startupStatus.identifier = @"startup-status";
    self.loginGuide = SettingsText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    self.loginButton = [NSButton buttonWithTitle:@"" target:self action:@selector(permissionsAction:)]; self.loginButton.identifier = @"login-items";
    for (NSView *view in @[self.startupToggle, self.startupStatus, self.loginGuide, self.loginButton]) SettingsFullWidth(startup, view);
    SettingsFullWidth(self.body, SettingsGroup(startup));

    self.permissionsLabel = SettingsText(15, NSFontWeightSemibold, NSColor.labelColor); SettingsFullWidth(self.body, self.permissionsLabel);
    NSStackView *permissions = SettingsStack();
    self.diskLabel = SettingsText(13, NSFontWeightMedium, NSColor.labelColor);
    self.diskGuide = SettingsText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); self.diskGuide.identifier = @"full-disk-access-guide"; self.diskGuide.selectable = YES;
    self.diskButton = [NSButton buttonWithTitle:@"" target:self action:@selector(permissionsAction:)]; self.diskButton.identifier = @"full-disk-access";
    self.revealButton = [NSButton buttonWithTitle:@"" target:self action:@selector(permissionsAction:)]; self.revealButton.identifier = @"reveal-app";
    for (NSView *view in @[self.diskLabel, self.diskGuide, self.diskButton, self.revealButton]) SettingsFullWidth(permissions, view);
    SettingsFullWidth(self.body, SettingsGroup(permissions));
    window.initialFirstResponder = self.foldersButton;
    [self.languagePicker setAccessibilityTitleUIElement:self.languageLabel]; [self.stylePicker setAccessibilityTitleUIElement:self.styleLabel];
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(contentZoomChanged:) name:JasoContentZoomDidChangeNotification object:nil];
    [self reloadLocalization];
    return self;
}
- (void)reloadLocalization {
    self.window.title = JasoText(@"Jaso NFC · Settings", @"Jaso NFC · 설정");
    self.headingLabel.stringValue = JasoText(@"Settings", @"설정");
    self.foldersLabel.stringValue = JasoText(@"Folders and automatic cleanup", @"정리할 폴더와 자동 정리");
    self.foldersGuide.stringValue = JasoText(@"Choose folders, review proposed filename changes, and start automatic cleanup.", @"폴더를 선택하고 바뀔 파일명을 확인한 뒤 자동 정리를 시작하세요.");
    self.foldersButton.title = JasoText(@"Manage folders…", @"정리할 폴더…");
    self.languageLabel.stringValue = JasoText(@"Language", @"언어");
    self.explanationLabel.stringValue = JasoText(@"Applies immediately to menus and windows. Your choice is saved for the next launch.", @"선택한 언어를 메뉴와 창에 바로 적용합니다.");
    [self.languagePicker itemAtIndex:0].title = JasoText(@"System setting", @"시스템 설정");
    [self.languagePicker itemAtIndex:1].title = @"한국어"; [self.languagePicker itemAtIndex:2].title = @"English";
    for (NSMenuItem *item in self.languagePicker.itemArray) if ([item.representedObject isEqual:JasoLanguagePreference()]) [self.languagePicker selectItem:item];
    self.languagePicker.accessibilityLabel = self.languageLabel.stringValue; self.languagePicker.accessibilityHelp = self.explanationLabel.stringValue;
    self.appearanceLabel.stringValue = JasoText(@"Menu bar appearance", @"메뉴 막대 모양");
    self.styleLabel.stringValue = JasoText(@"Icon theme", @"아이콘 테마");
    NSArray *themes = @[JasoText(@"Quiet Gothic", @"단정한 고딕"), JasoText(@"Bookish Myeongjo", @"책 속의 명조"), JasoText(@"Brushstroke Gungseo", @"한 획의 궁서")];
    for (NSUInteger i = 0; i < themes.count; i++) { NSMenuItem *item = [self.stylePicker itemAtIndex:i]; item.title = themes[i]; item.enabled = self.availableStyles.count != 3 || [self.availableStyles[i] boolValue]; }
    [self.stylePicker selectItemAtIndex:MAX(0, MIN(2, [NSUserDefaults.standardUserDefaults integerForKey:@"markStyle"]))];
    self.stylePicker.accessibilityLabel = self.styleLabel.stringValue;
    self.animationToggle.title = JasoText(@"Animate menu bar icon", @"메뉴 막대 아이콘 애니메이션");
    self.animationToggle.state = [NSUserDefaults.standardUserDefaults boolForKey:@"animateMark"] ? NSControlStateValueOn : NSControlStateValueOff;
    self.animationGuide.stringValue = JasoText(@"Slowly joins the letters. Reduce Motion keeps the icon still.", @"자소가 천천히 합쳐지는 모습을 보여줍니다. 시스템의 ‘동작 줄이기’ 설정도 함께 적용합니다.");
    self.startupLabel.stringValue = JasoText(@"Startup", @"시작 설정");
    self.startupToggle.title = JasoText(@"Run at login", @"로그인할 때 실행");
    self.loginGuide.stringValue = JasoText(@"Allow Jaso NFC background activity in Login Items to run it after you sign in.", @"로그인 후 자동으로 실행하려면 로그인 항목에서 Jaso NFC의 백그라운드 실행을 허용하세요.");
    self.loginButton.title = JasoText(@"Open Login Items…", @"로그인 항목 열기…");
    self.permissionsLabel.stringValue = JasoText(@"Access permissions", @"접근 권한");
    self.diskLabel.stringValue = JasoText(@"Full Disk Access", @"전체 디스크 접근 권한");
    self.diskGuide.stringValue = JasoText(@"Add /Applications/Jaso NFC.app with “+” and turn it on. If Open does not add it, drag the app from Finder into the list. Then choose Restart worker in Status and check for inaccessible folders.", @"‘+’를 눌러 /Applications/Jaso NFC.app을 추가하고 허용하세요. Finder에서 앱을 목록으로 끌어 넣어도 됩니다. 설정을 마친 뒤 상태 창에서 ‘작업 다시 시작’을 누르세요.");
    self.diskButton.title = JasoText(@"Open Full Disk Access…", @"전체 디스크 접근 권한 열기…");
    self.revealButton.title = JasoText(@"Show installed app in Finder", @"Finder에서 설치된 앱 보기");
    [self updateStartupEnabled:self.startupEnabled error:self.startupError busy:self.startupBusy];
    [self applyContentZoom];
}
- (void)setAvailableStyles:(NSArray<NSNumber *> *)availableStyles { _availableStyles = [availableStyles copy]; [self reloadLocalization]; }
- (void)updateStartupEnabled:(NSNumber *)enabled error:(NSString *)error busy:(BOOL)busy {
    self.startupEnabled = enabled; self.startupError = error; self.startupBusy = busy;
    self.startupToggle.allowsMixedState = enabled == nil;
    self.startupToggle.state = enabled ? (enabled.boolValue ? NSControlStateValueOn : NSControlStateValueOff) : NSControlStateValueMixed;
    self.startupToggle.enabled = enabled != nil && error == nil && !busy;
    self.startupStatus.stringValue = busy ? JasoText(@"Checking startup…", @"시작 설정 확인 중…") : error.length ? [NSString stringWithFormat:JasoText(@"Startup unavailable: %@", @"시작 설정 확인 불가: %@"), error] : !enabled ? JasoText(@"Open Settings again to check startup.", @"설정을 다시 열어 로그인 실행 상태를 확인하세요.") : enabled.boolValue ? JasoText(@"Starts automatically after you sign in.", @"로그인 후 자동으로 시작합니다.") : JasoText(@"Open Jaso NFC when you want to start it.", @"사용할 때 응용 프로그램 폴더에서 Jaso NFC를 여세요.");
    self.startupStatus.textColor = error.length ? NSColor.systemOrangeColor : NSColor.secondaryLabelColor;
}
- (void)changeStartup:(NSButton *)sender {
    if (!sender.enabled || !self.startupEnabled || !self.startupChangedHandler) return;
    BOOL enabled = sender.state == NSControlStateValueOn;
    [self updateStartupEnabled:self.startupEnabled error:nil busy:YES];
    self.startupChangedHandler(enabled);
}
- (void)changeAnimation:(NSButton *)sender {
    [NSUserDefaults.standardUserDefaults setBool:sender.state == NSControlStateValueOn forKey:@"animateMark"];
    if (self.appearanceChangedHandler) self.appearanceChangedHandler();
}
- (void)changeStyle:(NSPopUpButton *)sender {
    if (!sender.selectedItem.enabled) return;
    [NSUserDefaults.standardUserDefaults setInteger:sender.selectedItem.tag forKey:@"markStyle"];
    if (self.appearanceChangedHandler) self.appearanceChangedHandler();
}
- (CGFloat)contentZoom { return JasoContentZoom(); }
- (void)zoomIn:(id)sender { JasoSetContentZoom(self.contentZoom + .1); }
- (void)zoomOut:(id)sender { JasoSetContentZoom(self.contentZoom - .1); }
- (void)resetZoom:(id)sender { JasoSetContentZoom(1); }
- (BOOL)validateUserInterfaceItem:(id<NSValidatedUserInterfaceItem>)item { return JasoValidateContentZoomAction(self.window, item.action); }
- (void)contentZoomChanged:(NSNotification *)notification { [self applyContentZoom]; }
- (void)applyContentZoom {
    NSPoint previousOrigin = self.scroll.contentView.bounds.origin;
    JasoApplyContentZoom(self.body); [self.window.contentView layoutSubtreeIfNeeded];
    CGFloat maximumY = MAX(0, self.body.frame.size.height - self.scroll.contentView.bounds.size.height);
    [self.scroll.contentView scrollToPoint:NSMakePoint(0, MIN(previousOrigin.y, maximumY))]; [self.scroll reflectScrolledClipView:self.scroll.contentView];
}
- (void)dealloc { [NSNotificationCenter.defaultCenter removeObserver:self]; }
- (void)changeLanguage:(NSPopUpButton *)sender {
    NSString *language = sender.selectedItem.representedObject;
    if (![@[@"system", @"ko", @"en"] containsObject:language]) return;
    JasoSetLanguagePreference(language); [self reloadLocalization]; if (self.languageChangedHandler) self.languageChangedHandler();
}
- (void)permissionsAction:(NSButton *)sender { if (self.actionHandler) self.actionHandler([sender.identifier isEqual:@"setup-folders"] ? @"setup" : sender.identifier); }
- (void)showWindow:(id)sender { [self reloadLocalization]; [super showWindow:sender]; [NSApp activateIgnoringOtherApps:YES]; [self.window makeKeyAndOrderFront:nil]; }
- (void)windowWillClose:(NSNotification *)notification { if (self.closeHandler) self.closeHandler(); }
@end
