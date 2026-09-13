#import <Foundation/Foundation.h>

// Pure presentation of a status snapshot. No filesystem or process access.
// Returns title/subtitle/tone/symbol, three metrics (label/value/note), locations
// (title/path/state/detail/tone), notices (title/detail/tone), primaryTitle,
// primaryAction (start/resume/pause/empty), and progressNote.
FOUNDATION_EXPORT NSDictionary *JasoStatusPresentation(NSDictionary *snapshot, NSString *error, BOOL korean);

// Pure historical event presentation, independent of current retry samples.
// Returns title/detail/category, historicalIssue/resolved, path/pathActionAllowed,
// and resolvedAt (a validated timestamp or NSNull).
FOUNDATION_EXPORT NSDictionary *JasoActivityPresentation(NSDictionary *event, BOOL korean);
