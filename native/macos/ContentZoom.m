#import "ContentZoom.h"
#import <objc/runtime.h>
#import <math.h>

NSNotificationName const JasoContentZoomDidChangeNotification = @"JasoContentZoomDidChangeNotification";
static char BaseFontKey;
static char ButtonHeightKey;
static char BaseControlSizeKey;

CGFloat JasoContentZoom(void) {
    id value = [NSUserDefaults.standardUserDefaults objectForKey:@"contentZoom"];
    if (![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID() || !isfinite([value doubleValue])) return 1;
    return MAX(.8, MIN(2, round([value doubleValue] * 10) / 10));
}

void JasoSetContentZoom(CGFloat scale) {
    if (!isfinite(scale)) return;
    scale = MAX(.8, MIN(2, round(scale * 10) / 10));
    if (fabs(scale - JasoContentZoom()) < .001) return;
    [NSUserDefaults.standardUserDefaults setDouble:scale forKey:@"contentZoom"];
    [NSNotificationCenter.defaultCenter postNotificationName:JasoContentZoomDidChangeNotification object:nil];
}

void JasoApplyContentZoom(NSView *view) {
    if ([view isKindOfClass:NSControl.class]) {
        NSControl *control = (NSControl *)view;
        NSFont *base = objc_getAssociatedObject(control, &BaseFontKey);
        if (!base && control.font) {
            base = control.font;
            objc_setAssociatedObject(control, &BaseFontKey, base, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        }
        if ([control isKindOfClass:NSButton.class]) {
            NSNumber *size = objc_getAssociatedObject(control, &BaseControlSizeKey);
            if (!size) { size = @(control.controlSize); objc_setAssociatedObject(control, &BaseControlSizeKey, size, OBJC_ASSOCIATION_RETAIN_NONATOMIC); }
            control.controlSize = JasoContentZoom() > 1.2 ? NSControlSizeLarge : size.integerValue;
        }
        if (base) control.font = [NSFontManager.sharedFontManager convertFont:base toSize:base.pointSize * JasoContentZoom()];
        if ([control isKindOfClass:NSButton.class] && control.font) {
            // Native button cellSize can retain its standard height even when
            // its font grows. Reserve room for the actual ascenders/descenders.
            NSLayoutConstraint *height = objc_getAssociatedObject(control, &ButtonHeightKey);
            if (!height) {
                height = [control.heightAnchor constraintGreaterThanOrEqualToConstant:0];
                height.active = YES;
                objc_setAssociatedObject(control, &ButtonHeightKey, height, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
            }
            height.constant = ceil(control.font.ascender - control.font.descender + control.font.leading) + 8;
        }
        [control invalidateIntrinsicContentSize];
    }
    for (NSView *child in view.subviews) JasoApplyContentZoom(child);
}

BOOL JasoValidateContentZoomAction(NSWindow *window, SEL action) {
    if (!window.keyWindow || !window.visible) return NO;
    if (action == @selector(zoomIn:)) return JasoContentZoom() < 2;
    if (action == @selector(zoomOut:)) return JasoContentZoom() > .8;
    if (action == @selector(resetZoom:)) return YES;
    return NO;
}
