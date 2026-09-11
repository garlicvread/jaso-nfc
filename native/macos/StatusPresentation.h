#import <Foundation/Foundation.h>

// Pure presentation of a status snapshot. No filesystem or process access.
// Returns title/subtitle/tone/symbol, three metrics (label/value/note), locations
// (title/path/state/detail/tone), notices (title/detail/tone), primaryTitle,
// primaryAction (start/resume/pause/empty), and progressNote.
FOUNDATION_EXPORT NSDictionary *JasoStatusPresentation(NSDictionary *snapshot, NSString *error, BOOL korean);
