#import <Foundation/Foundation.h>

// Display-only comparison of a history record. Returns before/after strings
// and a separated boolean. Never use these strings as filenames or paths.
FOUNDATION_EXPORT NSDictionary *JasoNamePresentation(NSDictionary *record);

// Stable identity for one side of one history row, independent of display text.
FOUNDATION_EXPORT NSString *JasoHistoryRowIdentifier(NSDictionary *record, NSString *side);
