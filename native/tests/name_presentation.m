// Headless: clang -fobjc-arc -framework Foundation native/tests/name_presentation.m
//   native/macos/NamePresentation.m -o build/name-presentation-test
#import <Foundation/Foundation.h>
#if __has_include("../macos/NamePresentation.h")
#import "../macos/NamePresentation.h"
#else
#define JASO_MISSING_NAME_PRESENTATION 1
extern NSDictionary *JasoNamePresentation(NSDictionary *) __attribute__((weak_import));
#endif
extern NSString *JasoHistoryRowIdentifier(NSDictionary *, NSString *) __attribute__((weak_import));

static NSUInteger checks;
static void Require(BOOL condition, NSString *message) {
    checks++;
    if (!condition) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}
static BOOL Exact(NSString *a, NSString *b) {
    return [[a dataUsingEncoding:NSUTF8StringEncoding] isEqual:[b dataUsingEncoding:NSUTF8StringEncoding]];
}
static NSDictionary *Present(NSString *before, NSString *after) {
    NSMutableDictionary *record=[@{@"id":@"original-operation",@"revision":@"original-revision",
        @"old_name":before,@"new_name":after,@"old_path":[@"/fixture/" stringByAppendingString:before],
        @"new_path":[@"/fixture/" stringByAppendingString:after]} mutableCopy];
    NSDictionary *original=record.copy;
    NSDictionary *result=JasoNamePresentation(record);
    Require([result[@"before"] isKindOfClass:NSString.class] && [result[@"after"] isKindOfClass:NSString.class],@"Both comparison strings must be present");
    Require([result[@"separated"] isKindOfClass:NSNumber.class],@"The explanation must depend on an actual separated letter");
    Require(record.count==original.count,@"Presentation must not add operational fields");
    for(NSString *key in original)Require(Exact(record[key],original[key]),[@"Presentation changed raw field " stringByAppendingString:key]);
    return result;
}
int main(void) {
    @autoreleasepool {
        @try {
#ifdef JASO_MISSING_NAME_PRESENTATION
            Require(JasoNamePresentation!=NULL,@"History needs the linked display-only name comparison helper");
#endif
            NSString *combined=@"한글.txt";
            NSString *separated=@"\u1112\u1161\u11AB\u1100\u1173\u11AF.txt";
            NSDictionary *forward=Present(separated,combined);
            Require(Exact(forward[@"before"],@"ㅎㅏㄴㄱㅡㄹ.txt"),@"Stored separated letters must remain visibly separate");
            Require(Exact(forward[@"after"],combined),@"Already combined syllables must not be decomposed");
            Require([forward[@"separated"] boolValue],@"Separated input needs the comparison explanation");
            NSDictionary *reverse=Present(combined,separated);
            Require(Exact(reverse[@"before"],combined)&&Exact(reverse[@"after"],@"ㅎㅏㄴㄱㅡㄹ.txt"),@"A restore must represent each side independently");
            NSDictionary *mixed=Present(@"한\u1100\u1173\u11AF (1)_1.xlsx",@"한글 (1)_1.xlsx");
            Require(Exact(mixed[@"before"],@"한ㄱㅡㄹ (1)_1.xlsx"),@"Only actually separated parts may expand");
            Require(Exact(mixed[@"after"],@"한글 (1)_1.xlsx"),@"The ordinary suffix and combined name must survive unchanged");
            NSDictionary *finals=Present([@"값 닭 꽃" decomposedStringWithCanonicalMapping],@"값 닭 꽃");
            Require(Exact(finals[@"before"],@"ㄱㅏㅄ ㄷㅏㄺ ㄲㅗㅊ"),@"Complex final consonants and doubled initial consonants must remain legible");
            NSString *other=@"Report  e\u0301-é-👩🏽‍💻-ᄔ-ㄱ.TXT";
            NSDictionary *unchanged=Present(other,other);
            Require(Exact(unchanged[@"before"],other)&&Exact(unchanged[@"after"],other),@"Other scripts, accents, emoji, spacing, archaic letters and compatibility letters must stay exact");
            Require(![unchanged[@"separated"] boolValue],@"Unexpanded names must not claim letters were separated");
            NSDictionary *emoji=Present(@"👩🏽‍💻 \u1112\u1161\u11AB-é.png",@"👩🏽‍💻 한-é.png");
            Require(Exact(emoji[@"before"],@"👩🏽‍💻 ㅎㅏㄴ-é.png")&&Exact(emoji[@"after"],@"👩🏽‍💻 한-é.png"),@"Expanding adjacent letters must preserve a complete emoji sequence");
            NSDictionary *empty=JasoNamePresentation(@{@"old_name":NSNull.null,@"new_name":@7});
            Require(Exact(empty[@"before"],@"")&&Exact(empty[@"after"],@""),@"Missing or malformed names must be safe to display");
            Require(JasoHistoryRowIdentifier!=NULL,@"History row focus needs a stable record-specific identifier");
            NSDictionary *first=@{@"id":@"first",@"old_name":separated,@"new_name":combined};
            NSDictionary *second=@{@"id":@"second",@"old_name":separated,@"new_name":combined};
            NSString *secondBefore=JasoHistoryRowIdentifier(second,@"before");
            Require(!Exact(JasoHistoryRowIdentifier(first,@"before"),secondBefore),@"Visually identical records must retain distinct focus identities");
            Require(!Exact(secondBefore,JasoHistoryRowIdentifier(second,@"after")),@"Both sides of one record need distinct focus identities");
            Require(Exact(secondBefore,JasoHistoryRowIdentifier(@{@"id":@"second",@"revision":@"refreshed",@"old_name":@"updated"},@"before")),@"The raw operation id must keep focus stable across refreshes");
            NSDictionary *legacy=@{@"operation_id":@"legacy-id",@"old_name":separated};
            Require(Exact(JasoHistoryRowIdentifier(legacy,@"before"),JasoHistoryRowIdentifier(legacy.copy,@"before")),@"Legacy record fallback must be deterministic");
            Require(!Exact(JasoHistoryRowIdentifier(@{@"old_path":@"/first"},@"before"),JasoHistoryRowIdentifier(@{@"old_path":@"/second"},@"before")),@"Path fallback must distinguish records without an operation id");
            printf("PASS %lu headless name-presentation assertions\n",(unsigned long)checks);
        } @catch(NSException *failure) {
            fprintf(stderr,"FAIL %s\n",failure.reason.UTF8String);return 1;
        }
    }
    return 0;
}
