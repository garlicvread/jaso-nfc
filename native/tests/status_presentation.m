// clang -fobjc-arc -framework Foundation native/tests/status_presentation.m \
//   native/macos/StatusPresentation.m -o /tmp/jaso-status-presentation-test
#import <Foundation/Foundation.h>
#import <errno.h>
#if __has_include("../macos/StatusPresentation.h")
#import "../macos/StatusPresentation.h"
#else
#define JASO_MISSING_PRESENTATION 1
extern NSDictionary *JasoStatusPresentation(NSDictionary *, NSString *, BOOL) __attribute__((weak_import));
#endif

static NSUInteger checks;
static void Require(BOOL condition, NSString *message) {
    checks++;
    if (!condition) @throw [NSException exceptionWithName:@"TestFailure" reason:message userInfo:nil];
}

static NSDictionary *Snapshot(NSDictionary *changes) {
    NSMutableDictionary *value = [@{@"running":@YES, @"paused":@NO, @"apply":@YES,
        @"baseline_complete":@YES, @"needs_revalidation":@NO, @"pending_recovery":@NO,
        @"indexed_entries":@12345, @"indexed_directories":@500, @"pending_jobs":@0,
        @"deferred_jobs":@0, @"deferred_renames":@0, @"errors":@0,
        @"pending_baseline_roots":@[], @"next_retry":NSNull.null,
        @"roots":@[@"/Users/example/Documents"], @"active_roots":@[@"/Users/example/Documents"],
        @"unavailable_roots":@{}, @"catalog_unavailable":@{}} mutableCopy];
    [value addEntriesFromDictionary:changes];
    return value;
}

static NSString *VisibleText(id value) {
    if ([value isKindOfClass:NSString.class]) return value;
    NSMutableArray *parts = [NSMutableArray array];
    if ([value isKindOfClass:NSDictionary.class]) {
        for (id item in [value allValues]) [parts addObject:VisibleText(item)];
    } else if ([value isKindOfClass:NSArray.class]) {
        for (id item in value) [parts addObject:VisibleText(item)];
    }
    return [parts componentsJoinedByString:@" "];
}

static BOOL Contains(id value, NSString *text) {
    return [VisibleText(value) rangeOfString:text options:NSCaseInsensitiveSearch].location != NSNotFound;
}

static NSDictionary *Present(NSDictionary *changes) {
    return JasoStatusPresentation(Snapshot(changes), nil, NO);
}

static NSDictionary *Location(NSDictionary *presentation, NSString *path) {
    for (NSDictionary *location in presentation[@"locations"])
        if ([location[@"path"] isEqual:path]) return location;
    return nil;
}

static void CheckShape(NSDictionary *result) {
    for (NSString *key in @[@"title", @"subtitle", @"tone", @"symbol", @"primaryTitle", @"primaryAction", @"progressNote"])
        Require([result[key] isKindOfClass:NSString.class], [@"String field: " stringByAppendingString:key]);
    Require([@[@"good", @"working", @"warning", @"neutral"] containsObject:result[@"tone"]], @"Known overall tone");
    Require([@[@"", @"start", @"resume", @"pause"] containsObject:result[@"primaryAction"]], @"Known primary action");
    Require([result[@"metrics"] count] == 3, @"Exactly three primary metrics");
    for (NSDictionary *metric in result[@"metrics"])
        for (NSString *key in @[@"label", @"value", @"note"])
            Require([metric[key] isKindOfClass:NSString.class], @"Complete metric");
    for (NSDictionary *location in result[@"locations"])
        for (NSString *key in @[@"title", @"path", @"state", @"detail", @"tone"])
            Require([location[key] isKindOfClass:NSString.class], @"Complete location");
    for (NSDictionary *notice in result[@"notices"])
        for (NSString *key in @[@"title", @"detail", @"tone"])
            Require([notice[key] isKindOfClass:NSString.class], @"Complete notice");
    Require(!Contains(result, @"baseline_complete") && !Contains(result, @"pending_jobs"), @"No raw status schema");
    Require(!Contains(result, @"%") && ![VisibleText(result) containsString:@"ETA"], @"No invented progress denominator or ETA");
}

int main(void) {
    @autoreleasepool {
        @try {
#ifdef JASO_MISSING_PRESENTATION
            Require(JasoStatusPresentation != NULL, @"The human-readable presentation model is linked");
#endif
            NSDictionary *good = Present(@{});
            CheckShape(good);
            Require([good[@"tone"] isEqual:@"good"], @"Confirmed active idle worker is healthy");
            Require(Contains(good[@"title"], @"Watching"), @"Healthy title describes ongoing watching");
            Require([good[@"primaryAction"] isEqual:@"pause"], @"Running worker offers pause");
            Require([good[@"metrics"][0][@"value"] isEqual:@"12,345"], @"Indexed entries formatted as count");
            Require(Contains(good[@"metrics"][0][@"note"], @"files") && Contains(good[@"metrics"][0][@"note"], @"folders"), @"Indexed entries include files and folders");
            Require([good[@"locations"][0][@"path"] isEqual:@"/Users/example/Documents"], @"Location retains full path");

            NSDictionary *indexing = Present(@{@"baseline_complete":@NO, @"pending_jobs":@0});
            Require([indexing[@"tone"] isEqual:@"working"] && Contains(indexing[@"title"], @"index"), @"Zero queue does not complete initial indexing");
            NSDictionary *starting = @{@"indexed":@NO, @"running":@YES, @"paused":@NO, @"apply":@YES};
            NSDictionary *preparing = JasoStatusPresentation(starting, nil, NO);
            Require([preparing[@"title"] isEqual:@"Preparing the index"], @"A starting worker with no committed index has a preparation state");
            Require([preparing[@"metrics"][0][@"value"] isEqual:@"—"], @"Preparation does not invent an indexed count");
            Require([JasoStatusPresentation(starting, nil, YES)[@"title"] isEqual:@"인덱스 준비 중"], @"Preparation is localized");
            NSDictionary *queued = Present(@{@"pending_jobs":@17, @"deferred_jobs":@4});
            Require([queued[@"tone"] isEqual:@"working"], @"Queued directory work is processing");
            Require([queued[@"metrics"][1][@"value"] isEqual:@"17"], @"Deferred jobs are not added twice");
            Require(Contains(queued[@"notices"], @"4") && Contains(queued[@"notices"], @"capacity"), @"Deferred directory work has separate capacity explanation");

            NSDictionary *paused = Present(@{@"paused":@YES, @"pending_jobs":@25});
            Require(Contains(paused[@"title"], @"Paused") && [paused[@"primaryAction"] isEqual:@"resume"], @"Paused worker offers resume");
            Require(Contains(paused[@"subtitle"], @"record") && ![paused[@"tone"] isEqual:@"working"], @"Paused status explains event recording without active progress");
            NSDictionary *stopped = Present(@{@"running":@NO, @"paused":@YES});
            Require(Contains(stopped[@"title"], @"Stopped") && [stopped[@"primaryAction"] isEqual:@"start"], @"Stopped takes precedence over stale pause flag");
            Require(!Contains(stopped[@"locations"][0][@"state"], @"Watching") && Contains(stopped[@"locations"], @"saved"), @"Stopped coverage is historical, not active monitoring");

            NSDictionary *preview = Present(@{@"apply":@NO, @"baseline_complete":@NO, @"pending_jobs":@3});
            Require(Contains(preview[@"title"], @"Preview") && Contains(preview[@"subtitle"], @"Manage folders") && Contains(preview[@"subtitle"], @"Start automatic cleanup"), @"Preview explains how to review names and begin cleanup");
            Require(Contains(preview[@"progressNote"], @"index"), @"Preview still reports initial index state");
            NSDictionary *history = Present(@{@"errors":@801});
            Require([history[@"tone"] isEqual:@"good"] && [history[@"metrics"][2][@"value"] isEqual:@"0"], @"Historical errors do not imply current failed renames");
            Require(Contains(history[@"notices"], @"801") && Contains(history[@"notices"], @"cumulative"), @"Historical errors explicitly cumulative");
            NSDictionary *retries = Present(@{@"deferred_renames":@2, @"next_retry":@1000, @"pending_jobs":@1});
            Require([retries[@"metrics"][2][@"value"] isEqual:@"2"] && [retries[@"tone"] isEqual:@"working"], @"A retry count alone must not demand user intervention");
            Require(Contains(retries[@"notices"], @"Directory retry"), @"Delayed reconciliation retries are visible separately");
            Require([Present(@{@"next_retry":@1000, @"pending_jobs":@1})[@"tone"] isEqual:@"working"], @"Scheduled automatic retries use a waiting state");
            NSDictionary *lockedCloud = Present(@{@"deferred_renames":@1, @"rename_retry_items":@[@{
                @"path":@"/Users/example/Library/CloudStorage/GoogleDrive-example/.tmp/123/file.txt", @"reason":@"1", @"locked":@YES, @"attempts":@1, @"next_retry":@1000}]});
            Require([lockedCloud[@"tone"] isEqual:@"working"], @"A locked cloud item waits rather than demanding manual unlock");
            Require([lockedCloud[@"issues"] count] == 1 && Contains(lockedCloud[@"issues"], @"Wait for the sync app to release its managed lock"), @"Cloud lock guidance leaves managed locks to the sync app");
            Require([lockedCloud[@"issues"][0][@"action"] isEqual:@"reveal"] && Contains(lockedCloud[@"issues"][0][@"actionTitle"], @"Finder"), @"An issue offers the actual file location");
            NSString *cloudFile = @"/Users/example/Library/CloudStorage/GoogleDrive-example/.tmp/123/file.txt";
            for (NSString *directoryPath in @[cloudFile, cloudFile.stringByDeletingLastPathComponent]) {
                NSDictionary *pairedCloudFailures = Snapshot(@{@"deferred_renames":@1, @"directory_retry_count":@1, @"next_retry":@1000,
                    @"rename_retry_items":@[@{@"path":cloudFile, @"reason":@"1", @"locked":@YES, @"attempts":@1, @"next_retry":@1000}],
                    @"directory_retry_items":@[@{@"path":directoryPath, @"reason":@"Operation not permitted (os error 1)", @"attempts":@1, @"next_retry":@1000}]});
                for (NSNumber *korean in @[@NO, @YES]) {
                    NSDictionary *paired = JasoStatusPresentation(pairedCloudFailures, nil, korean.boolValue);
                    Require([paired[@"tone"] isEqual:@"working"], @"A duplicate cloud EPERM must not override the saved lock's automatic wait guidance");
                    Require([paired[@"issues"] count] == 2, @"Both saved retry operations remain visible");
                    NSDictionary *directoryIssue = paired[@"issues"][1];
                    Require(![directoryIssue[@"requiresAction"] boolValue] && [directoryIssue[@"category"] isEqual:@"retry"], @"Bare cloud EPERM does not establish permission denial or a lock");
                    Require(!Contains(directoryIssue, @"Full Disk Access") && !Contains(directoryIssue, @"전체 디스크 접근"), @"Bare cloud EPERM cannot diagnose missing privacy permission");
                    Require(Contains(directoryIssue, korean.boolValue ? @"자동으로 재시도" : @"retry automatically"), @"Both languages explain automatic retry for an unspecified cloud refusal");
                }
            }
            NSDictionary *unknownCloud = Present(@{@"deferred_renames":@1, @"rename_retry_items":@[@{@"path":cloudFile, @"reason":@"1"}]});
            Require([unknownCloud[@"tone"] isEqual:@"working"] && [unknownCloud[@"issues"][0][@"category"] isEqual:@"retry"], @"A cloud rename EPERM without saved lock evidence stays uncertain");
            NSDictionary *cloudAccess = Present(@{@"directory_retry_items":@[@{@"path":cloudFile.stringByDeletingLastPathComponent, @"reason":@"Permission denied (os error 13)"}]});
            Require([cloudAccess[@"tone"] isEqual:@"warning"] && [cloudAccess[@"issues"][0][@"category"] isEqual:@"permission"], @"Cloud EACCES retains access-permission guidance");
            NSDictionary *localPermission = Present(@{@"rename_retry_items":@[@{@"path":@"/Users/example/Documents/file.txt", @"reason":@"1"}]});
            Require([localPermission[@"tone"] isEqual:@"warning"] && [localPermission[@"issues"][0][@"category"] isEqual:@"permission"], @"Non-cloud EPERM retains permission guidance");
            NSDictionary *permission = Present(@{@"deferred_renames":@1, @"rename_retry_items":@[@{
                @"path":@"/Users/example/Documents/file.txt", @"reason":@"13", @"attempts":@3, @"next_retry":@1000}]});
            Require([permission[@"tone"] isEqual:@"warning"] && Contains(permission[@"title"], @"permission"), @"Permission failures name the needed action");
            Require(Contains(permission[@"issues"], @"Get Info") && Contains(permission[@"issues"], @"Full Disk Access"), @"Permission advice distinguishes ownership from privacy access");
            NSDictionary *busy = Present(@{@"directory_retry_count":@1, @"next_retry":@1000, @"pending_jobs":@1, @"directory_retry_items":@[@{
                @"path":@"/Users/example/Library/CloudStorage/OneDrive/Documents", @"reason":@"Resource deadlock avoided (os error 11)", @"attempts":@2, @"next_retry":@1000}]});
            Require([busy[@"tone"] isEqual:@"working"] && Contains(busy[@"issues"], @"automatically"), @"Temporary cloud read failure explains automatic retry");
            Require(!Contains(busy[@"issues"], @"Full Disk Access"), @"A busy file is not misdiagnosed as missing permission");
            for (NSNumber *korean in @[@NO, @YES]) {
                NSString *accountPath = @"/Users/example/Library/CloudStorage/Nextcloud-account/Documents";
                for (NSString *reason in @[@"81", @"Need authenticator (os error 81)", @"[Errno 81] Need authenticator", @"Authentication error (os error 80)"]) {
                    for (NSString *source in @[@"directory_retry_items", @"rename_retry_items"]) {
                        NSDictionary *auth = JasoStatusPresentation(Snapshot(@{source:@[@{@"path":accountPath, @"reason":reason, @"attempts":@1}]}), nil, korean.boolValue);
                        NSDictionary *issue = auth[@"issues"][0];
                        Require([issue[@"category"] isEqual:@"authentication"] && [issue[@"requiresAction"] boolValue], @"Authentication errors require an account check even on their first attempt");
                        Require([auth[@"tone"] isEqual:@"warning"] && Contains(auth[@"title"], korean.boolValue ? @"인증" : @"authentication"), @"Authentication failures must be visible in the summary");
                        Require(Contains(issue[@"detail"], korean.boolValue ? @"동기화 앱" : @"sync app") && Contains(issue[@"detail"], korean.boolValue ? @"해당 계정" : @"this account"), @"Authentication advice identifies the affected account and app");
                        Require(Contains(issue[@"detail"], korean.boolValue ? @"현재 로그인·연결 안내" : @"current sign-in and connection messages"), @"Authentication guidance asks the user to check the current account state");
                        Require(!Contains(issue, @"Full Disk Access") && !Contains(issue, @"전체 디스크 접근"), @"Account authentication is not a privacy-permission diagnosis");
                        Require(Contains(auth[@"issueSummary"], korean.boolValue ? @"계정" : @"account"), @"The summary must not direct an authentication failure to filename or permission repair");
                    }
                    for (NSString *source in @[@"unavailable_roots", @"catalog_unavailable"]) {
                        NSDictionary *location = Location(JasoStatusPresentation(Snapshot(@{source:@{accountPath:reason}}), nil, korean.boolValue), accountPath);
                        Require(Contains(location[@"state"], korean.boolValue ? @"인증" : @"authentication"), @"Root and catalog authentication failures use the same guidance");
                    }
                }
                for (NSString *reason in @[@"Operation timed out (os error 60)", @"Interrupted system call (os error 4)", @"Provider returned an unspecified failure"]) {
                    NSDictionary *repeated = JasoStatusPresentation(Snapshot(@{@"baseline_complete":@NO, @"needs_revalidation":@YES,
                        @"directory_retry_items":@[@{@"path":accountPath, @"reason":reason, @"attempts":@2}]}), nil, korean.boolValue);
                    NSDictionary *issue = repeated[@"issues"][0];
                    Require([issue[@"requiresAction"] boolValue] && [repeated[@"tone"] isEqual:@"warning"], @"Repeated failures must not remain a passive waiting state behind indexing");
                    Require(Contains(issue[@"title"], korean.boolValue ? @"반복" : @"Repeated"), @"Repeated failures are distinguished in their titles");
                    if ([reason hasSuffix:@"(os error 60)"] || [reason hasSuffix:@"(os error 4)"])
                        Require(Contains(issue[@"title"], korean.boolValue ? @"마지막 확인" : @"last check"), @"The failure count does not establish repeated occurrences of the latest error");
                    Require(Contains(issue[@"detail"], korean.boolValue ? @"해당 계정" : @"this account"), @"Repeated cloud failure guidance checks the affected account rather than only the app process");
                    Require(!Contains(issue, @"restart the worker") && !Contains(issue, @"작업을 다시 시작"), @"Retrying a provider failure does not establish a reason to restart the worker");
                    Require(!Contains(issue, @"signed out") && !Contains(issue, @"로그아웃") && !Contains(issue, @"Full Disk Access"), @"Repeated failure does not prove account logout or missing permissions");
                }
                for (id reason in @[@"provider failure 81", @"provider failure (os error 810)", @"path /folder (os error 81)/child", NSNull.null, @81]) {
                    NSDictionary *unknown = JasoStatusPresentation(Snapshot(@{@"directory_retry_items":@[@{@"path":accountPath, @"reason":reason}]}), nil, korean.boolValue);
                    Require(![unknown[@"issues"][0][@"category"] isEqual:@"authentication"], @"Unstructured numbers and path text must not fabricate an authentication diagnosis");
                }
                NSDictionary *authWithLock = JasoStatusPresentation(Snapshot(@{@"rename_retry_items":@[@{
                    @"path":accountPath, @"reason":@"81", @"locked":@YES, @"attempts":@1}]}), nil, korean.boolValue);
                Require([authWithLock[@"issues"][0][@"category"] isEqual:@"authentication"] && [authWithLock[@"issues"][0][@"requiresAction"] boolValue], @"A saved lock must not hide an explicitly reported authentication error");
            }
            for (NSNumber *korean in @[@NO, @YES]) {
                for (NSString *reason in @[[NSString stringWithFormat:@"%d", ETIMEDOUT],
                        [NSString stringWithFormat:@"Operation timed out (os error %d)", ETIMEDOUT]]) {
                    NSDictionary *timedOut = JasoStatusPresentation(Snapshot(@{@"directory_retry_count":@1,
                        @"directory_retry_items":@[@{@"path":@"/Users/example/Library/CloudStorage/Nextcloud-account/folder",
                            @"reason":reason, @"attempts":@1, @"next_retry":@1000}]}), nil, korean.boolValue);
                    NSDictionary *issue = timedOut[@"issues"][0];
                    Require([issue[@"category"] isEqual:@"timeout"] && ![issue[@"requiresAction"] boolValue], @"Slow directory responses are identified separately from permissions");
                    Require(Contains(issue, korean.boolValue ? @"다른 폴더" : @"Other folders") && Contains(issue, korean.boolValue ? @"동기화 앱" : @"sync app"), @"Timeout advice explains continued processing and the provider connection to check");
                    Require(!Contains(issue, @"Full Disk Access") && !Contains(issue, @"전체 디스크 접근"), @"A timeout must not imply missing privacy permission");
                }
            }
            NSDictionary *collision = Present(@{@"deferred_renames":@1, @"rename_retry_items":@[@{
                @"path":@"/Users/example/Documents/file.txt", @"reason":@"17", @"attempts":@1}]});
            Require(Contains(collision[@"issues"], @"conflict") && Contains(collision[@"issues"], @"this rename was deferred") && Contains(collision[@"issues"], @"compare the two items"), @"Collision advice explains the deferred rename and how to resolve it");
            NSDictionary *badIssues = Present(@{@"rename_retry_items":@[NSNull.null, @{@"path":@"relative", @"reason":@"13"}, @{@"path":@"/valid", @"reason":NSNull.null, @"locked":@"true"}], @"directory_retry_items":NSNull.null});
            Require([badIssues[@"issues"] count] == 1 && !Contains(badIssues[@"issues"], @"Get Info"), @"Malformed details cannot invent a permission diagnosis or reveal a relative path");
            NSDictionary *koreanIssues = JasoStatusPresentation(Snapshot(@{@"deferred_renames":@1, @"rename_retry_items":@[@{@"path":@"/Users/example/file", @"reason":@"13"}]}), nil, YES);
            Require(Contains(koreanIssues[@"issues"], @"권한") && Contains(koreanIssues[@"issues"][0][@"actionTitle"], @"Finder"), @"Issue advice and actions are localized");
            NSMutableArray *manyIssues = [NSMutableArray array];
            for (NSUInteger i=0; i<25; i++) [manyIssues addObject:@{@"path":[NSString stringWithFormat:@"/item/%lu",(unsigned long)i], @"reason":@"11", @"next_retry":@1e300}];
            NSDictionary *bounded = Present(@{@"deferred_renames":@25, @"directory_retry_count":@25, @"rename_retry_items":manyIssues, @"directory_retry_items":manyIssues});
            Require([bounded[@"issues"] count] == 16 && Contains(bounded[@"issueSummary"], @"16 of 50"), @"Large retry lists are bounded with an explicit partial-list count");
            Require(!Contains(bounded[@"issues"], @"(null)"), @"Invalid retry dates cannot display an invalid formatter result");
            NSDictionary *pausedRetry = Present(@{@"paused":@YES, @"deferred_renames":@1, @"rename_retry_items":@[@{@"path":@"/item", @"reason":@"11"}]});
            Require(Contains(pausedRetry[@"issueSummary"], @"resume"), @"Paused retries explain the required resume action");
            Require(![Present(@{@"running":NSNull.null, @"apply":@NO})[@"title"] isEqual:preview[@"title"]], @"Missing running state cannot claim an active preview worker");
            Require(![Present(@{@"apply":NSNull.null, @"baseline_complete":@NO})[@"tone"] isEqual:@"working"], @"Missing mode is presented as incomplete information");

            NSDictionary *unavailable = Present(@{@"unavailable_roots":@{@"/Volumes/Drive":@"raw failure {\"code\": 12}"},
                @"catalog_unavailable":@{@"/Users":@"raw catalog exception"}});
            Require([unavailable[@"tone"] isEqual:@"warning"], @"Coverage failure prevents healthy status");
            Require(Contains(unavailable[@"locations"], @"/Volumes/Drive") && Contains(unavailable[@"locations"], @"/Users"), @"Unavailable roots and discovery paths are listed");
            Require(Contains(unavailable, @"discovery") && !Contains(unavailable, @"raw failure") && !Contains(unavailable, @"raw catalog"), @"Human coverage guidance takes precedence over exceptions");
            NSString *unavailablePath = @"/Volumes/Drive";
            for (NSNumber *korean in @[@NO, @YES]) {
                for (NSString *source in @[@"unavailable_roots", @"catalog_unavailable"]) {
                    for (NSString *reason in @[[NSString stringWithFormat:@"%d", ETIMEDOUT],
                            [NSString stringWithFormat:@"Operation timed out (os error %d)", ETIMEDOUT],
                            [NSString stringWithFormat:@"[Errno %d] Operation timed out", ETIMEDOUT],
                            [NSString stringWithFormat:@"Reading /Volumes/Drive (os error 13)/folder: Operation timed out (os error %d)", ETIMEDOUT]]) {
                        NSDictionary *timedOut = JasoStatusPresentation(Snapshot(@{source:@{unavailablePath:reason}}), nil, korean.boolValue);
                        NSDictionary *location = Location(timedOut, unavailablePath);
                        Require(Contains(location[@"state"], korean.boolValue ? @"시간 초과" : @"timed out"), @"Unavailable roots and catalogs name the actual response timeout");
                        Require(Contains(location[@"detail"], korean.boolValue ? @"자동으로 재시도" : @"retry automatically") &&
                            Contains(location[@"detail"], korean.boolValue ? @"연결" : @"connection"), @"Unavailable timeout advice explains automatic retry and checking the connection in both languages");
                        Require(!Contains(location, @"permission") && !Contains(location, @"권한"), @"An unavailable timeout does not suggest permission changes");
                        Require([location[@"path"] isEqual:unavailablePath] && [location[@"tone"] isEqual:@"warning"], @"Unavailable locations retain their Finder path and coverage warning");
                    }
                    for (NSString *reason in @[@"4", @"Opening root directory: Interrupted system call (os error 4)", @"[Errno 4] Interrupted system call"]) {
                        NSDictionary *interrupted = Location(JasoStatusPresentation(Snapshot(@{source:@{unavailablePath:reason}}), nil, korean.boolValue), unavailablePath);
                        Require(Contains(interrupted[@"state"], korean.boolValue ? @"확인 중단" : @"Check interrupted"), @"An interrupted root or catalog check must not appear as an unknown cause");
                        Require(Contains(interrupted[@"detail"], korean.boolValue ? @"자동으로 재시도" : @"retry automatically"), @"Interrupted checks explain the next automatic action");
                        Require(!Contains(interrupted, @"Full Disk Access") && !Contains(interrupted, @"전체 디스크 접근"), @"An interrupted check does not establish missing privacy permission");
                    }
                    for (NSString *reason in @[@"Permission denied (os error 13)", @"[Errno 13] Permission denied"]) {
                        NSDictionary *denied = Location(JasoStatusPresentation(Snapshot(@{source:@{unavailablePath:reason}}), nil, korean.boolValue), unavailablePath);
                        Require(Contains(denied[@"state"], korean.boolValue ? @"권한" : @"permissions") && Contains(denied[@"detail"], korean.boolValue ? @"공유 및 사용 권한" : @"Sharing & Permissions"), @"Unavailable access denial keeps actionable permission guidance");
                    }
                    for (id reason in @[@"provider failure 60", @"provider failure (os error 600)", @"[Errno 600] Unknown error", @"raw failure {\"code\": 13}", @"Unavailable path /Volumes/Drive (os error 13)/folder", @"Unavailable path /Volumes/[Errno 13] folder", NSNull.null, @60]) {
                        NSDictionary *unknown = Location(JasoStatusPresentation(Snapshot(@{source:@{unavailablePath:reason}}), nil, korean.boolValue), unavailablePath);
                        Require(!Contains(unknown, @"permission") && !Contains(unknown, @"권한") && !Contains(unknown[@"state"], @"timed out") && !Contains(unknown[@"state"], @"시간 초과"), @"Malformed or unrelated error numbers do not invent a cause");
                        Require(Contains(unknown[@"detail"], korean.boolValue ? @"원인은 아직 확인되지 않았습니다" : @"cause is not yet known"), @"An unknown coverage error explicitly leaves the cause uncertain");
                    }
                }
                NSDictionary *dual = Location(JasoStatusPresentation(Snapshot(@{
                    @"unavailable_roots":@{unavailablePath:@"Operation timed out (os error 60)"},
                    @"catalog_unavailable":@{unavailablePath:@"Permission denied (os error 13)"}}), nil, korean.boolValue), unavailablePath);
                Require(Contains(dual, korean.boolValue ? @"시간 초과" : @"timed out") && Contains(dual[@"detail"], korean.boolValue ? @"공유 및 사용 권한" : @"Sharing & Permissions"), @"Distinct root and discovery failures on one path preserve both causes");
            }
            NSDictionary *recovery = Present(@{@"pending_recovery":@YES});
            Require([recovery[@"tone"] isEqual:@"warning"] && Contains(recovery, @"recovery"), @"Pending journal receives explicit recovery warning");
            Require(!Contains(recovery[@"title"], @"Watching"), @"Pending recovery is never a healthy summary");
            NSDictionary *revalidating = Present(@{@"needs_revalidation":@YES});
            Require(![revalidating[@"tone"] isEqual:@"good"] && Contains(revalidating, @"recheck"), @"Revalidation is not silently treated as complete");

            NSDictionary *loading = JasoStatusPresentation(nil, nil, NO);
            CheckShape(loading);
            Require(Contains(loading[@"title"], @"Loading") && [loading[@"primaryAction"] isEqual:@""], @"No snapshot starts in loading state without unsafe action");
            NSDictionary *error = JasoStatusPresentation(nil, @"{raw:exception}", NO);
            Require([error[@"tone"] isEqual:@"warning"] && !Contains(error, @"raw:exception"), @"Read failure is a human warning without raw diagnostics");
            NSDictionary *empty = JasoStatusPresentation(@{}, nil, NO);
            CheckShape(empty);
            Require(![empty[@"tone"] isEqual:@"good"] && [empty[@"primaryAction"] isEqual:@""], @"Missing running flag cannot fabricate a start action or healthy state");
            for (NSDictionary *metric in empty[@"metrics"]) Require([metric[@"value"] isEqual:@"—"], @"Absent count is a dash");
            NSDictionary *malformed = JasoStatusPresentation((NSDictionary *)@[@1], nil, NO);
            CheckShape(malformed);
            Require([malformed[@"tone"] isEqual:@"warning"], @"Malformed top level is unavailable");
            NSDictionary *nulls = Present(@{@"running":NSNull.null, @"paused":@"false", @"apply":@[],
                @"indexed_entries":@YES, @"pending_jobs":@"20", @"deferred_renames":@(-1),
                @"roots":@[@1, NSNull.null, @"/Valid"], @"active_roots":NSNull.null,
                @"unavailable_roots":NSNull.null, @"catalog_unavailable":@[], @"pending_baseline_roots":NSNull.null});
            CheckShape(nulls);
            for (NSDictionary *metric in nulls[@"metrics"]) Require([metric[@"value"] isEqual:@"—"], @"Invalid or boolean counts are unknown");
            Require([nulls[@"primaryAction"] isEqual:@""], @"Malformed controls produce no action");
            NSDictionary *unknownBaseline = Present(@{@"baseline_complete":NSNull.null});
            Require(![unknownBaseline[@"tone"] isEqual:@"good"], @"Missing baseline flag does not imply completion");
            NSDictionary *noCoverage = Present(@{@"active_roots":@[]});
            Require(![noCoverage[@"tone"] isEqual:@"good"], @"No active roots cannot claim healthy monitoring");
            NSDictionary *pendingRoots = Present(@{@"baseline_complete":@NO, @"pending_baseline_roots":@[@"/Volumes/New"]});
            Require(Contains(pendingRoots[@"locations"], @"/Volumes/New"), @"Pending baseline root remains visible");

            NSDictionary *korean = JasoStatusPresentation(Snapshot(@{@"errors":@3, @"deferred_renames":@2}), nil, YES);
            CheckShape(korean);
            Require(Contains(korean[@"metrics"][0][@"label"], @"인덱스") && Contains(korean[@"notices"], @"누적"), @"Korean status and historical error labels are localized");
            Require(!Contains(korean[@"title"], @"retry") && !Contains(korean[@"subtitle"], @"folder"), @"Korean human summary has no English fallback");
            printf("PASS: %lu status presentation assertions\n", (unsigned long)checks);
            return 0;
        } @catch (NSException *exception) {
            fprintf(stderr, "FAIL: %s\n", exception.reason.UTF8String);
            return 1;
        }
    }
}
