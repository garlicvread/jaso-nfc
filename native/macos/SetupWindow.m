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
    if (!self.card) { [NSColor.windowBackgroundColor setFill]; NSRectFill(NSIntersectionRect(dirtyRect, self.bounds)); return; }
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
static BOOL SetupDriveConnected(NSDictionary *drive) {
    return [drive[@"connected"] boolValue] && (!drive[@"availability"] || [drive[@"availability"] isEqual:@"connected"]);
}
static NSString *SetupDriveState(NSDictionary *drive) {
    if ([drive[@"availability"] isEqual:@"unavailable"]) return JasoText(@"Unavailable", @"확인할 수 없음");
    return SetupDriveConnected(drive) ? JasoText(@"Connected", @"연결됨") : JasoText(@"Disconnected", @"연결 해제됨");
}
static NSString *SetupShortPath(NSString *path) {
    if (path.length <= 180) return path;
    NSRange beginning = [path rangeOfComposedCharacterSequencesForRange:NSMakeRange(0, 85)];
    NSRange ending = [path rangeOfComposedCharacterSequencesForRange:NSMakeRange(path.length - 85, 85)];
    return [NSString stringWithFormat:@"%@…%@", [path substringWithRange:beginning], [path substringWithRange:ending]];
}
static NSDictionary *SetupDrivePreferences(id value) {
    if (!value) return @{@"mode":@"automatic", @"included":@[], @"excluded":@[]};
    if (![value isKindOfClass:NSDictionary.class] || ![@[@"automatic", @"selected"] containsObject:value[@"mode"] ?: @""]) return nil;
    for (NSString *key in @[@"included", @"excluded"]) {
        if (![value[key] isKindOfClass:NSArray.class]) return nil;
        for (id item in value[key]) if (![item isKindOfClass:NSDictionary.class] || ![item[@"uuid"] isKindOfClass:NSString.class] || ![item[@"uuid"] length] || ![item[@"mount"] isKindOfClass:NSString.class] || ![item[@"mount"] isAbsolutePath]) return nil;
    }
    NSMutableDictionary *result = [@{@"mode":value[@"mode"], @"included":[value[@"included"] copy], @"excluded":[value[@"excluded"] copy]} mutableCopy];
    if (value[@"reconnect"]) {
        if (![value[@"reconnect"] isKindOfClass:NSArray.class]) return nil;
        for (id item in value[@"reconnect"]) if (![item isKindOfClass:NSDictionary.class] || ![item[@"uuid"] isKindOfClass:NSString.class] || ![item[@"mount"] isKindOfClass:NSString.class] || ![@[@"automatic", @"manual"] containsObject:item[@"mode"] ?: @""]) return nil;
        if ([value[@"reconnect"] count]) result[@"reconnect"] = [value[@"reconnect"] copy];
    }
    return result;
}
static NSDictionary *SetupDriveReference(NSDictionary *drive) { return @{@"uuid":drive[@"uuid"], @"mount":drive[@"mount"]}; }
static BOOL SetupHasDrive(NSArray *references, NSString *uuid) {
    for (NSDictionary *reference in references) if ([reference[@"uuid"] isEqual:uuid]) return YES;
    return NO;
}

@interface JasoSetupWindowController ()
@property NSView *contentRoot;
@property (weak) NSWindow *hostWindow;
@property (readwrite, copy) NSString *revision;
@property NSMutableArray<NSString *> *roots;
@property NSMutableArray<NSString *> *excludes;
@property NSString *scope;
@property BOOL apply;
@property NSDictionary *confirmedDraft;
@property NSString *driveMode;
@property NSMutableArray *includedDrives;
@property NSMutableArray *excludedDrives;
@property NSMutableArray *reconnectPreferences;
@property NSArray *driveInventory;
@property NSArray<NSString *> *driveInventoryIssueMounts;
@property BOOL driveInventoryComplete;
@property NSView *drivesCard;
@property NSTextField *drivesHeading;
@property NSTextField *drivesGuide;
@property NSTextField *driveInventoryWarning;
@property NSPopUpButton *driveModePicker;
@property NSTableView *drivesTable;
@property NSPopUpButton *addDrive;
@property NSButton *removeDrive;
@property NSButton *refreshDrives;
@property NSButton *manageDrive;
@property NSPanel *driveSheet;
@property NSDictionary *sheetDrive;
@property NSPopUpButton *reconnectPicker;
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
    self.driveMode = @"automatic"; self.includedDrives = [NSMutableArray array]; self.excludedDrives = [NSMutableArray array]; self.driveInventory = @[];
    self.driveInventoryIssueMounts = @[]; self.driveInventoryComplete = YES;
    self.reconnectPreferences = [NSMutableArray array];
    window.contentView = [[JasoSetupBackground alloc] initWithFrame:window.contentView.bounds];
    self.contentRoot = window.contentView;
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

    NSStackView *drives = SetupStack();
    self.drivesHeading = SetupText(15, NSFontWeightSemibold, NSColor.labelColor);
    self.driveModePicker = [self picker:@"setup-drive-mode" choices:@[@"automatic", @"selected"] action:@selector(changeDriveMode:)];
    self.drivesGuide = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    self.driveInventoryWarning = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    self.driveInventoryWarning.identifier = @"setup-drive-inventory-warning"; self.driveInventoryWarning.selectable = YES;
    self.drivesTable = [self table:@"setup-drives"];
    self.addDrive = [self picker:@"setup-add-drive" choices:@[] action:@selector(addSelectedDrive:)];
    self.removeDrive = [self button:@"setup-remove-drive" action:@selector(removeDrives:)];
    self.refreshDrives = [self button:@"setup-refresh-drives" action:@selector(requestDriveRefresh:)];
    self.manageDrive = [self button:@"setup-manage-drive" action:@selector(showDriveDetails:)];
    for (NSView *view in @[self.drivesHeading, self.driveModePicker, self.drivesGuide, self.driveInventoryWarning, self.drivesTable.enclosingScrollView]) SetupFullWidth(drives, view);
    SetupFullWidth(drives, self.addDrive);
    SetupFullWidth(drives, self.removeDrive);
    SetupFullWidth(drives, self.manageDrive);
    SetupFullWidth(drives, self.refreshDrives);
    self.drivesCard = SetupCard(drives); self.drivesCard.identifier = @"setup-drives-card";
    SetupFullWidth(self.body, self.drivesCard);

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
- (NSView *)embeddedContentViewForWindow:(NSWindow *)host {
    self.hostWindow = host;
    [self.window orderOut:nil];
    return self.contentRoot;
}
- (NSWindow *)presentationWindow { return self.contentRoot.window ?: self.hostWindow ?: self.window; }
- (NSButton *)button:(NSString *)identifier action:(SEL)action {
    NSButton *button = [NSButton buttonWithTitle:@"" target:self action:action]; button.identifier = identifier; return button;
}
- (NSPopUpButton *)picker:(NSString *)identifier choices:(NSArray<NSString *> *)choices action:(SEL)action {
    NSPopUpButton *picker = [[NSPopUpButton alloc] initWithFrame:NSZeroRect pullsDown:NO]; picker.identifier = identifier; picker.target = self; picker.action = action;
    // refreshControls gates the whole picker; choices do not use zoom-command validation.
    picker.autoenablesItems = NO;
    // Menu item names can be much wider than the selected title. Keep their
    // full text in the menu while fitting the control to the shared page.
    [picker setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];
    picker.lineBreakMode = NSLineBreakByTruncatingMiddle;
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
- (NSDictionary *)draft {
    NSMutableDictionary *drives = [@{@"mode":self.driveMode, @"included":[self.includedDrives copy], @"excluded":[self.excludedDrives copy]} mutableCopy];
    if (self.reconnectPreferences.count) drives[@"reconnect"] = [self.reconnectPreferences copy];
    return @{@"scope":self.scope ?: @"configured", @"roots":[self.scope isEqual:@"all-user-files"] ? @[] : [self.roots copy] ?: @[], @"excludes":[self.excludes copy] ?: @[], @"apply":@(self.apply), @"drives":drives};
}
- (NSDictionary *)editableConfig:(NSDictionary *)result {
    if (![result isKindOfClass:NSDictionary.class]) return nil;
    NSDictionary *config = result[@"config"];
    if (![config isKindOfClass:NSDictionary.class] || ![@[@"configured", @"all-user-files"] containsObject:config[@"scope"] ?: @""] || !SetupStrings(config[@"roots"]) || !SetupStrings(config[@"excludes"]) || ![config[@"apply"] isKindOfClass:NSNumber.class] || ![result[@"revision"] isKindOfClass:NSString.class] || ![result[@"revision"] length]) return nil;
    NSDictionary *drives = SetupDrivePreferences(config[@"drives"]);
    if (!drives) return nil;
    return @{@"scope":config[@"scope"], @"roots":[config[@"roots"] copy], @"excludes":[config[@"excludes"] copy], @"apply":@([config[@"apply"] boolValue]), @"drives":drives};
}
- (void)loadConfirmed:(NSDictionary *)config revision:(NSString *)revision {
    self.scope = config[@"scope"];
    if (![self.scope isEqual:@"all-user-files"] || [config[@"roots"] count]) self.roots = [config[@"roots"] mutableCopy];
    self.excludes = [config[@"excludes"] mutableCopy]; self.apply = [config[@"apply"] boolValue];
    self.driveMode = config[@"drives"][@"mode"]; self.includedDrives = [config[@"drives"][@"included"] mutableCopy]; self.excludedDrives = [config[@"drives"][@"excluded"] mutableCopy];
    self.reconnectPreferences = [config[@"drives"][@"reconnect"] mutableCopy] ?: [NSMutableArray array];
    self.confirmedDraft = config; self.revision = revision;
    [self.scopePicker selectItemAtIndex:[self.scope isEqual:@"all-user-files"] ? 1 : 0]; [self.modePicker selectItemAtIndex:self.apply ? 1 : 0];
    [self.driveModePicker selectItemAtIndex:[self.driveMode isEqual:@"selected"] ? 1 : 0];
    [self.foldersTable reloadData]; [self.excludesTable reloadData];
}
- (void)updateConfiguration:(NSDictionary *)result error:(NSString *)error {
    NSDictionary *config = [self editableConfig:result];
    [self setBusy:NO cancellable:NO];
    if (error.length || !config) { [self showError:error.length ? error : JasoText(@"Could not read saved folders. Choose Reload saved settings to try again.", @"저장된 폴더를 읽을 수 없습니다. ‘저장된 설정 다시 읽기’를 선택하세요.")]; return; }
    [self loadDriveInventory:result];
    [self loadConfirmed:config revision:result[@"revision"]]; self.previewResult = nil; self.previewDraft = nil; self.messageText = nil; self.notice = nil; self.messageIsError = NO;
    [self reloadLocalization];
}
- (void)loadDriveInventory:(NSDictionary *)result {
    NSMutableOrderedSet *mounts = NSMutableOrderedSet.new;
    if ([result[@"drive_inventory_issues"] isKindOfClass:NSArray.class]) for (id issue in result[@"drive_inventory_issues"]) {
        if ([issue isKindOfClass:NSDictionary.class] && [issue[@"mount"] isKindOfClass:NSString.class] && [issue[@"mount"] isAbsolutePath]) [mounts addObject:issue[@"mount"]];
    }
    self.driveInventoryIssueMounts = mounts.array;
    self.driveInventoryComplete = [result[@"drive_inventory_complete"] isKindOfClass:NSNumber.class] ? [result[@"drive_inventory_complete"] boolValue] : mounts.count == 0;
    if (![result[@"drive_inventory"] isKindOfClass:NSArray.class]) { self.driveInventory = @[]; return; }
    NSMutableArray *inventory = [NSMutableArray array]; NSMutableSet *seen = [NSMutableSet set];
    for (id drive in result[@"drive_inventory"]) {
        if (![drive isKindOfClass:NSDictionary.class] || ![drive[@"uuid"] isKindOfClass:NSString.class] || ![drive[@"uuid"] length] || ![drive[@"mount"] isKindOfClass:NSString.class] || ![drive[@"mount"] isAbsolutePath] || ![drive[@"connected"] isKindOfClass:NSNumber.class] || [seen containsObject:drive[@"uuid"]]) continue;
        NSMutableDictionary *entry = [drive mutableCopy];
        NSString *availability = drive[@"availability"] ?: ([drive[@"connected"] boolValue] ? @"connected" : @"disconnected");
        if (![@[@"connected", @"disconnected", @"unavailable"] containsObject:availability]) availability = @"unavailable";
        entry[@"availability"] = availability; entry[@"connected"] = @(SetupDriveConnected(entry));
        [seen addObject:drive[@"uuid"]]; [inventory addObject:entry];
    }
    self.driveInventory = [inventory sortedArrayUsingComparator:^NSComparisonResult(NSDictionary *a, NSDictionary *b) { return [a[@"mount"] localizedStandardCompare:b[@"mount"]]; }];
}
- (void)updateDriveInventory:(NSDictionary *)result error:(NSString *)error {
    [self setBusy:NO cancellable:NO];
    if (error.length || ![result[@"drive_inventory"] isKindOfClass:NSArray.class]) {
        [self showError:error.length ? error : JasoText(@"Could not refresh drives. Try Refresh drives again.", @"드라이브 목록을 확인할 수 없습니다. ‘드라이브 새로고침’을 다시 선택하세요.")]; return;
    }
    [self loadDriveInventory:result]; self.messageText = nil; self.messageIsError = NO;
    [self.drivesTable deselectAll:nil]; [self reloadLocalization];
}
- (void)requestDriveRefresh:(NSButton *)sender {
    if (!sender.enabled || !self.refreshDrivesHandler) return;
    self.operation = @"drive-refresh"; [self setBusy:YES cancellable:NO]; self.refreshDrivesHandler();
}
- (BOOL)containsConfiguredFolders:(NSDictionary *)drive {
    NSString *mount = [drive[@"mount"] stringByStandardizingPath];
    for (NSString *root in self.roots) if ([root isEqual:mount] || [root hasPrefix:[mount stringByAppendingString:@"/"]]) return YES;
    return NO;
}
- (BOOL)includesDrive:(NSDictionary *)drive {
    if (SetupHasDrive(self.excludedDrives, drive[@"uuid"])) return NO;
    if ([self.scope isEqual:@"configured"]) return [self containsConfiguredFolders:drive] && ([drive[@"included"] boolValue] || SetupHasDrive(self.includedDrives, drive[@"uuid"]));
    return SetupHasDrive(self.includedDrives, drive[@"uuid"]) || ([self.driveMode isEqual:@"automatic"] && [drive[@"included"] boolValue]);
}
- (NSArray *)managedDrives {
    NSMutableArray *managed = [NSMutableArray array];
    for (NSDictionary *drive in self.driveInventory) if ([self includesDrive:drive]) [managed addObject:drive];
    // An added drive may disconnect before the draft is saved. Its explicit
    // selection remains editable even when discovery no longer returns it.
    for (NSDictionary *reference in self.includedDrives) if (!SetupHasDrive(managed, reference[@"uuid"]) && [self includesDrive:reference]) {
        NSMutableDictionary *drive = [reference mutableCopy]; drive[@"connected"] = @NO; drive[@"availability"] = self.driveInventoryComplete ? @"disconnected" : @"unavailable"; [managed addObject:drive];
    }
    return [managed sortedArrayUsingComparator:^NSComparisonResult(NSDictionary *a, NSDictionary *b) { return [a[@"mount"] localizedStandardCompare:b[@"mount"]]; }];
}
- (void)renderDrives {
    BOOL configured = [self.scope isEqual:@"configured"];
    BOOL relevantDrive = NO;
    for (NSDictionary *drive in self.driveInventory) if ([self containsConfiguredFolders:drive]) relevantDrive = YES;
    BOOL incomplete = !self.driveInventoryComplete || self.driveInventoryIssueMounts.count;
    self.drivesCard.hidden = configured && ![self managedDrives].count && !relevantDrive && !incomplete;
    self.driveInventoryWarning.hidden = !incomplete;
    NSMutableArray *warning = [NSMutableArray arrayWithObject:JasoText(@"Some drive information could not be checked. Choose Refresh drives to try again.", @"일부 드라이브 정보를 확인하지 못했습니다. ‘드라이브 새로고침’을 선택해 다시 확인하세요.")];
    NSUInteger shown = MIN(3, self.driveInventoryIssueMounts.count);
    for (NSUInteger i=0; i<shown; i++) [warning addObject:SetupShortPath(self.driveInventoryIssueMounts[i])];
    if (self.driveInventoryIssueMounts.count > shown) [warning addObject:[NSString stringWithFormat:JasoText(@"And %lu more locations", @"외 %lu개 위치"), (unsigned long)(self.driveInventoryIssueMounts.count - shown)]];
    self.driveInventoryWarning.stringValue = incomplete ? [warning componentsJoinedByString:@"\n"] : @"";
    self.driveModePicker.hidden = configured; self.addDrive.hidden = NO; self.removeDrive.hidden = NO;
    self.drivesHeading.stringValue = JasoText(@"Drives", @"드라이브");
    [self.driveModePicker itemAtIndex:0].title = JasoText(@"Include remembered drives", @"기존 드라이브 포함");
    [self.driveModePicker itemAtIndex:1].title = JasoText(@"Choose drives manually", @"직접 선택");
    self.driveModePicker.accessibilityLabel = JasoText(@"Drive selection", @"드라이브 선택 방식");
    self.drivesTable.accessibilityLabel = JasoText(@"Managed drives", @"관리 중인 드라이브");
    self.removeDrive.title = JasoText(@"Remove selected drive", @"선택한 드라이브 제거");
    self.refreshDrives.title = JasoText(@"Refresh drives", @"드라이브 새로고침");
    self.manageDrive.title = JasoText(@"Manage selected drive…", @"선택한 드라이브 관리…");
    self.removeDrive.toolTip = JasoText(@"Remove this drive from cleanup. Add it again here to include it later.", @"이 드라이브를 정리 대상에서 제거합니다. 나중에 정리하려면 여기서 다시 추가하세요.");
    self.drivesGuide.stringValue = configured ? JasoText(@"Manage the drives containing your selected folders. Add a replacement drive to use it with these folders.", @"선택한 폴더가 저장된 드라이브를 관리합니다. 드라이브를 교체했다면 새 드라이브를 추가하세요.") : JasoText(@"Add a connected drive to include it. Manage each drive to choose how cleanup resumes when it reconnects.", @"연결된 드라이브를 추가해 정리할 수 있습니다. 드라이브별 관리에서 다시 연결했을 때 정리를 이어갈 방식을 선택하세요.");
    self.drivesGuide.stringValue = [self.drivesGuide.stringValue stringByAppendingString:JasoText(@" After connecting a drive, choose Refresh drives.", @" 드라이브를 연결한 뒤 ‘드라이브 새로고침’을 선택하세요.")];
    [self.addDrive removeAllItems]; [self.addDrive addItemWithTitle:JasoText(@"Add a connected drive…", @"연결된 드라이브 추가…")];
    self.addDrive.lastItem.enabled = YES;
    for (NSDictionary *drive in self.driveInventory) if (SetupDriveConnected(drive) && ![self includesDrive:drive] && (!configured || [self containsConfiguredFolders:drive])) {
        [self.addDrive addItemWithTitle:[drive[@"mount"] lastPathComponent]]; self.addDrive.lastItem.representedObject = drive[@"uuid"];
    }
    [self.addDrive selectItemAtIndex:0]; [self.drivesTable reloadData];
}
- (void)changeDriveMode:(NSPopUpButton *)sender {
    if (!sender.enabled) return;
    NSString *mode = sender.selectedItem.representedObject;
    if ([mode isEqual:self.driveMode]) return;
    if ([mode isEqual:@"selected"]) {
        NSMutableArray *included = [NSMutableArray array];
        for (NSDictionary *drive in [self managedDrives]) [included addObject:SetupDriveReference(drive)];
        self.includedDrives = included;
    }
    self.driveMode = mode; [self.drivesTable deselectAll:nil]; [self draftChanged];
}
- (void)addSelectedDrive:(NSPopUpButton *)sender {
    if (!sender.enabled) return;
    NSString *uuid = sender.selectedItem.representedObject;
    for (NSDictionary *drive in self.driveInventory) if ([drive[@"uuid"] isEqual:uuid] && SetupDriveConnected(drive)) {
        [self.excludedDrives filterUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(NSDictionary *reference, NSDictionary *bindings) { return ![reference[@"uuid"] isEqual:uuid]; }]];
        if (!SetupHasDrive(self.includedDrives, uuid)) [self.includedDrives addObject:SetupDriveReference(drive)];
        [self draftChanged]; return;
    }
}
- (void)removeDrives:(NSButton *)sender {
    if (!sender.enabled) return;
    NSArray *managed = [self managedDrives];
    [self.drivesTable.selectedRowIndexes enumerateIndexesUsingBlock:^(NSUInteger index, BOOL *stop) {
        if (index >= managed.count) return;
        [self removeDriveFromDraft:managed[index]];
    }];
    [self.drivesTable deselectAll:nil]; [self draftChanged];
}
- (void)removeDriveFromDraft:(NSDictionary *)drive {
    NSString *uuid = drive[@"uuid"], *mount = [drive[@"mount"] stringByStandardizingPath];
    [self.includedDrives filterUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(NSDictionary *reference, NSDictionary *bindings) { return ![reference[@"uuid"] isEqual:uuid]; }]];
    [self.reconnectPreferences filterUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(NSDictionary *reference, NSDictionary *bindings) { return ![reference[@"uuid"] isEqual:uuid]; }]];
    if (!SetupHasDrive(self.excludedDrives, uuid)) [self.excludedDrives addObject:SetupDriveReference(drive)];
    if ([self.scope isEqual:@"configured"]) {
        // A selected replacement may share the remembered drive's mount path.
        BOOL replacement = NO;
        for (NSDictionary *other in [self managedDrives]) if ([[other[@"mount"] stringByStandardizingPath] isEqual:mount]) replacement = YES;
        if (!replacement) [self.roots filterUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(NSString *root, NSDictionary *bindings) {
            return ![root isEqual:mount] && ![root hasPrefix:[mount stringByAppendingString:@"/"]];
        }]];
    }
}
- (void)removeDriveDetails:(NSButton *)sender {
    if (!sender.enabled || self.busy || !self.sheetDrive) return;
    [self removeDriveFromDraft:self.sheetDrive];
    [self closeDriveDetails:nil]; [self.drivesTable deselectAll:nil]; [self draftChanged];
    if (self.saveButton.enabled) [self requestSave:self.saveButton];
    else {
        self.messageText = JasoText(@"Drive removed from this selection. Preview your other folder changes, then choose Save settings to apply them.", @"드라이브를 선택 목록에서 제거했습니다. 함께 변경한 폴더를 미리 확인한 뒤 ‘설정 저장’을 눌러 적용하세요.");
        [self refreshControls];
    }
}
- (NSString *)reconnectMode:(NSString *)uuid {
    for (NSDictionary *preference in self.reconnectPreferences) if ([preference[@"uuid"] isEqual:uuid]) return preference[@"mode"];
    return @"inherit";
}
- (void)showDriveDetails:(NSButton *)sender {
    if (!sender.enabled || self.driveSheet) return;
    NSArray *managed = [self managedDrives]; NSInteger row = self.drivesTable.selectedRow;
    if (row < 0 || (NSUInteger)row >= managed.count) return;
    self.sheetDrive = managed[(NSUInteger)row];
    self.driveSheet = [[NSPanel alloc] initWithContentRect:NSMakeRect(0, 0, 560, 350) styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
    self.driveSheet.releasedWhenClosed = NO;
    self.driveSheet.title = JasoText(@"Drive details", @"드라이브 상세");
    NSScrollView *scroll = NSScrollView.new; scroll.translatesAutoresizingMaskIntoConstraints = NO;
    scroll.identifier = @"setup-drive-sheet-scroll"; scroll.hasVerticalScroller = YES; scroll.autohidesScrollers = YES; scroll.drawsBackground = NO;
    NSStackView *content = SetupStack(); content.spacing = 16; content.edgeInsets = NSEdgeInsetsMake(24, 24, 24, 24);
    scroll.documentView = content; [self.driveSheet.contentView addSubview:scroll];
    [NSLayoutConstraint activateConstraints:@[
        [scroll.leadingAnchor constraintEqualToAnchor:self.driveSheet.contentView.leadingAnchor],
        [scroll.trailingAnchor constraintEqualToAnchor:self.driveSheet.contentView.trailingAnchor],
        [scroll.topAnchor constraintEqualToAnchor:self.driveSheet.contentView.topAnchor],
        [scroll.bottomAnchor constraintEqualToAnchor:self.driveSheet.contentView.bottomAnchor],
        [content.widthAnchor constraintEqualToAnchor:scroll.contentView.widthAnchor],
        [content.leadingAnchor constraintEqualToAnchor:scroll.contentView.leadingAnchor],
        [content.topAnchor constraintEqualToAnchor:scroll.contentView.topAnchor]
    ]];
    NSTextField *title = SetupText(20, NSFontWeightSemibold, NSColor.labelColor); title.identifier = @"setup-drive-title";
    title.stringValue = [self.sheetDrive[@"mount"] lastPathComponent]; SetupFullWidth(content, title);
    NSTextField *path = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    path.stringValue = self.sheetDrive[@"mount"]; path.selectable = YES; SetupFullWidth(content, path);
    NSTextField *status = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor); status.identifier = @"setup-drive-status";
    status.stringValue = SetupDriveState(self.sheetDrive);
    if ([self.sheetDrive[@"availability"] isEqual:@"unavailable"]) status.stringValue = [status.stringValue stringByAppendingString:JasoText(@". In Folders, choose Refresh drives to check its connection again.", @". 폴더 화면에서 ‘드라이브 새로고침’을 선택해 연결 정보를 다시 확인하세요.")];
    SetupFullWidth(content, status);
    NSButton *remove = [self button:@"setup-drive-details-remove" action:@selector(removeDriveDetails:)];
    remove.title = JasoText(@"Remove from list", @"목록에서 제거");
    remove.toolTip = JasoText(@"Remove this drive and its folders from cleanup. Add it again when you want to use it.", @"이 드라이브와 해당 폴더를 정리 대상에서 제거합니다. 다시 사용하려면 폴더 화면에서 추가하세요.");
    SetupFullWidth(content, remove);
    NSTextField *label = SetupText(13, NSFontWeightMedium, NSColor.labelColor);
    label.stringValue = JasoText(@"When this drive reconnects", @"이 드라이브를 다시 연결하면"); SetupFullWidth(content, label);
    self.reconnectPicker = [self picker:@"setup-drive-reconnect" choices:@[@"inherit", @"automatic", @"manual"] action:@selector(driveReconnectChanged:)];
    NSArray *titles = @[JasoText(@"Default · Resume automatically", @"기본값 · 자동으로 이어서 정리"), JasoText(@"Resume automatically", @"자동으로 이어서 정리"), JasoText(@"Start manually", @"직접 시작")];
    for (NSUInteger i = 0; i < titles.count; i++) { NSMenuItem *item = [self.reconnectPicker itemAtIndex:i]; item.title = titles[i]; if ([item.representedObject isEqual:[self reconnectMode:self.sheetDrive[@"uuid"]]]) [self.reconnectPicker selectItem:item]; }
    SetupFullWidth(content, self.reconnectPicker);
    NSTextField *guide = SetupText(12, NSFontWeightRegular, NSColor.secondaryLabelColor);
    guide.stringValue = JasoText(@"Automatic cleanup follows the app's running or paused state. Manual start prepares only this drive for its current connection.", @"자동 정리는 앱의 실행·일시 정지 상태에 따라 이어집니다. 직접 시작을 선택하면 연결할 때마다 이 드라이브의 시작 버튼을 누릅니다.");
    SetupFullWidth(content, guide);
    NSButton *start = [self button:@"setup-start-drive" action:@selector(startSelectedDrive:)];
    start.title = JasoText(@"Start this drive", @"이 드라이브 시작");
    start.enabled = SetupDriveConnected(self.sheetDrive) && [[self reconnectMode:self.sheetDrive[@"uuid"]] isEqual:@"manual"] && [self.draft isEqual:self.confirmedDraft] && self.startDriveHandler != nil;
    SetupFullWidth(content, start);
    NSButton *cancel = [self button:@"setup-drive-details-cancel" action:@selector(closeDriveDetails:)]; cancel.title = JasoText(@"Cancel", @"취소"); cancel.keyEquivalent = @"\e";
    NSButton *save = [self button:@"setup-drive-details-save" action:@selector(saveDriveDetails:)]; save.title = JasoText(@"Save", @"저장");
    SetupFullWidth(content, [self buttonRow:cancel second:save]);
    JasoApplyContentZoom(content);
    [[self presentationWindow] beginSheet:self.driveSheet completionHandler:nil];
}
- (void)driveReconnectChanged:(NSPopUpButton *)sender {
    // Preferences are committed by Save; opening or cancelling a sheet keeps
    // both the stored preference and the folder draft intact.
    for (NSView *view in [(NSStackView *)self.reconnectPicker.superview arrangedSubviews]) if ([view.identifier isEqual:@"setup-start-drive"]) [(NSControl *)view setEnabled:NO];
}
- (void)closeDriveDetails:(id)sender {
    if (!self.driveSheet) return;
    [self.driveSheet.sheetParent endSheet:self.driveSheet]; [self.driveSheet orderOut:nil];
    self.driveSheet = nil; self.sheetDrive = nil; self.reconnectPicker = nil;
}
- (void)saveDriveDetails:(NSButton *)sender {
    NSString *uuid = self.sheetDrive[@"uuid"], *mount = self.sheetDrive[@"mount"], *mode = self.reconnectPicker.selectedItem.representedObject;
    if (!uuid || !mode) return;
    [self.reconnectPreferences filterUsingPredicate:[NSPredicate predicateWithBlock:^BOOL(NSDictionary *preference, NSDictionary *bindings) { return ![preference[@"uuid"] isEqual:uuid]; }]];
    if (![mode isEqual:@"inherit"]) [self.reconnectPreferences addObject:@{@"uuid":uuid, @"mount":mount, @"mode":mode}];
    [self closeDriveDetails:nil]; [self draftChanged];
    if (self.saveButton.enabled) [self requestSave:self.saveButton];
}
- (void)startSelectedDrive:(NSButton *)sender {
    if (!sender.enabled || !self.startDriveHandler || !SetupDriveConnected(self.sheetDrive) || ![self.draft isEqual:self.confirmedDraft]) return;
    NSString *uuid = self.sheetDrive[@"uuid"]; [self closeDriveDetails:nil];
    self.operation = @"drive-start"; [self setBusy:YES cancellable:NO];
    self.startDriveHandler(uuid, self.revision);
}
- (void)updateDriveStart:(NSDictionary *)result error:(NSString *)error {
    [self setBusy:NO cancellable:NO];
    if (error.length || ![result[@"ready"] boolValue]) { [self showError:error.length ? error : JasoText(@"Could not start this drive. Check its connection and try again.", @"드라이브를 시작할 수 없습니다. 연결 상태를 확인한 뒤 다시 시도하세요.")]; return; }
    self.messageText = [result[@"paused"] boolValue] || ![result[@"running"] boolValue]
        ? JasoText(@"This drive is ready. Start or resume the app to process it.", @"이 드라이브가 준비되었습니다. 앱 작업을 시작하거나 재개하면 처리합니다.")
        : JasoText(@"This drive is ready. Cleanup will continue shortly.", @"이 드라이브가 준비되었습니다. 곧 정리를 이어갑니다.");
    self.messageIsError = NO; [self refreshControls];
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
    BOOL changed = ![config isEqual:self.draft]; [self loadDriveInventory:result]; [self loadConfirmed:config revision:result[@"revision"]];
    if (changed) { self.previewResult = nil; self.previewDraft = nil; }
    self.messageIsError = NO;
    BOOL paused = [result[@"paused"] boolValue];
    BOOL automaticStarted = [result[@"started"] boolValue] && [config[@"apply"] boolValue] && !paused;
    self.messageText = nil; self.notice = paused ? @"paused" : automaticStarted ? @"started" : @"saved";
    [self reloadLocalization];
}
- (void)showError:(NSString *)error { self.messageText = error; self.notice = nil; self.messageIsError = YES; [self refreshControls]; [self.contentRoot layoutSubtreeIfNeeded]; }
- (BOOL)validDraft { return self.revision.length > 0 && [@[@"all-user-files", @"configured"] containsObject:self.scope]; }
- (BOOL)previewMatchesScope {
    NSDictionary *draft = self.draft;
    for (NSString *key in @[@"scope", @"roots", @"excludes", @"drives"]) if (![self.previewDraft[key] isEqual:draft[key]]) return NO;
    return YES;
}
- (BOOL)reviewedDraft { return self.previewResult != nil && [self previewMatchesScope]; }
- (BOOL)onlyDriveSelectionChanged {
    NSDictionary *draft = self.draft;
    for (NSString *key in @[@"scope", @"excludes", @"apply"]) if (![draft[key] isEqual:self.confirmedDraft[key]]) return NO;
    // Removing a configured drive also removes its selected folders. A smaller
    // scope can be saved directly; adding folders still needs its own preview.
    for (NSString *root in draft[@"roots"]) if (![self.confirmedDraft[@"roots"] containsObject:root]) return NO;
    return ![draft[@"drives"] isEqual:self.confirmedDraft[@"drives"]];
}
- (void)refreshControls {
    BOOL ready = self.revision.length && !self.busy;
    BOOL selected = [self.scope isEqual:@"configured"];
    self.scopePicker.enabled = ready; self.modePicker.enabled = ready;
    self.excludesDisclosure.enabled = ready;
    self.moreSettings.enabled = ready;
    self.foldersTable.enabled = ready && selected; self.excludesTable.enabled = ready;
    self.addFolder.enabled = ready && selected; self.removeFolder.enabled = ready && selected && self.foldersTable.selectedRowIndexes.count;
    self.addExclude.enabled = ready; self.removeExclude.enabled = ready && self.excludesTable.selectedRowIndexes.count;
    self.driveModePicker.enabled = ready && !selected; self.drivesTable.enabled = ready;
    self.refreshDrives.enabled = ready;
    self.manageDrive.enabled = ready && self.drivesTable.selectedRowIndexes.count == 1;
    self.addDrive.enabled = ready && self.addDrive.numberOfItems > 1;
    self.removeDrive.enabled = ready && self.drivesTable.selectedRowIndexes.count > 0;
    BOOL empty = selected && !self.roots.count;
    self.previewButton.enabled = ready && [self validDraft] && !empty; self.cancelButton.enabled = self.busy && self.cancellable;
    self.reloadButton.enabled = !self.busy;
    self.saveButton.enabled = ready && [self validDraft] && (empty || !self.apply || [self reviewedDraft] || [self.draft isEqual:self.confirmedDraft] || [self onlyDriveSelectionChanged]);
    self.startButton.enabled = ready && [self validDraft] && !empty && [self reviewedDraft];
    self.startButton.keyEquivalent = self.startButton.enabled ? @"\r" : @"";
    self.previewButton.keyEquivalent = !self.startButton.enabled && self.previewButton.enabled ? @"\r" : @"";
    if (self.busy) self.message.stringValue = [self.operation isEqual:@"cancel"] ? JasoText(@"Cancelling preview…", @"미리보기를 취소하고 있습니다…") : [self.operation isEqual:@"preview"] ? JasoText(@"Reviewing filenames…", @"파일명을 확인하고 있습니다…") : [self.operation isEqual:@"start"] ? JasoText(@"Saving folders and starting cleanup…", @"폴더를 저장하고 정리를 시작하고 있습니다…") : [self.operation isEqual:@"save"] ? JasoText(@"Saving folder settings…", @"폴더 설정을 저장하고 있습니다…") : JasoText(@"Reading saved folders…", @"저장된 폴더를 읽고 있습니다…");
    else if (self.messageText.length) self.message.stringValue = self.messageText;
    else if ([self.notice isEqual:@"started"]) self.message.stringValue = JasoText(@"Saved. Automatic cleanup has started for available folders.", @"저장했습니다. 사용할 수 있는 폴더에서 자동 정리를 시작했습니다.");
    else if ([self.notice isEqual:@"paused"]) self.message.stringValue = JasoText(@"Folders saved. Resume from the menu when you are ready.", @"폴더를 저장했습니다. 준비되면 메뉴에서 작업을 재개하세요.");
    else if ([self.notice isEqual:@"saved"]) self.message.stringValue = JasoText(@"Folder settings saved.", @"폴더 설정을 저장했습니다.");
    else if ([self.notice isEqual:@"cancelled"]) self.message.stringValue = JasoText(@"Preview cancelled. Your folder selections are kept.", @"미리보기를 취소했습니다. 선택한 폴더는 유지됩니다.");
    else if (!self.revision.length) self.message.stringValue = JasoText(@"Load your saved folder settings to begin.", @"저장된 폴더 설정을 불러와 시작하세요.");
    else if (empty) self.message.stringValue = JasoText(@"Add a folder to begin filename cleanup.", @"파일명 정리를 시작하려면 폴더를 추가하세요.");
    else if (![self validDraft]) self.message.stringValue = JasoText(@"Add a folder to preview filenames and save your setup.", @"파일명을 미리 확인하고 설정을 저장할 폴더를 추가하세요.");
    else if ([self onlyDriveSelectionChanged]) self.message.stringValue = JasoText(@"Choose Save settings to apply your drive selection.", @"‘설정 저장’을 선택하면 드라이브 선택을 적용합니다.");
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
    [panel beginSheetModalForWindow:[self presentationWindow] completionHandler:^(NSModalResponse response) {
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
- (NSInteger)numberOfRowsInTableView:(NSTableView *)tableView { return (NSInteger)(tableView == self.drivesTable ? [self managedDrives].count : tableView == self.foldersTable ? self.roots.count : self.excludes.count); }
- (NSView *)tableView:(NSTableView *)tableView viewForTableColumn:(NSTableColumn *)tableColumn row:(NSInteger)row {
    if (tableView == self.drivesTable) {
        NSArray *managed = [self managedDrives]; if (row < 0 || (NSUInteger)row >= managed.count) return nil;
        NSDictionary *drive = managed[(NSUInteger)row];
        NSTextField *field = [NSTextField wrappingLabelWithString:@""];
        NSString *state = SetupDriveState(drive);
        field.stringValue = [NSString stringWithFormat:@"%@ · %@\n%@", [drive[@"mount"] lastPathComponent], state, drive[@"mount"]];
        field.font = [NSFont systemFontOfSize:12 * self.contentZoom]; field.lineBreakMode = NSLineBreakByTruncatingMiddle;
        field.textColor = NSColor.labelColor; field.accessibilityLabel = field.stringValue; return field;
    }
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
    self.window.title = JasoText(@"Jaso NFC · Folders", @"Jaso NFC · 폴더");
    self.heading.stringValue = JasoText(@"Folders", @"폴더");
    self.introduction.stringValue = JasoText(@"Choose folders, preview the filename changes, and start automatic cleanup.", @"폴더를 선택하고 바뀔 파일 이름을 미리 확인한 뒤 자동 정리를 시작하세요.");
    self.scopeHeading.stringValue = JasoText(@"1 · Choose folders", @"1 · 폴더 선택");
    [self.scopePicker itemAtIndex:0].title = JasoText(@"Selected folders", @"선택한 폴더"); [self.scopePicker itemAtIndex:1].title = JasoText(@"All user folders", @"모든 사용자 파일 영역");
    self.scopeGuide.stringValue = [self.scope isEqual:@"all-user-files"] ? JasoText(@"Includes user folders, shared folders, and cloud documents. Manage external drives below.", @"사용자 폴더, 공유 폴더와 클라우드 문서를 정리합니다. 외장 드라이브는 아래에서 선택하세요.") : JasoText(@"Add folders or a drive to clean up. Their subfolders are included. Add exceptions below.", @"정리할 폴더나 드라이브를 추가하세요. 하위 폴더도 함께 정리합니다. 아래에서 제외할 폴더를 지정할 수 있습니다.");
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
    [self renderDrives]; [self renderPreview]; [self refreshControls]; [self applyContentZoom];
}
- (CGFloat)contentZoom { return JasoContentZoom(); }
- (void)zoomIn:(id)sender { JasoSetContentZoom(self.contentZoom + .1); }
- (void)zoomOut:(id)sender { JasoSetContentZoom(self.contentZoom - .1); }
- (void)resetZoom:(id)sender { JasoSetContentZoom(1); }
- (BOOL)validateUserInterfaceItem:(id<NSValidatedUserInterfaceItem>)item { return JasoValidateContentZoomAction([self presentationWindow], item.action); }
- (void)contentZoomChanged:(NSNotification *)notification { [self applyContentZoom]; }
- (void)applyContentZoom {
    NSPoint origin = self.scroll.contentView.bounds.origin;
    JasoApplyContentZoom(self.body); JasoApplyContentZoom(self.footer);
    BOOL stackActions = self.contentZoom > 1.2;
    for (NSButton *first in @[self.addFolder, self.addExclude, self.previewButton]) {
        NSStackView *row = (NSStackView *)first.superview;
        NSUserInterfaceLayoutOrientation orientation = stackActions ? NSUserInterfaceLayoutOrientationVertical : NSUserInterfaceLayoutOrientationHorizontal;
        if (row.orientation == orientation) continue;
        for (NSLayoutConstraint *constraint in [row.constraints copy]) if ([constraint.identifier isEqual:@"setup-stacked-action-width"]) constraint.active = NO;
        row.orientation = orientation;
        row.alignment = stackActions ? NSLayoutAttributeLeading : NSLayoutAttributeCenterY;
        row.distribution = stackActions ? NSStackViewDistributionFill : NSStackViewDistributionFillEqually;
        if (stackActions) for (NSView *button in row.arrangedSubviews) {
            NSLayoutConstraint *width = [button.widthAnchor constraintEqualToAnchor:row.widthAnchor];
            width.identifier = @"setup-stacked-action-width"; width.active = YES;
        }
    }
    if (self.driveSheet) JasoApplyContentZoom(self.driveSheet.contentView);
    [self.contentRoot layoutSubtreeIfNeeded]; [self sizeTables];
    CGFloat maximumY = MAX(0, self.body.frame.size.height - self.scroll.contentView.bounds.size.height);
    [self.scroll.contentView scrollToPoint:NSMakePoint(0, MIN(origin.y, maximumY))]; [self.scroll reflectScrolledClipView:self.scroll.contentView];
}
- (void)sizeTables {
    for (NSTableView *table in @[self.foldersTable, self.excludesTable, self.drivesTable]) {
        NSScrollView *scroll = table.enclosingScrollView;
        table.rowHeight = ceil((table == self.drivesTable ? 42 : 26) * self.contentZoom);
        for (NSLayoutConstraint *constraint in scroll.constraints) if (constraint.firstAttribute == NSLayoutAttributeHeight && !constraint.secondItem) constraint.constant = MAX(40, MIN(table == self.drivesTable ? 192 : 104, table.numberOfRows * (table.rowHeight + 4) + 8));
        [self.contentRoot layoutSubtreeIfNeeded];
        [table reloadData]; [scroll tile];
        CGFloat width = scroll.contentView.bounds.size.width;
        NSRect frame = table.frame; frame.size.width = width; table.frame = frame;
        [table sizeLastColumnToFit];
    }
}
- (void)windowDidResize:(NSNotification *)notification { [self.contentRoot layoutSubtreeIfNeeded]; [self sizeTables]; }
- (void)revealPreview {
    NSRect frame = [self.body convertRect:self.previewHeading.bounds fromView:self.previewHeading];
    CGFloat height = self.scroll.contentView.bounds.size.height;
    CGFloat y = self.body.isFlipped ? NSMinY(frame) - 16 : NSMaxY(frame) + 16 - height;
    CGFloat maximum = MAX(0, self.body.frame.size.height - height);
    [self.scroll.contentView scrollToPoint:NSMakePoint(0, MAX(0, MIN(y, maximum)))]; [self.scroll reflectScrolledClipView:self.scroll.contentView];
}
- (void)showWindow:(id)sender { [self reloadLocalization]; if (!self.hostWindow) [super showWindow:sender]; [NSApp activateIgnoringOtherApps:YES]; [[self presentationWindow] makeKeyAndOrderFront:nil]; }
- (BOOL)windowShouldClose:(NSWindow *)sender { return !self.busy || self.cancellable || [self.operation isEqual:@"cancel"]; }
- (void)windowWillClose:(NSNotification *)notification { if (self.busy && self.cancellable) [self cancelPreview:nil]; if (self.closeHandler) self.closeHandler(); }
- (void)dealloc { [NSNotificationCenter.defaultCenter removeObserver:self]; }
@end
