#import "AnimatedMark.h"
#import <QuartzCore/QuartzCore.h>
#import <CoreText/CoreText.h>

const CGFloat JasoMarkWidth = 56;
const CGPoint JasoMarkSeparatedPositions[4] = {{28,11},{28,11},{28,11},{28,11}};
static const NSUInteger Samples = 256;
typedef struct { CGPoint from, to; } MorphPoint;
static NSArray<NSData *> *Morphs;
static NSInteger CurrentStyle = 1;
static NSString *TargetFont(NSInteger style) {
    return style == 2 ? @"GungSeo" : style == 1 ? @"AppleMyungjo" : @"AppleSDGothicNeo-Medium";
}
BOOL JasoMarkStyleAvailable(NSInteger style) { return [NSFont fontWithName:TargetFont(style) size:15] != nil; }
void JasoMarkSetStyle(NSInteger style) { CurrentStyle = MAX(0,MIN(2,style)); Morphs = nil; }

static void AppendPoint(NSMutableData *data, CGPoint point) { [data appendBytes:&point length:sizeof(point)]; }
static CGPoint Mix(CGPoint a, CGPoint b, CGFloat t) { return CGPointMake(a.x+(b.x-a.x)*t,a.y+(b.y-a.y)*t); }
static NSArray<NSData *> *Contours(CGPathRef path) {
    NSMutableArray *contours = [NSMutableArray array];
    __block NSMutableData *current;
    __block CGPoint previous = CGPointZero, first = CGPointZero;
    CGPathApplyWithBlock(path, ^(const CGPathElement *element) {
        if (element->type == kCGPathElementMoveToPoint) {
            current = [NSMutableData data]; [contours addObject:current];
            first = previous = element->points[0]; AppendPoint(current,first); return;
        }
        if (element->type == kCGPathElementCloseSubpath) { AppendPoint(current,first); previous=first; return; }
        NSUInteger steps = element->type == kCGPathElementAddLineToPoint ? 1 : 32;
        for (NSUInteger step=1;step<=steps;step++) {
            CGFloat t=(CGFloat)step/steps; CGPoint point;
            if (element->type == kCGPathElementAddCurveToPoint) {
                CGPoint a=Mix(previous,element->points[0],t), b=Mix(element->points[0],element->points[1],t), c=Mix(element->points[1],element->points[2],t);
                point=Mix(Mix(a,b,t),Mix(b,c,t),t);
            } else if (element->type == kCGPathElementAddQuadCurveToPoint) point=Mix(Mix(previous,element->points[0],t),Mix(element->points[0],element->points[1],t),t);
            else point=element->points[0];
            AppendPoint(current,point);
        }
        previous=element->points[element->type==kCGPathElementAddCurveToPoint?2:element->type==kCGPathElementAddQuadCurveToPoint?1:0];
    });
    return contours;
}
static CGRect Bounds(NSData *data) {
    const CGPoint *points=data.bytes; NSUInteger count=data.length/sizeof(CGPoint); CGRect bounds=CGRectNull;
    for (NSUInteger i=0;i<count;i++) bounds=CGRectUnion(bounds,CGRectMake(points[i].x,points[i].y,0,0));
    return bounds;
}
static NSData *Transform(NSData *data, CGAffineTransform transform) {
    NSMutableData *result=[data mutableCopy]; CGPoint *points=result.mutableBytes;
    for (NSUInteger i=0;i<result.length/sizeof(CGPoint);i++) points[i]=CGPointApplyAffineTransform(points[i],transform);
    return result;
}
static NSData *Resample(NSData *data) {
    const CGPoint *p=data.bytes; NSUInteger count=data.length/sizeof(CGPoint);
    NSMutableData *result=[NSMutableData dataWithLength:Samples*sizeof(CGPoint)]; CGPoint *out=result.mutableBytes;
    if (count<2) return result;
    NSMutableData *distances=[NSMutableData dataWithLength:count*sizeof(CGFloat)]; CGFloat *distance=distances.mutableBytes;
    for (NSUInteger i=1;i<count;i++) distance[i]=distance[i-1]+hypot(p[i].x-p[i-1].x,p[i].y-p[i-1].y);
    NSUInteger segment=1;
    for (NSUInteger i=0;i<Samples;i++) {
        CGFloat at=distance[count-1]*i/Samples;
        while (segment+1<count && distance[segment]<at) segment++;
        CGFloat length=distance[segment]-distance[segment-1];
        out[i]=Mix(p[segment-1],p[segment],length>0?(at-distance[segment-1])/length:0);
    }
    return result;
}
static CGFloat Distance(CGPoint a, CGPoint b) { CGFloat x=a.x-b.x,y=a.y-b.y; return x*x+y*y; }
static NSData *Normalize(NSData *data) {
    CGRect b=Bounds(data); CGFloat x=1/MAX(b.size.width,0.001),y=1/MAX(b.size.height,0.001);
    return Transform(data,CGAffineTransformMake(x,0,0,y,-b.origin.x*x,-b.origin.y*y));
}
// Match the outline's direction and start point, then align its local features.
// Repeated correspondence points keep serifs and stems from sliding around the
// contour when two fonts have different point counts. This runs only on setup.
static NSData *Correspondence(NSData *from, NSData *to) {
    NSData *a=Resample(from), *b=Resample(to), *na=Normalize(a), *nb=Normalize(b);
    const CGPoint *ap=a.bytes,*bp=b.bytes,*an=na.bytes,*bn=nb.bytes;
    CGFloat best=CGFLOAT_MAX; NSInteger shift=0,direction=1;
    for (NSInteger d=-1;d<=1;d+=2) for (NSUInteger offset=0;offset<Samples;offset++) {
        CGFloat cost=0;
        for (NSUInteger i=0;i<Samples;i++) cost+=Distance(an[i],bn[(offset+Samples+d*(NSInteger)i)%Samples]);
        if (cost<best) { best=cost; shift=offset; direction=d; }
    }
    CGPoint target[256], unit[256];
    for (NSUInteger i=0;i<Samples;i++) { NSUInteger j=(shift+Samples+direction*(NSInteger)i)%Samples; target[i]=bp[j]; unit[i]=bn[j]; }
    NSMutableData *matrix=[NSMutableData dataWithLength:Samples*Samples*sizeof(CGFloat)]; CGFloat *cost=matrix.mutableBytes;
    for (NSUInteger i=0;i<Samples;i++) for (NSUInteger j=0;j<Samples;j++) {
        CGFloat previous=i==0&&j==0?0:CGFLOAT_MAX/4;
        if (i&&j) previous=cost[(i-1)*Samples+j-1];
        if (i) previous=MIN(previous,cost[(i-1)*Samples+j]+0.002);
        if (j) previous=MIN(previous,cost[i*Samples+j-1]+0.002);
        cost[i*Samples+j]=previous+Distance(an[i],unit[j]);
    }
    MorphPoint reverse[512]; NSUInteger count=0,i=Samples-1,j=Samples-1;
    while (YES) {
        reverse[count++]=(MorphPoint){ap[i],target[j]}; if (!i&&!j) break;
        CGFloat diagonal=i&&j?cost[(i-1)*Samples+j-1]:CGFLOAT_MAX;
        CGFloat up=i?cost[(i-1)*Samples+j]+0.002:CGFLOAT_MAX;
        CGFloat left=j?cost[i*Samples+j-1]+0.002:CGFLOAT_MAX;
        if (diagonal<=up&&diagonal<=left) { i--;j--; } else if (up<=left) i--; else j--;
    }
    NSMutableData *result=[NSMutableData dataWithLength:count*sizeof(MorphPoint)]; MorphPoint *points=result.mutableBytes;
    for (NSUInteger k=0;k<count;k++) points[k]=reverse[count-k-1];
    return result;
}
static NSArray<NSData *> *GlyphContours(NSFont *font, UniChar character, CGSize *advance) {
    CGGlyph glyph; CTFontRef face=(__bridge CTFontRef)font;
    if (!font||!CTFontGetGlyphsForCharacters(face,&character,&glyph,1)||!glyph) return @[];
    if (advance) CTFontGetAdvancesForGlyphs(face,kCTFontOrientationHorizontal,&glyph,advance,1);
    CGPathRef path=CTFontCreatePathForGlyph(face,glyph,NULL); if (!path) return @[];
    NSArray *contours=Contours(path); CGPathRelease(path); return contours;
}
static void PreparePaths(void) {
    if (Morphs) return;
    NSFont *source=[NSFont fontWithName:@"AppleSDGothicNeo-Medium" size:15];
    NSFont *target=[NSFont fontWithName:TargetFont(CurrentStyle) size:15] ?: source;
    CGSize advance; NSArray *ja=GlyphContours(target,0xC790,&advance), *so=GlyphContours(target,0xC18C,NULL);
    // Reading order varies between font files: identify components geometrically.
    ja=[ja sortedArrayUsingComparator:^NSComparisonResult(NSData *a,NSData *b){ return CGRectGetMidX(Bounds(a))<CGRectGetMidX(Bounds(b))?NSOrderedAscending:NSOrderedDescending; }];
    so=[so sortedArrayUsingComparator:^NSComparisonResult(NSData *a,NSData *b){ return CGRectGetMidY(Bounds(a))>CGRectGetMidY(Bounds(b))?NSOrderedAscending:NSOrderedDescending; }];
    if (ja.count!=2||so.count!=2) { if (CurrentStyle!=0) { CurrentStyle=0; PreparePaths(); } return; }
    NSArray *parts=@[ja[0],ja[1],Transform(so[0],CGAffineTransformMakeTranslation(advance.width,0)),Transform(so[1],CGAffineTransformMakeTranslation(advance.width,0))];
    CGRect word=CGRectNull; for (NSData *part in parts) word=CGRectUnion(word,Bounds(part));
    CGFloat targetScale=MIN(1,13.5/MAX(word.size.height,0.001));
    CGAffineTransform joined=CGAffineTransformMake(targetScale,0,0,-targetScale,28-CGRectGetMidX(word)*targetScale,11+CGRectGetMidY(word)*targetScale);
    const UniChar characters[]={0x3148,0x314F,0x3145,0x3157}; const CGFloat centers[]={7,20.5,34,48};
    NSMutableArray *morphs=[NSMutableArray array];
    for (NSUInteger i=0;i<4;i++) {
        NSData *glyph=GlyphContours(source,characters[i],NULL).firstObject; CGRect box=Bounds(glyph);
        NSData *independent=Transform(glyph,CGAffineTransformMake(1,0,0,-1,centers[i]-CGRectGetMidX(box),11+CGRectGetMidY(box)));
        [morphs addObject:Correspondence(independent,Transform(parts[i],joined))];
    }
    Morphs=morphs;
}
CGPoint JasoMarkPosition(NSUInteger piece, CGFloat progress) { return CGPointMake(JasoMarkWidth/2,11); }
CFTimeInterval JasoMarkCycleDuration(void) { return 12; }
CGFloat JasoMarkProgressAtCycleFraction(CGFloat fraction) {
    CGFloat f=fraction-floor(fraction);
    if (f<0.1) return 0;
    if (f<0.35) { CGFloat u=(f-0.1)/0.25; return u*u*(3-2*u); }
    if (f<0.75) return 1;
    CGFloat u=(f-0.75)/0.25; return 1-u*u*(3-2*u);
}
CGPathRef JasoMarkCreateInterpolatedPath(NSUInteger piece, CGFloat progress) {
    PreparePaths(); CGMutablePathRef path=CGPathCreateMutable(); if (piece>=Morphs.count) return path;
    NSData *data=Morphs[piece]; const MorphPoint *points=data.bytes; CGFloat t=MAX(0,MIN(1,progress));
    for (NSUInteger i=0;i<data.length/sizeof(MorphPoint);i++) {
        CGPoint point=Mix(points[i].from,points[i].to,t);
        // The lower vowel travels below the consonant instead of brushing its
        // diagonal while the two components exchange their horizontal layout.
        point.y += (piece==3 ? 2.2 : piece==2 ? -0.8 : 0) * sin(M_PI*t);
        if (i==0) CGPathMoveToPoint(path,NULL,point.x,point.y); else CGPathAddLineToPoint(path,NULL,point.x,point.y);
    }
    CGPathCloseSubpath(path); return path;
}
CGPathRef JasoMarkCreatePath(NSUInteger piece, BOOL joined) { return JasoMarkCreateInterpolatedPath(piece,joined?1:0); }

@interface JasoAnimatedMark ()
@property NSArray<CAShapeLayer *> *pieces;
@end
@implementation JasoAnimatedMark
- (BOOL)isFlipped { return YES; }
- (NSView *)hitTest:(NSPoint)point { return nil; }
- (instancetype)initWithFrame:(NSRect)frame {
    if ((self=[super initWithFrame:frame])) {
        self.wantsLayer=YES; NSMutableArray *pieces=[NSMutableArray array];
        for (NSUInteger i=0;i<4;i++) {
            CAShapeLayer *piece=[CAShapeLayer layer]; piece.bounds=CGRectMake(0,0,JasoMarkWidth,22); piece.lineWidth=0; piece.strokeColor=nil;
            [pieces addObject:piece]; [self.layer addSublayer:piece];
        }
        self.pieces=pieces;
        [NSWorkspace.sharedWorkspace.notificationCenter addObserver:self selector:@selector(accessibilityChanged:) name:NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification object:nil];
        [self updateColors];
    }
    return self;
}
- (void)dealloc { [NSWorkspace.sharedWorkspace.notificationCenter removeObserver:self]; }
- (void)viewDidChangeEffectiveAppearance { [super viewDidChangeEffectiveAppearance]; [self updateColors]; }
- (void)updateColors {
    [self.effectiveAppearance performAsCurrentDrawingAppearance:^{
        [CATransaction begin]; [CATransaction setDisableActions:YES];
        for (CAShapeLayer *piece in self.pieces) piece.fillColor=NSColor.labelColor.CGColor;
        [CATransaction commit];
    }];
}
- (void)setAnimationEnabled:(BOOL)enabled { _animationEnabled=enabled; [self refreshAnimation]; }
- (void)accessibilityChanged:(NSNotification *)notification { [self refreshAnimation]; }
- (void)refreshAnimation {
    BOOL animate=self.animationEnabled&&!NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion;
    CFTimeInterval start=CACurrentMediaTime();
    [CATransaction begin]; [CATransaction setDisableActions:YES];
    for (NSUInteger i=0;i<self.pieces.count;i++) {
        CAShapeLayer *piece=self.pieces[i]; [piece removeAllAnimations];
        CGPathRef independent=JasoMarkCreatePath(i,NO),composed=JasoMarkCreatePath(i,YES);
        piece.position=JasoMarkPosition(i,1); piece.path=composed; piece.opacity=1; piece.transform=CATransform3DIdentity;
        if (animate) {
            CAKeyframeAnimation *shape=[CAKeyframeAnimation animationWithKeyPath:@"path"];
            NSMutableArray *values=[NSMutableArray arrayWithObjects:(__bridge id)independent,(__bridge id)independent,nil];
            CGFloat inStart=0.1,inEnd=0.35,outStart=0.75;
            NSMutableArray *times=[NSMutableArray arrayWithObjects:@0,@(inStart),nil];
            for (NSUInteger step=1;step<=12;step++) {
                CGFloat u=(CGFloat)step/12, time=inStart+(inEnd-inStart)*u, progress=JasoMarkProgressAtCycleFraction(time);
                CGPathRef p=JasoMarkCreateInterpolatedPath(i,progress); [values addObject:(__bridge id)p]; CGPathRelease(p);
                [times addObject:@(time)];
            }
            [values addObject:(__bridge id)composed]; [times addObject:@(outStart)];
            for (NSUInteger step=1;step<=12;step++) {
                CGFloat u=(CGFloat)step/12, time=outStart+(1-outStart)*u, progress=step==12?0:JasoMarkProgressAtCycleFraction(time);
                CGPathRef p=JasoMarkCreateInterpolatedPath(i,progress); [values addObject:(__bridge id)p]; CGPathRelease(p);
                [times addObject:@(time)];
            }
            shape.values=values; shape.keyTimes=times; shape.calculationMode=kCAAnimationLinear;
            shape.duration=JasoMarkCycleDuration(); shape.repeatCount=HUGE_VALF; shape.beginTime=start;
            [piece addAnimation:shape forKey:@"compose-shape"];

        }
        CGPathRelease(independent); CGPathRelease(composed);
    }
    [CATransaction commit];
}
@end
