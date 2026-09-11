#import "SetupWindow.h"
#import "Localization.h"
#import "ContentZoom.h"

static NSTextField *SetupText(CGFloat size, NSFontWeight weight, NSColor *color) {
    NSTextField *field = [NSTextField wrappingLabelWithString:@""];
    field.font = [NSFont systemFontOfSize:size weight:weight]; field.textColor = color;
    field.translatesAutoresizingMaskIntoConstraints = NO;
    [field setContentCompressionResistancePriority:NSLayoutPriorityRequired forOrientation:NSLayoutConstraintOrientationVertical];
    return field;
}
static NSStackView *SetupStack(void) {
    NSStackView *stack = [NSStackView new]; stack.orientation = NSUserInterfaceLayoutOrientationVertical;
    stack.alignment = NSLayoutAttributeLeading; stack.spacing = 10; stack.translatesAutoresizingMaskIntoConstraints = NO;
    return stack;
}
static void SetupFullWidth(NSStackView *stack, NSView *view) {
    [stack addArrangedSubview:view]; view.translatesAutoresizingMaskIntoConstraints = NO;
    [view.widthAnchor constraintEqualToAnchor:stack.widthAnchor constant:-(stack.edgeInsets.left + stack.edgeInsets.right)].active = YES;
}
@interface JasoSetupDocument : NSStackView
@end
@implementation JasoSetupDocument
- (BOOL)isFlipped { return YES; }
@end
@interface JasoSetupBackground : NSView
@property BOOL card;
@end
@implementation JasoSetupBackground
- (void)drawRect:(NSRect)dirtyRect {
    if (!self.card) { [NSColor.windowBackgroundColor setFill]; NSRectFill(dirtyRect); return; }
    NSBezierPath *path = [NSBezierPath bezierPathWithRoundedRect:NSInsetRect(self.bounds, .5, .5) xRadius:10 yRadius:10];
    [NSColor.controlBackgroundColor setFill]; [path fill]; [NSColor.separatorColor setStroke]; path.lineWidth = .5; [path stroke];
}
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; self.needsDisplay = YES; }
@end
static NSView *SetupCard(NSStackView *stack) {
    JasoSetupBackground *view = [JasoSetupBackground new]; view.card = YES; view.translatesAutoresizingMaskIntoConstraints = NO;
    [view addSubview:stack];
    [NSLayoutConstraint activateConstraints:@[
        [stack.leadingAnchor constraintEqualToAnchor:view.leadingAnchor constant:16], [stack.trailingAnchor constraintEqualToAnchor:view.trailingAnchor constant:-16],
        [stack.topAnchor constraintEqualToAnchor:view.topAnchor constant:16], [stack.bottomAnchor constraintEqualToAnchor:view.bottomAnchor constant:-16]
    ]];
    return view;
}
static BOOL SetupStrings(id value) {
    if (![value isKindOfClass:NSArray.class]) return NO;
    for (id item in value) if (![item isKindOfClass:NSString.class]) return NO;
    return YES;
}

@interface JasoSetupWindowController ()
@property (readwrite, copy) NSString *revision;
@property NSMutableArray<NSString *> *roots;
@property NSMutableArray<NSString *> *excludes;
@property NSString *scope;
@property BOOL apply;
@property NSDictionary *confirmedDraft;
@property NSDictionary *previewDraft;
@property NSDictionary *previewResult;
@property BOOL busy;
@property BOOL cancellable;
@property NSString *operation;
@property NSString *messageText;
@property NSString *notice;
@property BOOL messageIsError;
@property NSScrollView *scroll;
@property NSStackView *body;
@property NSStackView *footer;
@property NSTextField *heading;
@property NSTextField *introduction;
@property NSTextField *scopeHeading;
@property NSPopUpButton *scopePicker;
@property NSTextField *scopeGuide;
@property NSTableView *foldersTable;
@property NSButton *addFolder;
@property NSButton *removeFolder;
@property NSButton *excludesDisclosure;
@property NSStackView *excludesContent;
@property BOOL excludesExpanded;
@property NSTextField *excludesGuide;
@property NSTableView *excludesTable;
@property NSButton *addExclude;
@property NSButton *removeExclude;
@property NSTextField *modeHeading;
@property NSButton *moreSettings;
@property NSStackView *modeContent;
@property BOOL modeExpanded;
@property NSPopUpButton *modePicker;
@property NSTextField *modeGuide;
@property NSTextField *previewHeading;
@property NSButton *previewButton;
@property NSButton *cancelButton;
@property NSTextField *previewSummary;
@property NSStackView *previewRows;
@property NSTextField *message;
@property NSProgressIndicator *progress;
@property NSButton *reloadButton;
@property NSButton *saveButton;
@property NSButton *startButton;
@end

@implementation JasoSetupWindowController
- (instancetype)init {
    NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 640, 760)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskResizable backing:NSBackingStoreBuffered defer:NO];
    if (!(self = [super initWithWindow:window])) return nil;
    window.releasedWhenClosed = NO; window.delegate = self; window.minSize = NSMakeSize(560, 560); [window center];
    self.roots = [NSMutableArray array]; self.excludes = [NSMutableArray array]; self.scope = @"configured";
    window.contentView = [[JasoSetupBackground alloc] initWithFrame:window.contentView.bounds];
    self.scroll = [NSScrollView new]; self.scroll.translatesAutoresizingMaskIntoConstraints = NO;
    self.scroll.hasVerticalScroller = YES; self.scroll.autohidesScrollers = YES; self.scroll.drawsBackground = NO;
    [window.contentView addSubview:self.scroll];
    self.body = [JasoSetupDocument new]; self.body.orientation = NSUserInterfaceLayoutOrientationVertical;
    self.body.alignment = NSLayoutAttributeLeading; self.body.translatesAutoresizingMaskIntoConstraints = NO;
    self.body.spacing = 20; self.body.edgeInsets = NSEdgeInsetsMake(24, 28, 24, 28);
    self.scroll.documentView = self.body;
    self.footer = SetupStack(); self.footer.spacing = 8;
    [window.contentView addSubview:self.footer];
    [NSLayoutConstraint activateConstraints:@[
        [self.scroll.topAnchor constraintEqualToAnchor:window.contentView.topAnchor], [self.scroll.leadingAnchor constraintEqualToAnchor:window.contentView.leadingAnchor],
        [self.scroll.trailingAnchor constraintEqualToAnchor:window.contentView.trailingAnchor], [self.scroll.bottomAnchor constraintEqualToAnchor:self.footer.topAnchor constant:-12],
        [self.body.widthAnchor constraintEqualToAnchor:self.scroll.contentView.widthAnchor], [self.body.leadingAnchor constraintEqualToAnchor:self.scroll.contentView.leadingAnchor],
        [self.body.topAnchor constraintEqualToAnchor:self.scroll.contentView.topAnchor],
        [self.footer.leadingAnchor constraintEqualToAnchor:window.contentView.leadingAnchor constant:28], [self.footer.trailingAnchor constraintEqualToAnchor:window.contentView.trailingAnchor constant:-28],
        [self.footer.bottomAnchor constraintEqualToAnchor:window.contentView.bottomAnchor constant:-20]
    ]];
    NSStackView *intro = SetupStack(); intro.spacing = 8;
    self.heading = SetupText(24, NSFontWeightSemibold, NSColor.labelColor); self.heading.identifier = @"setup-heading";
    self.introduction = SetupText(13, NSFontWeightRegular, NSColor.secondaryLabelColor);
    SetupFullWidth(intro, self.heading); SetupFullWidth(intro, self.introduction); SetupFullWidth(self.body, intro);

    NSStackView *folders = SetupStack();
    self.scopeHeading = SetupText(15, NSFontWeightSemibold, NSColor.labelColor);
    self.scopePicker = [self picker:@"setup-scope" choices:@[@"configured", @"all-user-files"] action:@selector(changeScope:)];
    self.scopeGuide = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    for (NSView *view in @[self.scopeHeading, self.scopePicker, self.scopeGuide]) SetupFullWidth(folders, view);
    self.foldersTable = [self table:@"setup-folders"];
    SetupFullWidth(folders, self.foldersTable.enclosingScrollView);
    self.addFolder = [self button:@"setup-add-folder" action:@selector(chooseFolders:)];
    self.removeFolder = [self button:@"setup-remove-folder" action:@selector(removeFolders:)];
    SetupFullWidth(folders, [self buttonRow:self.addFolder second:self.removeFolder]);
    self.excludesDisclosure = [self button:@"setup-exclusions-toggle" action:@selector(toggleExclusions:)];
    self.excludesDisclosure.bordered = NO; self.excludesDisclosure.alignment = NSTextAlignmentLeft; self.excludesDisclosure.imagePosition = NSImageLeft;
    SetupFullWidth(folders, self.excludesDisclosure);
    self.excludesContent = SetupStack();
    self.excludesGuide = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    SetupFullWidth(self.excludesContent, self.excludesGuide);
    self.excludesTable = [self table:@"setup-excludes"]; SetupFullWidth(self.excludesContent, self.excludesTable.enclosingScrollView);
    self.addExclude = [self button:@"setup-add-exclude" action:@selector(chooseFolders:)];
    self.removeExclude = [self button:@"setup-remove-exclude" action:@selector(removeFolders:)];
    SetupFullWidth(self.excludesContent, [self buttonRow:self.addExclude second:self.removeExclude]);
    SetupFullWidth(folders, self.excludesContent);
    SetupFullWidth(self.body, SetupCard(folders));

    NSStackView *mode = SetupStack(); self.modeContent = mode;
    self.modeHeading = SetupText(15, NSFontWeightSemibold, NSColor.labelColor);
    self.modePicker = [self picker:@"setup-mode" choices:@[@"preview", @"automatic"] action:@selector(changeMode:)];
    self.modeGuide = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    for (NSView *view in @[self.modeHeading, self.modePicker, self.modeGuide]) SetupFullWidth(mode, view);
    NSStackView *additional = SetupStack();
    self.moreSettings = [self button:@"setup-more-settings" action:@selector(toggleMoreSettings:)];
    self.moreSettings.bordered = NO; self.moreSettings.alignment = NSTextAlignmentLeft; self.moreSettings.imagePosition = NSImageLeft;
    SetupFullWidth(additional, self.moreSettings); SetupFullWidth(additional, mode);
    NSView *modeCard = SetupCard(additional);

    NSStackView *preview = SetupStack();
    self.previewHeading = SetupText(15, NSFontWeightSemibold, NSColor.labelColor);
    self.previewButton = [self button:@"setup-preview" action:@selector(requestPreview:)];
    self.cancelButton = [self button:@"setup-cancel" action:@selector(cancelPreview:)]; self.cancelButton.keyEquivalent = @"\e";
    self.previewSummary = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); self.previewSummary.identifier = @"setup-preview-summary";
    self.previewRows = SetupStack(); self.previewRows.spacing = 14;
    SetupFullWidth(preview, self.previewHeading);
    SetupFullWidth(preview, self.previewSummary); SetupFullWidth(preview, self.previewRows);
    SetupFullWidth(self.body, SetupCard(preview));
    SetupFullWidth(self.body, modeCard);
    self.reloadButton = [self button:@"setup-reload" action:@selector(requestReload:)]; SetupFullWidth(self.body, self.reloadButton);

    self.message = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); self.message.identifier = @"setup-message"; self.message.selectable = YES;
    self.progress = [NSProgressIndicator new]; self.progress.style = NSProgressIndicatorStyleSpinning; self.progress.controlSize = NSControlSizeSmall;
    self.progress.displayedWhenStopped = NO; self.progress.translatesAutoresizingMaskIntoConstraints = NO;
    [self.progress.widthAnchor constraintEqualToConstant:16].active = YES; [self.progress.heightAnchor constraintEqualToConstant:16].active = YES;
    NSStackView *status = [NSStackView stackViewWithViews:@[self.progress, self.message]]; status.orientation = NSUserInterfaceLayoutOrientationHorizontal;
    status.alignment = NSLayoutAttributeCenterY; status.spacing = 8;
    SetupFullWidth(self.footer, status);
    SetupFullWidth(self.footer, [self buttonRow:self.previewButton second:self.cancelButton]);
    self.saveButton = [self button:@"setup-save" action:@selector(requestSave:)]; self.startButton = [self button:@"setup-start" action:@selector(requestSave:)];
    self.startButton.bezelColor = NSColor.controlAccentColor;
    SetupFullWidth(self.footer, self.saveButton); SetupFullWidth(self.footer, self.startButton);
    window.initialFirstResponder = self.scopePicker;
    [self.scopePicker setAccessibilityTitleUIElement:self.scopeHeading]; [self.modePicker setAccessibilityTitleUIElement:self.modeHeading];
    [NSNotificationCenter.defaultCenter addObserver:self selector:@selector(contentZoomChanged:) name:JasoContentZoomDidChangeNotification object:nil];
    [self reloadLocalization];
    return self;
}
- (NSButton *)button:(NSString *)identifier action:(SEL)action {
    NSButton *button = [NSButton buttonWithTitle:@"" target:self action:action]; button.identifier = identifier; return button;
}
- (NSPopUpButton *)picker:(NSString *)identifier choices:(NSArray<NSString *> *)choices action:(SEL)action {
    NSPopUpButton *picker = [[NSPopUpButton alloc] initWithFrame:NSZeroRect pullsDown:NO]; picker.identifier = identifier; picker.target = self; picker.action = action;
    // refreshControls gates the whole picker; choices do not use zoom-command validation.
    picker.autoenablesItems = NO;
    for (NSString *choice in choices) { [picker addItemWithTitle:choice]; picker.lastItem.representedObject = choice; }
    return picker;
}
- (NSStackView *)buttonRow:(NSButton *)first second:(NSButton *)second {
    NSStackView *row = [NSStackView stackViewWithViews:@[first, second]]; row.orientation = NSUserInterfaceLayoutOrientationHorizontal;
    row.distribution = NSStackViewDistributionFillEqually; row.spacing = 8; first.translatesAutoresizingMaskIntoConstraints = NO; second.translatesAutoresizingMaskIntoConstraints = NO;
    return row;
}
- (NSTableView *)table:(NSString *)identifier {
    NSScrollView *scroll = [[NSScrollView alloc] initWithFrame:NSMakeRect(0, 0, 480, 88)]; scroll.translatesAutoresizingMaskIntoConstraints = NO;
    scroll.hasVerticalScroller = YES; scroll.autohidesScrollers = YES; scroll.borderType = NSBezelBorder;
    [scroll.heightAnchor constraintEqualToConstant:88].active = YES;
    NSTableView *table = [[NSTableView alloc] initWithFrame:scroll.contentView.bounds]; table.identifier = identifier;
    table.headerView = nil; table.allowsMultipleSelection = YES; table.allowsEmptySelection = YES; table.rowSizeStyle = NSTableViewRowSizeStyleCustom;
    table.style = NSTableViewStyleFullWidth; table.intercellSpacing = NSMakeSize(0, 4);
    table.usesAlternatingRowBackgroundColors = YES; table.columnAutoresizingStyle = NSTableViewUniformColumnAutoresizingStyle;
    NSTableColumn *column = [[NSTableColumn alloc] initWithIdentifier:@"path"]; column.resizingMask = NSTableColumnAutoresizingMask; column.width = 460; column.minWidth = 0;
    [table addTableColumn:column]; table.dataSource = self; table.delegate = self; table.autoresizingMask = NSViewWidthSizable;
    scroll.documentView = table;
    // Retain the scroll view until it is attached; NSTableView's enclosure is weak.
    [self.body addSubview:scroll];
    return table;
}
- (NSDictionary *)draft { return @{@"scope":self.scope ?: @"configured", @"roots":[self.scope isEqual:@"all-user-files"] ? @[] : [self.roots copy] ?: @[], @"excludes":[self.excludes copy] ?: @[], @"apply":@(self.apply)}; }
- (NSDictionary *)editableConfig:(NSDictionary *)result {
    if (![result isKindOfClass:NSDictionary.class]) return nil;
    NSDictionary *config = result[@"config"];
    if (![config isKindOfClass:NSDictionary.class] || ![@[@"configured", @"all-user-files"] containsObject:config[@"scope"] ?: @""] || !SetupStrings(config[@"roots"]) || !SetupStrings(config[@"excludes"]) || ![config[@"apply"] isKindOfClass:NSNumber.class] || ![result[@"revision"] isKindOfClass:NSString.class] || ![result[@"revision"] length]) return nil;
    return @{@"scope":config[@"scope"], @"roots":[config[@"roots"] copy], @"excludes":[config[@"excludes"] copy], @"apply":@([config[@"apply"] boolValue])};
}
- (void)loadConfirmed:(NSDictionary *)config revision:(NSString *)revision {
    self.scope = config[@"scope"];
    if (![self.scope isEqual:@"all-user-files"] || [config[@"roots"] count]) self.roots = [config[@"roots"] mutableCopy];
    self.excludes = [config[@"excludes"] mutableCopy]; self.apply = [config[@"apply"] boolValue];
    self.confirmedDraft = config; self.revision = revision;
    [self.scopePicker selectItemAtIndex:[self.scope isEqual:@"all-user-files"] ? 1 : 0]; [self.modePicker selectItemAtIndex:self.apply ? 1 : 0];
    [self.foldersTable reloadData]; [self.excludesTable reloadData];
}
- (void)updateConfiguration:(NSDictionary *)result error:(NSString *)error {
    NSDictionary *config = [self editableConfig:result];
    [self setBusy:NO cancellable:NO];
    if (error.length || !config) { [self showError:error.length ? error : JasoText(@"Could not read saved folders. Choose Reload saved settings to try again.", @"저장된 폴더를 읽을 수 없습니다. ‘저장된 설정 다시 읽기’를 선택하세요.")]; return; }
    [self loadConfirmed:config revision:result[@"revision"]]; self.previewResult = nil; self.previewDraft = nil; self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    [self reloadLocalization];
}
- (void)updatePreview:(NSDictionary *)result error:(NSString *)error {
    if (!self.previewDraft || ![self previewMatchesScope]) return;
    [self setBusy:NO cancellable:NO];
    if (error.length || ![result[@"candidates"] isKindOfClass:NSArray.class] || ![result[@"errors"] isKindOfClass:NSArray.class]) {
        self.previewResult = nil; self.previewDraft = nil; [self renderPreview];
        [self showError:error.length ? error : JasoText(@"The preview could not finish. Check the folder paths and try Preview filenames again.", @"미리보기를 완료할 수 없습니다. 폴더 경로를 확인한 뒤 ‘파일명 미리보기’를 다시 선택하세요.")]; return;
    }
    if (result[@"revision"] && ![result[@"revision"] isEqual:self.revision]) {
        self.previewResult = nil; self.previewDraft = nil; [self renderPreview];
        [self showError:JasoText(@"Saved settings changed. Your selections are kept here; reload saved settings before reviewing again.", @"저장된 설정이 바뀌었습니다. 선택한 폴더는 이 창에 유지됩니다. 저장된 설정을 다시 읽은 뒤 미리보기를 진행하세요.")]; return;
    }
    self.previewResult = [result copy]; self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    [self renderPreview]; [self refreshControls]; [self applyContentZoom];
    [self revealPreview];
}
- (void)updateSave:(NSDictionary *)result error:(NSString *)error {
    NSDictionary *config = [self editableConfig:result]; [self setBusy:NO cancellable:NO];
    if (error.length || !config) { [self showError:error.length ? error : JasoText(@"Could not save the selected folders. Review the details and try again.", @"선택한 폴더를 저장할 수 없습니다. 안내 내용을 확인한 뒤 다시 시도하세요.")]; return; }
    BOOL changed = ![config isEqual:self.draft]; [self loadConfirmed:config revision:result[@"revision"]];
    if (changed) { self.previewResult = nil; self.previewDraft = nil; }
    self.messageIsError = NO;
    BOOL paused = [result[@"paused"] boolValue];
    BOOL automaticStarted = [result[@"started"] boolValue] && [config[@"apply"] boolValue] && !paused;
    self.messageText = nil; self.notice = paused ? @"paused" : automaticStarted ? @"started" : @"saved";
    [self reloadLocalization];
}
- (void)showError:(NSString *)error { self.messageText = error; self.notice = nil; self.messageIsError = YES; [self refreshControls]; [self.window.contentView layoutSubtreeIfNeeded]; }
- (BOOL)validDraft { return self.revision.length > 0 && ([self.scope isEqual:@"all-user-files"] || self.roots.count > 0); }
- (BOOL)previewMatchesScope {
    NSDictionary *draft = self.draft;
    for (NSString *key in @[@"scope", @"roots", @"excludes"]) if (![self.previewDraft[key] isEqual:draft[key]]) return NO;
    return YES;
}
- (BOOL)reviewedDraft { return self.previewResult != nil && [self previewMatchesScope]; }
- (void)refreshControls {
    BOOL ready = self.revision.length && !self.busy;
    BOOL selected = [self.scope isEqual:@"configured"];
    self.scopePicker.enabled = ready; self.modePicker.enabled = ready;
    self.excludesDisclosure.enabled = ready;
    self.moreSettings.enabled = ready;
    self.foldersTable.enabled = ready && selected; self.excludesTable.enabled = ready;
    self.addFolder.enabled = ready && selected; self.removeFolder.enabled = ready && selected && self.foldersTable.selectedRowIndexes.count;
    self.addExclude.enabled = ready; self.removeExclude.enabled = ready && self.excludesTable.selectedRowIndexes.count;
    self.previewButton.enabled = ready && [self validDraft]; self.cancelButton.enabled = self.busy && self.cancellable;
    self.reloadButton.enabled = !self.busy;
    self.saveButton.enabled = ready && [self validDraft] && (!self.apply || [self reviewedDraft] || [self.draft isEqual:self.confirmedDraft]);
    self.startButton.enabled = ready && [self validDraft] && [self reviewedDraft];
    self.startButton.keyEquivalent = self.startButton.enabled ? @"\r" : @"";
    self.previewButton.keyEquivalent = !self.startButton.enabled && self.previewButton.enabled ? @"\r" : @"";
    if (self.busy) self.message.stringValue = [self.operation isEqual:@"cancel"] ? JasoText(@"Cancelling preview…", @"미리보기를 취소하고 있습니다…") : [self.operation isEqual:@"preview"] ? JasoText(@"Reviewing filenames…", @"파일명을 확인하고 있습니다…") : [self.operation isEqual:@"start"] ? JasoText(@"Saving folders and starting cleanup…", @"폴더를 저장하고 정리를 시작하고 있습니다…") : [self.operation isEqual:@"save"] ? JasoText(@"Saving folder settings…", @"폴더 설정을 저장하고 있습니다…") : JasoText(@"Reading saved folders…", @"저장된 폴더를 읽고 있습니다…");
    else if (self.messageText.length) self.message.stringValue = self.messageText;
    else if ([self.notice isEqual:@"started"]) self.message.stringValue = JasoText(@"Saved. Automatic cleanup has started for available folders.", @"저장했습니다. 사용할 수 있는 폴더에서 자동 정리를 시작했습니다.");
    else if ([self.notice isEqual:@"paused"]) self.message.stringValue = JasoText(@"Folders saved. Resume from the menu when you are ready.", @"폴더를 저장했습니다. 준비되면 메뉴에서 작업을 재개하세요.");
    else if ([self.notice isEqual:@"saved"]) self.message.stringValue = JasoText(@"Folder settings saved.", @"폴더 설정을 저장했습니다.");
    else if ([self.notice isEqual:@"cancelled"]) self.message.stringValue = JasoText(@"Preview cancelled. Your folder selections are kept.", @"미리보기를 취소했습니다. 선택한 폴더는 유지됩니다.");
    else if (!self.revision.length) self.message.stringValue = JasoText(@"Load your saved folder settings to begin.", @"저장된 폴더 설정을 불러와 시작하세요.");
    else if (![self validDraft]) self.message.stringValue = JasoText(@"Add a folder to preview filenames and save your setup.", @"파일명을 미리 확인하고 설정을 저장할 폴더를 추가하세요.");
    else if (![self reviewedDraft]) self.message.stringValue = JasoText(@"Preview these folders, then start automatic cleanup.", @"이 폴더의 파일명을 미리 확인한 뒤 자동 정리를 시작하세요.");
    else if ([self.previewResult[@"errors"] count]) self.message.stringValue = JasoText(@"Start cleanup in available folders. Open the listed locations in Finder to check the reported causes.", @"사용할 수 있는 폴더에서 정리를 시작하세요. 안내된 위치를 Finder에서 열어 원인을 확인하세요.");
    else self.message.stringValue = JasoText(@"Your preview is ready. Start automatic cleanup when you are ready.", @"미리보기를 준비했습니다. 준비되면 자동 정리를 시작하세요.");
    self.message.textColor = self.messageIsError ? NSColor.systemOrangeColor : NSColor.secondaryLabelColor;
}
- (void)setBusy:(BOOL)busy cancellable:(BOOL)cancellable {
    self.busy = busy; self.cancellable = busy && cancellable;
    if (busy) [self.progress startAnimation:nil]; else { [self.progress stopAnimation:nil]; self.operation = nil; }
    [self refreshControls];
}
- (void)draftChanged {
    self.previewResult = nil; self.previewDraft = nil; self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    [self renderPreview]; [self reloadLocalization];
}
- (void)changeScope:(NSPopUpButton *)sender {
    if (!sender.enabled) return; self.scope = sender.selectedItem.representedObject; [self draftChanged];
}
- (void)changeMode:(NSPopUpButton *)sender {
    if (!sender.enabled) return;
    self.apply = [sender.selectedItem.representedObject isEqual:@"automatic"]; self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    [self reloadLocalization];
}
- (void)toggleExclusions:(NSButton *)sender {
    if (!sender.enabled) return; self.excludesExpanded = !self.excludesExpanded; [self reloadLocalization];
}
- (void)toggleMoreSettings:(NSButton *)sender {
    if (!sender.enabled) return; self.modeExpanded = !self.modeExpanded; [self reloadLocalization];
}
- (void)addFolderURLs:(NSArray<NSURL *> *)urls excluding:(BOOL)excluding {
    if (self.busy || !self.revision.length || (!excluding && ![self.scope isEqual:@"configured"])) return;
    NSMutableArray *paths = excluding ? self.excludes : self.roots;
    BOOL changed = NO;
    for (NSURL *url in urls) if (url.isFileURL) {
        NSString *path = url.path.stringByStandardizingPath;
        if (path.isAbsolutePath && ![paths containsObject:path]) { [paths addObject:path]; changed = YES; }
    }
    if (!changed) return;
    if (excluding) self.excludesExpanded = YES;
    [(excluding ? self.excludesTable : self.foldersTable) reloadData]; [self draftChanged];
}
- (void)chooseFolders:(NSButton *)sender {
    if (!sender.enabled) return;
    BOOL excluding = sender == self.addExclude;
    NSOpenPanel *panel = [NSOpenPanel openPanel]; panel.canChooseFiles = NO; panel.canChooseDirectories = YES;
    panel.allowsMultipleSelection = YES; panel.canCreateDirectories = NO;
    panel.message = excluding ? JasoText(@"Choose folders to keep out of filename cleanup.", @"파일명 정리에서 제외할 폴더를 선택하세요.") : JasoText(@"Choose the folders whose filenames you want to keep tidy.", @"파일명을 정리할 폴더를 선택하세요.");
    panel.prompt = JasoText(@"Add folders", @"폴더 추가");
    __weak typeof(self) weakSelf = self;
    [panel beginSheetModalForWindow:self.window completionHandler:^(NSModalResponse response) {
        if (response == NSModalResponseOK) [weakSelf addFolderURLs:panel.URLs excluding:excluding];
    }];
}
- (void)removeFolders:(NSButton *)sender {
    if (!sender.enabled) return;
    BOOL excluding = sender == self.removeExclude; NSTableView *table = excluding ? self.excludesTable : self.foldersTable;
    NSMutableArray *paths = excluding ? self.excludes : self.roots;
    NSIndexSet *selection = table.selectedRowIndexes;
    if (!selection.count || selection.lastIndex >= paths.count) return;
    [paths removeObjectsAtIndexes:selection]; [table deselectAll:nil]; [table reloadData]; [self draftChanged];
}
- (void)requestPreview:(NSButton *)sender {
    if (!sender.enabled || !self.previewHandler) return;
    self.previewDraft = self.draft; self.previewResult = nil; self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    self.operation = @"preview";
    [self renderPreview]; [self setBusy:YES cancellable:YES]; self.previewHandler(self.previewDraft);
}
- (void)cancelPreview:(id)sender {
    if (!self.busy || !self.cancellable) return;
    self.previewDraft = nil; self.previewResult = nil;
    self.messageText = nil; self.notice = @"cancelled"; self.messageIsError = NO;
    self.operation = @"cancel";
    [self setBusy:YES cancellable:NO]; [self renderPreview]; if (self.cancelHandler) self.cancelHandler();
}
- (void)requestSave:(NSButton *)sender {
    if (!sender.enabled || !self.saveHandler) return;
    BOOL start = sender == self.startButton;
    if (start) { self.apply = YES; [self.modePicker selectItemAtIndex:1]; }
    NSDictionary *draft = self.draft; NSString *revision = self.revision;
    self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    self.operation = start ? @"start" : @"save";
    [self setBusy:YES cancellable:NO]; self.saveHandler(draft, revision, start);
}
- (void)requestReload:(NSButton *)sender {
    if (!sender.enabled || !self.reloadHandler) return;
    self.previewDraft = nil; self.previewResult = nil; self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    [self renderPreview]; [self setBusy:YES cancellable:NO]; self.reloadHandler();
}
- (NSInteger)numberOfRowsInTableView:(NSTableView *)tableView { return (NSInteger)(tableView == self.foldersTable ? self.roots.count : self.excludes.count); }
- (NSView *)tableView:(NSTableView *)tableView viewForTableColumn:(NSTableColumn *)tableColumn row:(NSInteger)row {
    NSArray *paths = tableView == self.foldersTable ? self.roots : self.excludes;
    if (row < 0 || (NSUInteger)row >= paths.count) return nil;
    NSTextField *field = [tableView makeViewWithIdentifier:@"folder-path" owner:self];
    if (!field) { field = [NSTextField labelWithString:@""]; field.identifier = @"folder-path"; field.selectable = YES; field.lineBreakMode = NSLineBreakByTruncatingMiddle; }
    field.font = [NSFont systemFontOfSize:13 * self.contentZoom]; field.stringValue = paths[(NSUInteger)row]; field.toolTip = field.stringValue;
    field.accessibilityLabel = field.stringValue; return field;
}
- (void)tableViewSelectionDidChange:(NSNotification *)notification { [self refreshControls]; }
- (void)renderPreview {
    for (NSView *view in [self.previewRows.arrangedSubviews copy]) { [self.previewRows removeArrangedSubview:view]; [view removeFromSuperview]; }
    if (!self.previewResult) {
        self.previewSummary.stringValue = JasoText(@"Review a sample of original and cleaned-up filenames before starting.", @"정리를 시작하기 전에 원래 이름과 정리된 이름의 예시를 확인하세요."); return;
    }
    NSArray *candidates = self.previewResult[@"candidates"], *errors = self.previewResult[@"errors"], *interruptedChecks = self.previewResult[@"interrupted_checks"];
    NSUInteger shown = MIN(candidates.count, 8);
    BOOL timeLimited = [self.previewResult[@"stop_reason"] isEqual:@"time_limit"];
    BOOL partial = timeLimited || [self.previewResult[@"truncated"] boolValue] || ![self.previewResult[@"complete"] boolValue] || errors.count || interruptedChecks.count || candidates.count > shown;
    NSString *englishFormat = partial ? (shown == 1 ? @"Preview sample · %@ entries checked. Showing %lu filename change. Available folders can begin cleanup." : @"Preview sample · %@ entries checked. Showing %lu filename changes. Available folders can begin cleanup.") : (shown == 1 ? @"%@ entries checked · %lu filename change shown." : @"%@ entries checked · %lu filename changes shown.");
    NSString *format = JasoText(englishFormat, partial ? @"미리보기 예시 · 항목 %@개를 확인했습니다. 파일명 변경 %lu개를 표시합니다. 사용할 수 있는 폴더에서 정리를 시작할 수 있습니다." : @"항목 %@개를 확인했습니다. 파일명 변경 %lu개를 표시합니다.");
    self.previewSummary.stringValue = [NSString stringWithFormat:format, self.previewResult[@"entries"] ?: @0, (unsigned long)shown];
    if (timeLimited) self.previewSummary.stringValue = [self.previewSummary.stringValue stringByAppendingString:JasoText(@"\nThese results were checked within the preview time limit. Choose a specific folder for more detail.", @"\n미리보기 시간 안에 확인한 결과입니다. 더 자세히 보려면 특정 폴더를 선택하세요.")];
    if (!shown) self.previewSummary.stringValue = [self.previewSummary.stringValue stringByAppendingString:JasoText(@" The checked sample has no names to change. Future files can still be handled automatically.", @" 확인한 예시에서 바꿀 이름이 없습니다. 이후 추가되는 파일도 자동으로 정리할 수 있습니다.")];
    for (NSUInteger i = 0; i < shown; i++) {
        NSDictionary *candidate = candidates[i]; if (![candidate isKindOfClass:NSDictionary.class]) continue;
        NSStackView *row = SetupStack(); row.spacing = 4;
        NSTextField *path = SetupText(11, NSFontWeightRegular, NSColor.secondaryLabelColor); path.stringValue = [candidate[@"path"] isKindOfClass:NSString.class] ? candidate[@"path"] : @""; path.selectable = YES;
        NSTextField *beforeLabel = SetupText(11, NSFontWeightMedium, NSColor.secondaryLabelColor); beforeLabel.stringValue = JasoText(@"Original name", @"원래 이름");
        NSTextField *before = SetupText(13, NSFontWeightRegular, NSColor.labelColor); before.stringValue = [candidate[@"before"] isKindOfClass:NSString.class] ? candidate[@"before"] : @""; before.selectable = YES; before.identifier = [NSString stringWithFormat:@"setup-before-%lu", (unsigned long)i];
        NSTextField *afterLabel = SetupText(11, NSFontWeightMedium, NSColor.secondaryLabelColor); afterLabel.stringValue = JasoText(@"↓ After cleanup", @"↓ 정리된 이름");
        NSTextField *after = SetupText(13, NSFontWeightMedium, NSColor.labelColor); after.stringValue = [candidate[@"after"] isKindOfClass:NSString.class] ? candidate[@"after"] : @""; after.selectable = YES; after.identifier = [NSString stringWithFormat:@"setup-after-%lu", (unsigned long)i];
        for (NSView *view in @[path, beforeLabel, before, afterLabel, after]) SetupFullWidth(row, view);
        SetupFullWidth(self.previewRows, row);
    }
    for (NSUInteger i = 0; i < errors.count; i++) {
        NSDictionary *error = errors[i]; if (![error isKindOfClass:NSDictionary.class]) continue;
        NSTextField *field = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); field.selectable = YES; field.identifier = [NSString stringWithFormat:@"setup-preview-error-%lu", (unsigned long)i];
        field.stringValue = [NSString stringWithFormat:JasoText(@"%@\n%@\nOpen this location in Finder and check the reported cause.", @"%@\n%@\n해당 위치를 Finder에서 열어 안내된 원인을 확인하세요."), error[@"path"] ?: @"", error[@"error"] ?: @""];
        SetupFullWidth(self.previewRows, field);
    }
    if (interruptedChecks.count) {
        NSTextField *heading = SetupText(12, NSFontWeightMedium, NSColor.labelColor); heading.identifier = @"setup-interrupted-heading";
        heading.stringValue = JasoText(@"Locations not fully checked", @"확인이 끝나지 않은 위치");
        SetupFullWidth(self.previewRows, heading);
        for (NSUInteger i = 0; i < interruptedChecks.count; i++) {
            NSDictionary *check = interruptedChecks[i]; if (![check isKindOfClass:NSDictionary.class]) continue;
            NSTextField *field = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); field.selectable = YES; field.identifier = [NSString stringWithFormat:@"setup-interrupted-check-%lu", (unsigned long)i];
            field.stringValue = [NSString stringWithFormat:JasoText(@"%@\n%@\nFor more detail, preview a specific folder.", @"%@\n%@\n더 자세히 확인하려면 특정 폴더를 선택해 미리보세요."), check[@"path"] ?: @"", check[@"error"] ?: @""];
            SetupFullWidth(self.previewRows, field);
        }
    }
}
- (void)reloadLocalization {
    self.window.title = JasoText(@"Jaso NFC · Folders & cleanup", @"Jaso NFC · 정리할 폴더");
    self.heading.stringValue = JasoText(@"Choose your folders", @"정리할 폴더를 선택하세요");
    self.introduction.stringValue = JasoText(@"Keep filenames easy to share. Choose folders, review their names, and start cleanup when you are ready.", @"공유하기 편한 파일명으로 정리합니다. 폴더와 파일명을 확인한 뒤 준비되면 정리를 시작하세요.");
    self.scopeHeading.stringValue = JasoText(@"1 · Folders to clean up", @"1 · 정리할 폴더");
    [self.scopePicker itemAtIndex:0].title = JasoText(@"Selected folders", @"선택한 폴더"); [self.scopePicker itemAtIndex:1].title = JasoText(@"All user folders", @"모든 사용자 파일 영역");
    self.scopeGuide.stringValue = [self.scope isEqual:@"all-user-files"] ? JasoText(@"Your saved broad scope includes user folders, shared folders, cloud documents, and connected drives. Choose Selected folders to set a focused list.", @"저장된 범위에는 사용자 폴더, 공유 폴더, 클라우드 문서와 연결된 드라이브가 포함됩니다. 특정 폴더를 정리하려면 ‘선택한 폴더’를 선택하세요.") : JasoText(@"Add one or more folders. Their subfolders are included; exclusions below stay outside cleanup.", @"폴더를 하나 이상 추가하세요. 하위 폴더도 함께 정리하며, 아래에서 제외할 폴더를 지정할 수 있습니다.");
    self.scopePicker.accessibilityLabel = self.scopeHeading.stringValue;
    self.foldersTable.accessibilityLabel = JasoText(@"Selected folders", @"선택한 폴더"); self.excludesTable.accessibilityLabel = JasoText(@"Excluded folders", @"제외한 폴더");
    self.addFolder.title = JasoText(@"Add folders…", @"폴더 추가…"); self.removeFolder.title = JasoText(@"Remove selected", @"선택 항목 제거");
    self.excludesDisclosure.title = [NSString stringWithFormat:JasoText(@"Excluded folders (%lu)", @"제외할 폴더 (%lu)"), (unsigned long)self.excludes.count];
    self.excludesDisclosure.image = [NSImage imageWithSystemSymbolName:self.excludesExpanded ? @"chevron.down" : @"chevron.right" accessibilityDescription:nil];
    self.excludesDisclosure.accessibilityLabel = self.excludesDisclosure.title; self.excludesDisclosure.accessibilityValue = @(self.excludesExpanded);
    self.excludesContent.hidden = !self.excludesExpanded;
    self.excludesGuide.stringValue = JasoText(@"Optional. Keep work archives or other selected folders outside cleanup.", @"선택 사항입니다. 작업 보관함 등 정리에서 제외할 폴더를 추가하세요.");
    self.addExclude.title = JasoText(@"Add exclusions…", @"제외 폴더 추가…"); self.removeExclude.title = JasoText(@"Remove selected", @"선택 항목 제거");
    self.moreSettings.title = JasoText(@"More settings", @"추가 설정");
    self.moreSettings.image = [NSImage imageWithSystemSymbolName:self.modeExpanded ? @"chevron.down" : @"chevron.right" accessibilityDescription:nil];
    self.moreSettings.accessibilityLabel = self.moreSettings.title; self.moreSettings.accessibilityValue = @(self.modeExpanded);
    self.modeContent.hidden = !self.modeExpanded;
    self.modeHeading.stringValue = JasoText(@"Saved mode", @"저장할 모드");
    [self.modePicker itemAtIndex:0].title = JasoText(@"Preview mode", @"미리보기 모드"); [self.modePicker itemAtIndex:1].title = JasoText(@"Automatic cleanup", @"자동 정리");
    self.modePicker.accessibilityLabel = self.modeHeading.stringValue;
    self.modeGuide.stringValue = JasoText(@"Save settings keeps this mode and your current running or paused state. Start automatic cleanup enables automatic mode.", @"‘설정 저장’은 선택한 모드와 현재 실행·일시 정지 상태를 유지합니다. ‘자동 정리 시작’은 자동 모드를 켭니다.");
    self.previewHeading.stringValue = JasoText(@"2 · Review filenames", @"2 · 파일명 미리 확인");
    self.previewButton.title = JasoText(@"Preview filenames", @"파일명 미리보기"); self.cancelButton.title = JasoText(@"Cancel preview", @"미리보기 취소");
    self.reloadButton.title = JasoText(@"Reload saved settings", @"저장된 설정 다시 읽기");
    self.saveButton.title = JasoText(@"Save settings", @"설정 저장"); self.startButton.title = JasoText(@"Start automatic cleanup", @"자동 정리 시작");
    [self renderPreview]; [self refreshControls]; [self applyContentZoom];
}
- (CGFloat)contentZoom { return JasoContentZoom(); }
- (void)zoomIn:(id)sender { JasoSetContentZoom(self.contentZoom + .1); }
- (void)zoomOut:(id)sender { JasoSetContentZoom(self.contentZoom - .1); }
- (void)resetZoom:(id)sender { JasoSetContentZoom(1); }
- (BOOL)validateUserInterfaceItem:(id<NSValidatedUserInterfaceItem>)item { return JasoValidateContentZoomAction(self.window, item.action); }
- (void)contentZoomChanged:(NSNotification *)notification { [self applyContentZoom]; }
- (void)applyContentZoom {
    NSPoint origin = self.scroll.contentView.bounds.origin;
    JasoApplyContentZoom(self.body); JasoApplyContentZoom(self.footer);
    [self.window.contentView layoutSubtreeIfNeeded]; [self sizeTables];
    CGFloat maximumY = MAX(0, self.body.frame.size.height - self.scroll.contentView.bounds.size.height);
    [self.scroll.contentView scrollToPoint:NSMakePoint(0, MIN(origin.y, maximumY))]; [self.scroll reflectScrolledClipView:self.scroll.contentView];
}
- (void)sizeTables {
    for (NSTableView *table in @[self.foldersTable, self.excludesTable]) {
        NSScrollView *scroll = table.enclosingScrollView;
        table.rowHeight = ceil(26 * self.contentZoom);
        for (NSLayoutConstraint *constraint in scroll.constraints) if (constraint.firstAttribute == NSLayoutAttributeHeight && !constraint.secondItem) constraint.constant = MAX(40, MIN(104, table.numberOfRows * (table.rowHeight + 4) + 8));
        [self.window.contentView layoutSubtreeIfNeeded];
        [table reloadData]; [scroll tile];
        CGFloat width = scroll.contentView.bounds.size.width;
        NSRect frame = table.frame; frame.size.width = width; table.frame = frame;
        [table sizeLastColumnToFit];
    }
}
- (void)windowDidResize:(NSNotification *)notification { [self.window.contentView layoutSubtreeIfNeeded]; [self sizeTables]; }
- (void)revealPreview {
    NSRect frame = [self.body convertRect:self.previewHeading.bounds fromView:self.previewHeading];
    CGFloat height = self.scroll.contentView.bounds.size.height;
    CGFloat y = self.body.isFlipped ? NSMinY(frame) - 16 : NSMaxY(frame) + 16 - height;
    CGFloat maximum = MAX(0, self.body.frame.size.height - height);
    [self.scroll.contentView scrollToPoint:NSMakePoint(0, MAX(0, MIN(y, maximum)))]; [self.scroll reflectScrolledClipView:self.scroll.contentView];
}
- (void)showWindow:(id)sender { [self reloadLocalization]; [super showWindow:sender]; [NSApp activateIgnoringOtherApps:YES]; [self.window makeKeyAndOrderFront:nil]; }
- (BOOL)windowShouldClose:(NSWindow *)sender { return !self.busy || self.cancellable || [self.operation isEqual:@"cancel"]; }
- (void)windowWillClose:(NSNotification *)notification { if (self.busy && self.cancellable) [self cancelPreview:nil]; if (self.closeHandler) self.closeHandler(); }
- (void)dealloc { [NSNotificationCenter.defaultCenter removeObserver:self]; }
@end
