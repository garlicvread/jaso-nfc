#import "Localization.h"

NSString *JasoLanguagePreference(void) {
    NSString *value = [NSUserDefaults.standardUserDefaults stringForKey:@"interfaceLanguage"];
    return [@[@"ko", @"en"] containsObject:value ?: @""] ? value : @"system";
}
void JasoSetLanguagePreference(NSString *value) {
    [NSUserDefaults.standardUserDefaults setObject:[@[@"ko", @"en"] containsObject:value ?: @""] ? value : @"system" forKey:@"interfaceLanguage"];
}
BOOL JasoUsesKorean(void) {
    NSString *value = JasoLanguagePreference();
    return [value isEqual:@"ko"] || ([value isEqual:@"system"] && [[NSLocale preferredLanguages].firstObject hasPrefix:@"ko"]);
}
NSString *JasoText(NSString *english, NSString *korean) { return JasoUsesKorean() ? korean : english; }
