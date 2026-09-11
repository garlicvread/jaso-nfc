#import "../macos/SettingsWindow.h"
#import "../macos/Localization.h"
#import <objc/runtime.h>

static NSUserDefaults *TestDefaults;
static id IsolatedDefaults(id self, SEL selector) { (void)self; (void)selector; return TestDefaults; }
static void Require(BOOL value, NSString *message) {
    if (!value) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static NSView *Find(NSView *view, NSString *identifier) {
    if ([view.identifier isEqual:identifier]) return view;
    for (NSView *child in view.subviews) {
        NSView *match = Find(child, identifier);
        if (match) return match;
    }
    return nil;
}
static void Choose(NSPopUpButton *picker, NSString *value) {
    [picker.menu update];
    for (NSMenuItem *item in picker.itemArray) {
        if ([item.representedObject isEqual:value]) {
            Require(picker.enabled && item.enabled, [NSString stringWithFormat:@"Language option is disabled after menu validation: %@", item.title]);
            [picker.menu performActionForItemAtIndex:[picker indexOfItem:item]];
            return;
        }
    }
    Require(NO, @"The requested language is absent from Settings");
}
static void CheckLayout(NSView *view, NSView *content) {
    if ([view isKindOfClass:NSScrollView.class]) {
        NSView *document = [(NSScrollView *)view documentView];
        Require(document.frame.size.width <= [(NSScrollView *)view contentView].bounds.size.width + 1, @"Settings document must fit its viewport");
        CheckLayout(document, document);
        return;
    }
    if ([view isKindOfClass:NSTextField.class] || [view isKindOfClass:NSButton.class]) {
        NSRect frame = [content convertRect:view.bounds fromView:view];
        Require(!NSIsEmptyRect(frame) && NSContainsRect(NSInsetRect(content.bounds, -1, -1), frame), @"A settings control falls outside the window");
        if ([view isKindOfClass:NSTextField.class]) {
            NSTextField *field = (NSTextField *)view;
            NSSize required = [field.cell cellSizeForBounds:NSMakeRect(0, 0, field.bounds.size.width, CGFLOAT_MAX)];
            Require(required.height <= field.bounds.size.height + 1, @"A localized settings label is vertically clipped");
        } else if ([view isKindOfClass:NSButton.class]) {
            Require([(NSButton *)view cell].cellSize.width <= view.bounds.size.width + 1, @"A localized settings button title is clipped");
        }
    }
    for (NSView *child in view.subviews) CheckLayout(child, content);
}
static void Render(JasoSettingsWindowController *controller, NSString *path) {
    NSView *view = controller.window.contentView;
    [view layoutSubtreeIfNeeded];
    NSBitmapImageRep *bitmap = [view bitmapImageRepForCachingDisplayInRect:view.bounds];
    [view cacheDisplayInRect:view.bounds toBitmapImageRep:bitmap];
    NSData *data = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
    Require(data.length > 1000 && [data writeToFile:path atomically:YES], @"Could not render the production Settings view");
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSString *suite = [@"jaso-nfc.settings-window-test." stringByAppendingString:NSUUID.UUID.UUIDString];
        TestDefaults = [[NSUserDefaults alloc] initWithSuiteName:suite];
        [TestDefaults setObject:@"en" forKey:@"interfaceLanguage"];
        // Route production localization through a disposable suite, never the app's preferences.
        Method method = class_getClassMethod(NSUserDefaults.class, @selector(standardUserDefaults));
        IMP original = method_setImplementation(method, (IMP)IsolatedDefaults);
        NSString *output = argc > 1 ? [NSString stringWithUTF8String:argv[1]] : nil;
        int failures = 0;
        int passed = 0;
        @try {
            if (output) Require([NSFileManager.defaultManager createDirectoryAtPath:output withIntermediateDirectories:YES attributes:nil error:NULL], @"Could not create Settings preview directory");
            JasoSettingsWindowController *controller = [JasoSettingsWindowController new];
            NSPopUpButton *picker = (NSPopUpButton *)Find(controller.window.contentView, @"interface-language-picker");
            Require(picker != nil && picker.numberOfItems == 3, @"Settings must expose exactly three language options");
            Require([picker.itemTitles isEqual:@[@"System setting", @"한국어", @"English"]], @"Language choices must remain discoverable in their own languages");
            Require([picker.selectedItem.representedObject isEqual:@"en"], @"Settings did not load the saved language");
            Require([controller.window.title isEqual:@"Jaso NFC · Settings"], @"English window title is missing");
            passed++;

            NSButton *folders = (NSButton *)Find(controller.window.contentView, @"setup-folders");
            Require([folders isKindOfClass:NSButton.class] && folders.enabled, @"Users must be able to configure folders and automatic cleanup from Settings");
            __block NSString *setupAction = nil;
            controller.actionHandler = ^(NSString *action) { setupAction = action; };
            [folders performClick:nil];
            Require([setupAction isEqual:@"setup"], @"The folder settings action must open the in-app setup flow");
            passed++;

            Require(Find(controller.window.contentView, @"run-at-login") != nil, @"Run at login belongs in Settings");
            Require(Find(controller.window.contentView, @"animate-mark") != nil, @"Icon animation belongs in Settings");
            Require(Find(controller.window.contentView, @"mark-style") != nil, @"Icon theme belongs in Settings");
            passed++;

            NSButton *startup = (NSButton *)Find(controller.window.contentView, @"run-at-login");
            Require(!startup.enabled && startup.state == NSControlStateValueMixed, @"Unknown startup must not appear off or be editable");
            [controller updateStartupEnabled:@NO error:nil busy:NO];
            __block NSUInteger startupChanges = 0; __block BOOL requestedStartup = NO;
            controller.startupChangedHandler = ^(BOOL enabled) { startupChanges++; requestedStartup = enabled; };
            [startup performClick:nil];
            Require(startupChanges == 1 && requestedStartup && !startup.enabled && startup.state == NSControlStateValueOff, @"Startup change must request registration and retain confirmed state until refreshed");
            [controller updateStartupEnabled:@YES error:nil busy:NO];
            Require(startup.enabled && startup.state == NSControlStateValueOn, @"Confirmed startup state did not appear");
            [controller updateStartupEnabled:nil error:@"Inconsistent registration" busy:NO];
            Require(!startup.enabled && startup.state == NSControlStateValueMixed && [[(NSTextField *)Find(controller.window.contentView, @"startup-status") stringValue] containsString:@"Inconsistent"], @"Startup errors must remain visible without claiming off");
            [controller updateStartupEnabled:@NO error:nil busy:NO];
            __block NSUInteger appearanceChanges = 0;
            controller.appearanceChangedHandler = ^{ appearanceChanges++; };
            NSButton *animation = (NSButton *)Find(controller.window.contentView, @"animate-mark");
            [animation performClick:nil];
            Require(appearanceChanges == 1 && ![TestDefaults boolForKey:@"animateMark"], @"Animation must persist and update the menu immediately");
            NSPopUpButton *style = (NSPopUpButton *)Find(controller.window.contentView, @"mark-style");
            controller.availableStyles = @[@YES, @YES, @NO];
            [style.menu update];
            Require(![style itemAtIndex:2].enabled, @"Unavailable fonts must not be selectable");
            Require([style itemAtIndex:0].enabled, @"Available fonts must remain selectable after menu validation");
            [style.menu performActionForItemAtIndex:0];
            Require(appearanceChanges == 2 && [TestDefaults integerForKey:@"markStyle"] == 0, @"Icon theme must persist and notify the menu immediately");
            passed++;

            NSMutableArray *actions = [NSMutableArray array];
            controller.actionHandler = ^(NSString *action) { [actions addObject:action]; };
            NSArray *actionIDs = @[@"login-items", @"full-disk-access", @"reveal-app"];
            for (NSString *identifier in actionIDs) {
                NSButton *button = (NSButton *)Find(controller.window.contentView, identifier);
                Require([button isKindOfClass:NSButton.class] && button.enabled, @"A required permissions action is missing from Settings");
                [button performClick:nil];
            }
            Require([actions isEqual:actionIDs], @"Permissions buttons did not dispatch their intended callbacks");
            NSString *guide = [(NSTextField *)Find(controller.window.contentView, @"full-disk-access-guide") stringValue];
            Require(![guide containsString:@"~/Applications/"] && [guide containsString:@"Finder"] && [guide containsString:@"+"] && [guide containsString:@"/Applications/Jaso NFC.app"] && [guide containsString:@"Restart worker"], @"Full Disk Access guidance must explain adding the installed app and restarting the worker");
            passed++;

            __block int changes = 0;
            __block NSString *callbackTitle = nil;
            __weak JasoSettingsWindowController *weakController = controller;
            controller.languageChangedHandler = ^{ changes++; callbackTitle = weakController.window.title; };
            Choose(picker, @"ko");
            Require(changes == 1 && [JasoLanguagePreference() isEqual:@"ko"] && JasoUsesKorean(), @"Korean selection did not save and invoke the callback");
            Require([[TestDefaults stringForKey:@"interfaceLanguage"] isEqual:@"ko"], @"Language selection was not persisted to defaults");
            Require([callbackTitle isEqual:@"Jaso NFC · 설정"], @"The callback ran before the Settings window was relocalized");
            Require([[picker itemAtIndex:0].title isEqual:@"시스템 설정"], @"The system language choice was not relocalized");
            Require([[(NSTextField *)Find(controller.window.contentView, @"interface-language-label") stringValue] isEqual:@"언어"], @"The language label was not relocalized");
            Require([[(NSButton *)Find(controller.window.contentView, @"login-items") title] isEqual:@"로그인 항목 열기…"], @"Permissions actions did not relocalize");
            Require([[(NSTextField *)Find(controller.window.contentView, @"full-disk-access-guide") stringValue] containsString:@"/Applications/Jaso NFC.app"], @"The Korean permissions guide lost the installed app location");
            passed++;

            JasoSettingsWindowController *reopened = [JasoSettingsWindowController new];
            NSPopUpButton *reopenedPicker = (NSPopUpButton *)Find(reopened.window.contentView, @"interface-language-picker");
            Require([reopenedPicker.selectedItem.representedObject isEqual:@"ko"] && [reopened.window.title isEqual:@"Jaso NFC · 설정"], @"A new Settings window lost the saved choice");
            [reopened close];
            passed++;

            Choose(picker, @"en");
            Require(changes == 2 && !JasoUsesKorean() && [controller.window.title isEqual:@"Jaso NFC · Settings"], @"English selection did not apply immediately");
            Choose(picker, @"system");
            Require(changes == 3 && [JasoLanguagePreference() isEqual:@"system"] && [picker.selectedItem.representedObject isEqual:@"system"], @"System language selection did not apply");
            Require(JasoUsesKorean() == [[NSLocale preferredLanguages].firstObject hasPrefix:@"ko"], @"System language selection does not follow the preferred language");
            passed++;

            JasoSetLanguagePreference(@"en");
            [controller reloadLocalization];
            Require(changes == 3 && [picker.selectedItem.representedObject isEqual:@"en"], @"Reload must reflect an external choice without invoking the callback");
            passed++;

            for (NSString *language in @[@"en", @"ko"]) {
                Choose(picker, language);
                for (NSString *appearance in @[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) {
                    controller.window.appearance = [NSAppearance appearanceNamed:appearance];
                    [controller.window.contentView layoutSubtreeIfNeeded];
                    CheckLayout(controller.window.contentView, controller.window.contentView);
                    if (output) Render(controller, [output stringByAppendingPathComponent:[NSString stringWithFormat:@"settings-%@-%@.png", language, [appearance isEqual:NSAppearanceNameAqua] ? @"light" : @"dark"]]);
                    passed++;
                }
            }

            __block int closes = 0;
            controller.closeHandler = ^{ closes++; };
            [controller showWindow:nil];
            [controller close];
            Require(closes == 1 && !controller.window.visible, @"Closing Settings did not notify the owner");
            passed++;

            __weak JasoSettingsWindowController *released;
            @autoreleasepool {
                JasoSettingsWindowController *temporary = [JasoSettingsWindowController new];
                released = temporary;
                [temporary close];
            }
            Require(released == nil, @"A closed Settings window must be releasable");
            passed++;
        } @catch (NSException *error) {
            fprintf(stderr, "FAIL: %s\n", error.reason.UTF8String);
            failures++;
        } @finally {
            method_setImplementation(method, original);
            [TestDefaults removePersistentDomainForName:suite];
            TestDefaults = nil;
        }
        printf("%d settings window cases, %d failures\n", passed, failures);
        return failures ? 1 : 0;
    }
}
