// Deterministic offscreen asset rendering from the production vector mark.
#import "AnimatedMark.h"
#import <QuartzCore/QuartzCore.h>
#import <ImageIO/ImageIO.h>

static CGAffineTransform PieceTransform(CALayer *piece) {
    CGAffineTransform transform = piece.affineTransform;
    CGFloat anchorX = piece.bounds.origin.x + piece.anchorPoint.x * piece.bounds.size.width;
    CGFloat anchorY = piece.bounds.origin.y + piece.anchorPoint.y * piece.bounds.size.height;
    transform.tx += piece.position.x - transform.a * anchorX - transform.c * anchorY;
    transform.ty += piece.position.y - transform.b * anchorX - transform.d * anchorY;
    return transform;
}

static NSBitmapImageRep *RenderImage(NSUInteger width, NSUInteger height, BOOL icon, CGFloat progress, BOOL dark) {
    JasoAnimatedMark *mark = [[JasoAnimatedMark alloc] initWithFrame:NSMakeRect(0, 0, JasoMarkWidth, 22)];
    mark.appearance = [NSAppearance appearanceNamed:dark ? NSAppearanceNameDarkAqua : NSAppearanceNameAqua];
    [mark viewDidChangeEffectiveAppearance];
    mark.animationEnabled = NO;
    NSMutableArray<CAShapeLayer *> *pieces = [NSMutableArray array];
    for (CALayer *layer in mark.layer.sublayers) if ([layer isKindOfClass:CAShapeLayer.class]) [pieces addObject:(CAShapeLayer *)layer];
    if (pieces.count != 4) return nil;
    {
        // The same path and position interpolation drives production animation.
        // Percentages here are geometry progress, with no preview-only easing.
        [CATransaction begin]; [CATransaction setDisableActions:YES];
        for (NSUInteger i = 0; i < pieces.count; i++) {
            CGPathRef path = JasoMarkCreateInterpolatedPath(i, progress);
            pieces[i].path = path; CGPathRelease(path);
            pieces[i].position = JasoMarkPosition(i, progress); pieces[i].transform = CATransform3DIdentity;
        }
        [CATransaction commit];
    }
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef context = CGBitmapContextCreate(NULL, width, height, 8, width * 4, space, (CGBitmapInfo)kCGImageAlphaPremultipliedLast);
    CGColorSpaceRelease(space);
    if (!context) return nil;
    CGContextSetAllowsAntialiasing(context, true);
    CGContextSetShouldAntialias(context, true);
    // Both production paths and preview frame positions use the flipped view's
    // top-left coordinates. No screen, window, font, or GPU capture is involved.
    CGContextTranslateCTM(context, 0, height); CGContextScaleCTM(context, 1, -1);
    if (icon) {
        CGFloat inset = width / 16.0;
        CGPathRef tile = CGPathCreateWithRoundedRect(CGRectMake(inset, inset, width - 2 * inset, height - 2 * inset), width * 0.20, height * 0.20, NULL);
        CGContextSetRGBFillColor(context, 0.075, 0.49, 0.50, 1);
        CGContextAddPath(context, tile); CGContextFillPath(context); CGPathRelease(tile);
        CGRect bounds = CGRectNull;
        for (CAShapeLayer *piece in pieces) {
            CGPathRef stroke = piece.strokeColor && piece.lineWidth > 0 ?
                CGPathCreateCopyByStrokingPath(piece.path, NULL, piece.lineWidth, kCGLineCapRound, kCGLineJoinRound, 10) : CGPathRetain(piece.path);
            CGAffineTransform transform = PieceTransform(piece);
            CGPathRef transformed = CGPathCreateCopyByTransformingPath(stroke, &transform);
            bounds = CGRectUnion(bounds, CGPathGetPathBoundingBox(transformed));
            CGPathRelease(transformed); CGPathRelease(stroke);
        }
        CGFloat scale = MIN(width * 0.60 / bounds.size.width, height * 0.60 / bounds.size.height);
        CGContextTranslateCTM(context, width / 2.0 - CGRectGetMidX(bounds) * scale, height / 2.0 - CGRectGetMidY(bounds) * scale);
        CGContextScaleCTM(context, scale, scale);
    } else {
        CGFloat background = dark ? 0.15 : 0.96;
        CGContextSetRGBFillColor(context, background, background, background, 1);
        CGContextFillRect(context, CGRectMake(0, 0, width, height));
        CGContextScaleCTM(context, width / JasoMarkWidth, height / 22.0);
    }
    for (CAShapeLayer *piece in pieces) {
        CGContextSaveGState(context); CGContextConcatCTM(context, PieceTransform(piece));
        BOOL fill = piece.fillColor != NULL, stroke = piece.strokeColor != NULL && piece.lineWidth > 0;
        if (fill) {
            if (icon) CGContextSetRGBFillColor(context, 0.95, 0.99, 0.98, 1);
            else CGContextSetFillColorWithColor(context, piece.fillColor);
        }
        if (stroke) {
            if (icon) CGContextSetRGBStrokeColor(context, 0.95, 0.99, 0.98, 1);
            else CGContextSetStrokeColorWithColor(context, piece.strokeColor);
            CGContextSetLineWidth(context, piece.lineWidth);
            CGContextSetLineCap(context, kCGLineCapRound); CGContextSetLineJoin(context, kCGLineJoinRound);
        }
        if (fill || stroke) {
            BOOL evenOdd = [piece.fillRule isEqualToString:kCAFillRuleEvenOdd];
            CGPathDrawingMode mode = fill ? (stroke ? (evenOdd ? kCGPathEOFillStroke : kCGPathFillStroke) : (evenOdd ? kCGPathEOFill : kCGPathFill)) : kCGPathStroke;
            CGContextAddPath(context, piece.path); CGContextDrawPath(context, mode);
        }
        CGContextRestoreGState(context);
    }
    CGImageRef image = CGBitmapContextCreateImage(context);
    NSBitmapImageRep *bitmap = image ? [[NSBitmapImageRep alloc] initWithCGImage:image] : nil;
    if (image) CGImageRelease(image); CGContextRelease(context);
    return bitmap;
}

static NSData *PNG(NSBitmapImageRep *image) {
    return [image representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
}

static NSArray<NSNumber *> *PathTopology(CGPathRef path) {
    NSMutableArray<NSNumber *> *commands = [NSMutableArray array];
    CGPathApplyWithBlock(path, ^(const CGPathElement *element) { [commands addObject:@(element->type)]; });
    return commands;
}

static CGFloat SheetPanelWidth(void) { return JasoMarkWidth * 5 + 48; }
static CGRect SheetFrame(NSUInteger row, BOOL dark, NSUInteger zoom) {
    CGFloat panelX = 56 + (dark ? SheetPanelWidth() : 0);
    return CGRectMake(panelX + (zoom == 1 ? 8 : JasoMarkWidth + 24),
        40 + row * 112 + (zoom == 1 ? 33 : 0), JasoMarkWidth * zoom, 22 * zoom);
}

static void DrawLabel(CGContextRef context, CGFloat height, NSString *label, CGFloat x, CGFloat y, BOOL dark) {
    [NSGraphicsContext saveGraphicsState];
    NSGraphicsContext.currentContext = [NSGraphicsContext graphicsContextWithCGContext:context flipped:NO];
    [label drawAtPoint:NSMakePoint(x, height - y - 14) withAttributes:@{
        NSFontAttributeName: [NSFont systemFontOfSize:11],
        NSForegroundColorAttributeName: dark ? NSColor.whiteColor : NSColor.darkGrayColor
    }];
    [NSGraphicsContext restoreGraphicsState];
}

static NSBitmapImageRep *ContactSheet(void) {
    NSUInteger width = (NSUInteger)(64 + SheetPanelWidth() * 2), height = 608;
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef context = CGBitmapContextCreate(NULL, width, height, 8, width * 4, space, (CGBitmapInfo)kCGImageAlphaPremultipliedLast);
    CGColorSpaceRelease(space);
    if (!context) return nil;
    CGContextSetRGBFillColor(context, 0.96, 0.96, 0.96, 1); CGContextFillRect(context, CGRectMake(0, 0, width, height));
    CGContextSetRGBFillColor(context, 0.15, 0.15, 0.15, 1);
    CGContextFillRect(context, CGRectMake(56 + SheetPanelWidth(), 0, SheetPanelWidth() + 8, height));
    for (NSUInteger dark = 0; dark < 2; dark++) {
        CGFloat panelX = 56 + dark * SheetPanelWidth();
        DrawLabel(context, height, @"Actual size", panelX + 8, 12, dark);
        DrawLabel(context, height, @"4×", panelX + JasoMarkWidth + 24, 12, dark);
        for (NSUInteger row = 0; row < 5; row++) for (NSUInteger zoom = 1; zoom <= 4; zoom += 3) {
            CGRect frame = SheetFrame(row, dark, zoom);
            NSBitmapImageRep *image = RenderImage((NSUInteger)frame.size.width, (NSUInteger)frame.size.height, NO, row / 4.0, dark);
            if (!image) { CGContextRelease(context); return nil; }
            CGContextSetInterpolationQuality(context, kCGInterpolationNone);
            CGContextDrawImage(context, CGRectMake(frame.origin.x, height - CGRectGetMaxY(frame), frame.size.width, frame.size.height), image.CGImage);
        }
    }
    for (NSUInteger row = 0; row < 5; row++) DrawLabel(context, height, [NSString stringWithFormat:@"%lu%%", (unsigned long)(row * 25)], 8, 40 + row * 112 + 36, NO);
    CGImageRef image = CGBitmapContextCreateImage(context);
    NSBitmapImageRep *bitmap = image ? [[NSBitmapImageRep alloc] initWithCGImage:image] : nil;
    if (image) CGImageRelease(image); CGContextRelease(context);
    return bitmap;
}

static BOOL WriteImage(NSString *directory, NSString *name, NSBitmapImageRep *image) {
    NSData *data = PNG(image);
    NSError *error = nil;
    if (!data || ![data writeToFile:[directory stringByAppendingPathComponent:name] options:NSDataWritingAtomic error:&error]) {
        fprintf(stderr, "Cannot write %s: %s\n", name.UTF8String, error ? error.localizedDescription.UTF8String : "render failed");
        return NO;
    }
    return YES;
}

static BOOL WritePNG(NSString *directory, NSString *name, NSUInteger width, NSUInteger height, BOOL icon, CGFloat progress, BOOL dark) {
    return WriteImage(directory, name, RenderImage(width, height, icon, progress, dark));
}

static BOOL WriteIconset(NSString *directory) {
    const NSUInteger sizes[] = {16, 32, 128, 256, 512};
    for (NSUInteger i = 0; i < sizeof(sizes) / sizeof(sizes[0]); i++) for (NSUInteger factor = 1; factor <= 2; factor++) {
        NSString *name = [NSString stringWithFormat:@"icon_%lux%lu%@.png", (unsigned long)sizes[i], (unsigned long)sizes[i], factor == 2 ? @"@2x" : @""];
        if (!WritePNG(directory, name, sizes[i] * factor, sizes[i] * factor, YES, 1, NO)) return NO;
    }
    return YES;
}

static BOOL WritePreviews(NSString *directory) {
    for (NSUInteger dark = 0; dark < 2; dark++) for (NSUInteger step = 0; step < 5; step++) for (NSUInteger zoom = 1; zoom <= 8; zoom += 7) {
        NSString *state = step == 0 ? @"separated" : step == 4 ? @"joined" : [NSString stringWithFormat:@"%lupct", (unsigned long)(step * 25)];
        NSString *name = [NSString stringWithFormat:@"menu-%@-%@-%lux.png", dark ? @"dark" : @"light", state, (unsigned long)zoom];
        if (!WritePNG(directory, name, (NSUInteger)JasoMarkWidth * zoom, 22 * zoom, NO, step / 4.0, dark)) return NO;
    }
    return WritePNG(directory, @"application-icon-512.png", 512, 512, YES, 1, NO) &&
        WriteImage(directory, @"morph-contact-sheet.png", ContactSheet());
}

static BOOL WriteAnimation(NSString *path, BOOL dark) {
    const CFTimeInterval cycle = 12;
    const NSUInteger frames = (NSUInteger)lround(cycle * 18), scale = 4;
    NSURL *url = [NSURL fileURLWithPath:path];
    CGImageDestinationRef destination = CGImageDestinationCreateWithURL((__bridge CFURLRef)url, CFSTR("com.compuserve.gif"), frames, NULL);
    if (!destination) return NO;
    CGImageDestinationSetProperties(destination, (__bridge CFDictionaryRef)@{
        (__bridge NSString *)kCGImagePropertyGIFDictionary: @{(__bridge NSString *)kCGImagePropertyGIFLoopCount: @0}
    });
    for (NSUInteger frame = 0; frame < frames; frame++) @autoreleasepool {
        // GIF timing is quantized to centiseconds. Alternate 5/6cs intervals
        // instead of rounding every 18fps frame up and lengthening the cycle.
        CGFloat start = round(frame * cycle * 100 / frames), end = round((frame + 1) * cycle * 100 / frames);
        CGFloat fraction = start / (cycle * 100), progress = JasoMarkProgressAtCycleFraction(fraction);
        if (!isfinite(progress) || progress < 0 || progress > 1) { CFRelease(destination); return NO; }
        NSBitmapImageRep *image = RenderImage((NSUInteger)JasoMarkWidth * scale, 22 * scale, NO, progress, dark);
        if (!image) { CFRelease(destination); return NO; }
        NSDictionary *properties = @{
            (__bridge NSString *)kCGImagePropertyGIFDictionary: @{
                (__bridge NSString *)kCGImagePropertyGIFDelayTime: @((end - start) / 100.0),
                (__bridge NSString *)kCGImagePropertyGIFUnclampedDelayTime: @((end - start) / 100.0)
            }
        };
        CGImageDestinationAddImage(destination, image.CGImage, (__bridge CFDictionaryRef)properties);
    }
    BOOL written = CGImageDestinationFinalize(destination); CFRelease(destination);
    if (!written) return NO;
    CGImageSourceRef source = CGImageSourceCreateWithURL((__bridge CFURLRef)url, NULL);
    if (!source) return NO;
    BOOL valid = CGImageSourceGetCount(source) == frames;
    CGFloat duration = 0;
    for (NSUInteger frame = 0; frame < CGImageSourceGetCount(source); frame++) {
        NSDictionary *properties = CFBridgingRelease(CGImageSourceCopyPropertiesAtIndex(source, frame, NULL));
        NSDictionary *gif = properties[(__bridge NSString *)kCGImagePropertyGIFDictionary];
        NSNumber *delay = gif[(__bridge NSString *)kCGImagePropertyGIFUnclampedDelayTime] ?: gif[(__bridge NSString *)kCGImagePropertyGIFDelayTime];
        duration += delay.doubleValue;
    }
    CGImageRef first = CGImageSourceCreateImageAtIndex(source, 0, NULL);
    valid = valid && first && CGImageGetWidth(first) == (NSUInteger)JasoMarkWidth * scale && CGImageGetHeight(first) == 22 * scale && fabs(duration - cycle) < 0.01;
    if (first) CGImageRelease(first); CFRelease(source);
    if (!valid) fprintf(stderr, "GIF verification failed: expected %lu frames and %.3f seconds, got %.3f seconds\n", (unsigned long)frames, cycle, duration);
    return valid;
}

static BOOL StyleSelfTest(void) {
    NSMutableSet<NSData *> *frames = [NSMutableSet set];
    for (NSUInteger step = 0; step < 5; step++) {
        CGFloat progress = step / 4.0;
        NSData *frame = PNG(RenderImage((NSUInteger)JasoMarkWidth, 22, NO, progress, NO));
        if (frame) [frames addObject:frame];
    }
    if (frames.count != 5) {
        fprintf(stderr, "FAIL: previews must render five distinct production morph states\n"); return NO;
    }
    NSBitmapImageRep *sheet = ContactSheet();
    if (!sheet || sheet.pixelsWide < JasoMarkWidth * 10 || sheet.pixelsHigh < 5 * 88) {
        fprintf(stderr, "FAIL: contact sheet must include five actual-size and 4x frames in both appearances\n"); return NO;
    }
    for (NSUInteger dark = 0; dark < 2; dark++) for (NSUInteger row = 0; row < 5; row++) {
        CGRect frame = SheetFrame(row, dark, 1);
        NSBitmapImageRep *expected = RenderImage((NSUInteger)JasoMarkWidth, 22, NO, row / 4.0, dark);
        for (NSUInteger y = 0; y < 22; y++) for (NSUInteger x = 0; x < (NSUInteger)JasoMarkWidth; x++) {
            NSColor *actual = [[sheet colorAtX:(NSUInteger)frame.origin.x + x y:(NSUInteger)frame.origin.y + y] colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
            NSColor *pixel = [[expected colorAtX:x y:y] colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
            if (fabs(actual.redComponent - pixel.redComponent) > 0.005 ||
                fabs(actual.greenComponent - pixel.greenComponent) > 0.005 ||
                fabs(actual.blueComponent - pixel.blueComponent) > 0.005) {
                fprintf(stderr, "FAIL: contact-sheet frame differs from the actual-size production preview\n"); return NO;
            }
        }
    }
    for (NSUInteger i = 0; i < 4; i++) {
        CGPathRef independent = JasoMarkCreateInterpolatedPath(i, 0), composed = JasoMarkCreateInterpolatedPath(i, 1);
        BOOL interpolatable = [PathTopology(independent) isEqualToArray:PathTopology(composed)];
        BOOL changesVertices = !CGPathEqualToPath(independent, composed);
        CGPathRelease(independent); CGPathRelease(composed);
        if (!interpolatable || !changesVertices) {
            fprintf(stderr, "FAIL: each jamo must morph stroke vertices with compatible path commands\n"); return NO;
        }
        for (NSUInteger step = 0; step < 5; step++) {
            CGPathRef path = JasoMarkCreateInterpolatedPath(i, step / 4.0);
            CGRect bounds = CGPathGetPathBoundingBox(path);
            BOOL valid = !CGPathIsEmpty(path) && isfinite(bounds.origin.x) && isfinite(bounds.origin.y) &&
                isfinite(bounds.size.width) && isfinite(bounds.size.height) && bounds.size.width > 0 && bounds.size.height > 0;
            CGPathRelease(path);
            if (!valid) {
                fprintf(stderr, "FAIL: every intermediate component must retain finite nonempty geometry\n"); return NO;
            }
        }
    }
    NSBitmapImageRep *icon = RenderImage(128, 128, YES, 1, NO);
    if (!icon || icon.pixelsWide != 128 || icon.pixelsHigh != 128) {
        fprintf(stderr, "FAIL: icon renderer must return the requested dimensions\n"); return NO;
    }
    if ([icon colorAtX:0 y:0].alphaComponent != 0) {
        fprintf(stderr, "FAIL: rounded application icon requires transparent corners\n"); return NO;
    }
    NSUInteger teal = 0, ink = 0;
    for (NSUInteger y = 0; y < 128; y++) for (NSUInteger x = 0; x < 128; x++) {
        NSColor *color = [[icon colorAtX:x y:y] colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
        if (color.alphaComponent > 0.9 && color.greenComponent > color.redComponent + 0.15) teal++;
        if (color.alphaComponent > 0.9 && color.redComponent > 0.8 && color.greenComponent > 0.8) ink++;
    }
    if (teal < 8000 || ink < 150) {
        fprintf(stderr, "FAIL: icon must contain both its teal tile and composed vector mark\n"); return NO;
    }
    NSData *joined = PNG(RenderImage((NSUInteger)JasoMarkWidth, 22, NO, 1, NO));
    NSBitmapImageRep *separatedImage = RenderImage((NSUInteger)JasoMarkWidth, 22, NO, 0, NO);
    NSData *separated = PNG(separatedImage);
    NSData *dark = PNG(RenderImage((NSUInteger)JasoMarkWidth, 22, NO, 1, YES));
    if (!joined || !separated || !dark || [joined isEqualToData:separated] || [joined isEqualToData:dark]) {
        fprintf(stderr, "FAIL: production previews must distinguish composition and appearance\n"); return NO;
    }
    if (![PNG(icon) isEqualToData:PNG(RenderImage(128, 128, YES, 1, NO))]) {
        fprintf(stderr, "FAIL: repeated vector rendering must be deterministic\n"); return NO;
    }
    return YES;
}

static BOOL SelfTest(void) {
    NSUInteger available = 0; NSMutableSet<NSData *> *designs = [NSMutableSet set];
    for (NSInteger style = 0; style < 3; style++) {
        if (!JasoMarkStyleAvailable(style)) continue;
        available++; JasoMarkSetStyle(style);
        if (!StyleSelfTest()) return NO;
        [designs addObject:PNG(RenderImage((NSUInteger)JasoMarkWidth * 4, 88, NO, 1, NO))];
    }
    if (!available || designs.count != available) {
        fprintf(stderr, "FAIL: every available font style must produce a distinct design\n"); return NO;
    }
    NSString *animationPath = [NSTemporaryDirectory() stringByAppendingPathComponent:[NSString stringWithFormat:@"jaso-render-test-%@.gif", NSUUID.UUID.UUIDString]];
    BOOL animated = WriteAnimation(animationPath, NO);
    [NSFileManager.defaultManager removeItemAtPath:animationPath error:nil];
    if (!animated) {
        fprintf(stderr, "FAIL: native GIF encoding must preserve 216 frames and a 12-second cycle\n"); return NO;
    }
    printf("RenderIcon self-test passed (%lu available styles)\n", (unsigned long)available); return YES;
}

static BOOL WriteStylePreviews(NSString *directory) {
    NSArray<NSString *> *names = @[@"gothic", @"myungjo", @"gungseo"];
    NSUInteger available = 0;
    for (NSInteger style = 0; style < 3; style++) @autoreleasepool {
        if (!JasoMarkStyleAvailable(style)) {
            fprintf(stderr, "Skipping unavailable style: %s\n", names[style].UTF8String); continue;
        }
        available++; JasoMarkSetStyle(style);
        NSString *output = [directory stringByAppendingPathComponent:names[style]];
        NSError *error = nil;
        if (![NSFileManager.defaultManager createDirectoryAtPath:output withIntermediateDirectories:YES attributes:nil error:&error]) {
            fprintf(stderr, "Cannot create style preview directory: %s\n", error.localizedDescription.UTF8String); return NO;
        }
        if (!WritePreviews(output) || !WriteAnimation([output stringByAppendingPathComponent:@"morph-light-4x.gif"], NO) ||
            !WriteAnimation([output stringByAppendingPathComponent:@"morph-dark-4x.gif"], YES)) return NO;
        printf("Rendered %s style, including 216-frame, 12-second GIFs\n", names[style].UTF8String);
    }
    return available > 0;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc == 2 && strcmp(argv[1], "--self-test") == 0) return SelfTest() ? 0 : 1;
        if (argc == 3 && (strcmp(argv[1], "--iconset") == 0 || strcmp(argv[1], "--preview") == 0 || strcmp(argv[1], "--style-previews") == 0)) {
            NSString *directory = [NSString stringWithUTF8String:argv[2]];
            NSError *error = nil;
            if (!directory || ![NSFileManager.defaultManager createDirectoryAtPath:directory withIntermediateDirectories:YES attributes:nil error:&error]) {
                fprintf(stderr, "Cannot create output directory: %s\n", error ? error.localizedDescription.UTF8String : "invalid UTF-8 path"); return 1;
            }
            return (strcmp(argv[1], "--iconset") == 0 ? WriteIconset(directory) : strcmp(argv[1], "--style-previews") == 0 ? WriteStylePreviews(directory) : WritePreviews(directory)) ? 0 : 1;
        }
        fprintf(stderr, "usage: RenderIcon --iconset DIRECTORY | --preview DIRECTORY | --style-previews DIRECTORY | --self-test\n");
        return 2;
    }
}
