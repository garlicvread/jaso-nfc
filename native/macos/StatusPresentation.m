#import "StatusPresentation.h"
#import <math.h>
#import <errno.h>

static NSString *Text(BOOL korean, NSString *english, NSString *translation) {
    return korean ? translation : english;
}

static NSDictionary *Dictionary(id value) {
    return [value isKindOfClass:NSDictionary.class] ? value : nil;
}

// JSON booleans must not accidentally become item counts; strings are unknown.
static NSNumber *Count(id value) {
    if (![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID()) return nil;
    double number = [value doubleValue];
    return isfinite(number) && number >= 0 && floor(number) == number ? value : nil;
}

static NSNumber *Flag(id value) {
    if (![value isKindOfClass:NSNumber.class]) return nil;
    double number = [value doubleValue];
    return number == 0 || number == 1 ? @([value boolValue]) : nil;
}

static NSArray<NSString *> *Paths(id value) {
    if (![value isKindOfClass:NSArray.class]) return @[];
    NSMutableOrderedSet *paths = [NSMutableOrderedSet orderedSet];
    for (id path in value)
        if ([path isKindOfClass:NSString.class] && [path length]) [paths addObject:path];
    return paths.array;
}

static NSArray<NSString *> *FailurePaths(id value) {
    return Paths(Dictionary(value).allKeys);
}

static NSString *Number(NSNumber *value) {
    if (!value) return @"—";
    NSNumberFormatter *formatter = [NSNumberFormatter new];
    formatter.locale = [NSLocale localeWithLocaleIdentifier:@"en_US"];
    formatter.numberStyle = NSNumberFormatterDecimalStyle;
    formatter.maximumFractionDigits = 0;
    return [formatter stringFromNumber:value] ?: @"—";
}

static NSDictionary *Notice(NSString *title, NSString *detail, NSString *tone) {
    return @{@"title":title, @"detail":detail, @"tone":tone};
}

static BOOL HasError(NSString *reason, int code) {
    // Match serialized errors, not numbers or error-like text inside a path.
    if (reason.isAbsolutePath || [reason hasPrefix:@"file:"]) return NO;
    return [reason isEqual:[NSString stringWithFormat:@"%d", code]] ||
        [reason hasPrefix:[NSString stringWithFormat:@"[Errno %d]", code]] ||
        [reason hasSuffix:[NSString stringWithFormat:@"(os error %d)", code]];
}

// Classification is shared by current retry advice and historical observations.
// Only the former may recommend a next action or promise an automatic retry.
static NSString *FailureCategory(NSString *path, NSString *reason, BOOL locked) {
    BOOL cloud = [path containsString:@"/Library/CloudStorage/"] || [path containsString:@"/Library/Mobile Documents/"];
    BOOL authentication = HasError(reason, EAUTH) || HasError(reason, ENEEDAUTH);
    if ([reason isEqual:@"dataless-file"] || [reason isEqual:@"dataless file deferred; download state is not verified"]) return @"cloud-only";
    if (locked && !authentication) return @"locked";
    if (authentication) return @"authentication";
    if (cloud && HasError(reason, EPERM)) return @"cloud-refused";
    if (HasError(reason, EACCES) || HasError(reason, EPERM)) return @"permission";
    if (HasError(reason, EEXIST)) return @"conflict";
    if (HasError(reason, EROFS)) return @"readonly";
    if (HasError(reason, ENOSPC) || ([reason hasPrefix:@"Critical storage: fewer than "] && [reason containsString:@" MiB available for "])) return @"storage";
    if (HasError(reason, EINTR)) return @"interrupted";
    if (HasError(reason, ETIMEDOUT)) return @"timeout";
    if (HasError(reason, EBUSY) || HasError(reason, EAGAIN) || HasError(reason, EDEADLK)) return @"busy";
    if (HasError(reason, ENOENT) || HasError(reason, ENODEV)) return @"unavailable";
    return @"retry";
}

static NSDictionary *FailureAdvice(NSString *path, id savedReason, BOOL locked, BOOL repeated, BOOL korean) {
    NSString *reason = [savedReason isKindOfClass:NSString.class] ? savedReason : @"";
    BOOL cloud = [path containsString:@"/Library/CloudStorage/"] || [path containsString:@"/Library/Mobile Documents/"];
    NSString *cause = FailureCategory(path, reason, locked);
    NSString *category = @"retry";
    NSString *title = Text(korean, @"Waiting for another attempt", @"다시 시도할 때까지 보류");
    NSString *detail = Text(korean, @"The cause is not yet known. Cleanup will retry automatically. If this keeps happening, open the location in Finder and check whether the item is accessible.", @"원인은 아직 확인되지 않았습니다. 자동으로 재시도합니다. 계속되면 Finder에서 이 위치를 열어 항목에 접근할 수 있는지 확인하세요.");
    BOOL required = NO;
    if ([cause isEqual:@"cloud-only"]) {
        category = @"cloud-only";
        title = Text(korean, @"Stored in the cloud", @"클라우드에 보관 중");
        detail = Text(korean, @"Filename cleanup resumes when the file is available on this Mac.", @"이 Mac에 파일이 저장되면 이름을 정리합니다.");
    } else if ([cause isEqual:@"locked"]) {
        category = @"locked";
        title = cloud ? Text(korean, @"Cloud item is locked", @"클라우드 항목 잠금으로 보류") : Text(korean, @"Check the file or folder lock", @"파일 또는 폴더 잠금 확인");
        detail = cloud
            ? Text(korean, @"The file or its parent folder was locked at the last attempt. Wait for the sync app to release its managed lock. If this persists, check the sync app and the item in Finder.", @"마지막 시도에서 파일 또는 상위 폴더가 잠겨 있었습니다. 동기화 앱이 관리하는 잠금은 해당 앱이 해제할 때까지 기다려 주세요. 계속되면 동기화 앱과 Finder에서 항목 상태를 확인하세요.")
            : Text(korean, @"The file or its parent folder was locked at the last attempt. In Finder → Get Info, check Locked. You can change a lock you set yourself; let the owning app release any lock it manages.", @"마지막 시도에서 파일 또는 상위 폴더가 잠겨 있었습니다. Finder → 정보 가져오기에서 ‘잠김’을 확인하세요. 직접 설정한 잠금은 변경할 수 있으며, 앱이 관리하는 잠금은 해당 앱이 해제할 때까지 기다려 주세요.");
        required = !cloud;
    } else if ([cause isEqual:@"authentication"]) {
        category = @"authentication"; required = YES;
        title = Text(korean, @"Check account authentication", @"계정 인증 확인 필요");
        detail = cloud
            ? Text(korean, @"The last check reported an authentication problem. Open the sync app and check this account's current sign-in and connection messages. Follow its instructions to restore access; cleanup will then check the location again.", @"마지막 확인에서 인증 오류가 발생했습니다. 동기화 앱에서 해당 계정의 현재 로그인·연결 안내를 확인하세요. 앱의 안내에 따라 접근을 복구하면 이 위치를 다시 확인합니다.")
            : Text(korean, @"The last check reported an authentication problem. In Finder or the service app, check the current sign-in and connection messages for the account used here. Follow its instructions to restore access; cleanup will then check this location again.", @"마지막 확인에서 인증 오류가 발생했습니다. Finder 또는 해당 서비스 앱에서 이 위치에 연결하는 계정의 현재 로그인·연결 안내를 확인하세요. 안내에 따라 접근을 복구하면 이 위치를 다시 확인합니다.");
    } else if ([cause isEqual:@"cloud-refused"]) {
        // A cloud rename can also leave a directory retry without its saved
        // lock signature. EPERM alone does not identify the cause.
        title = Text(korean, @"Cloud operation was not permitted", @"클라우드 작업이 허용되지 않음");
        detail = Text(korean, @"The cause is not yet known. Wait for syncing to finish; cleanup will retry automatically. If this persists, check the sync app and open the item in Finder.", @"원인은 아직 확인되지 않았습니다. 동기화가 끝날 때까지 기다려 주세요. 자동으로 재시도합니다. 계속되면 동기화 앱을 확인하고 Finder에서 항목을 열어 보세요.");
    } else if ([cause isEqual:@"permission"]) {
        category = @"permission"; required = YES;
        title = Text(korean, @"Check access permissions", @"접근 권한 확인 필요");
        detail = Text(korean, @"In Finder → Get Info, check Sharing & Permissions and the lock for this item and its parent folder. For protected folders, also check Full Disk Access in Settings. Each of these controls can affect access.", @"Finder → 정보 가져오기에서 이 항목과 상위 폴더의 ‘공유 및 사용 권한’과 잠금을 확인하세요. 보호된 폴더라면 설정의 전체 디스크 접근 권한도 확인하세요. 각각의 설정이 접근에 영향을 줄 수 있습니다.");
    } else if ([cause isEqual:@"conflict"]) {
        category = @"conflict"; required = YES;
        title = Text(korean, @"Resolve a filename conflict", @"파일 이름 충돌 확인 필요");
        detail = Text(korean, @"Another item already uses the proposed name, so this rename was deferred. Open the folder in Finder, compare the two items, and give one a distinct name if needed. Cleanup will retry afterward.", @"정리 후 이름을 다른 항목이 사용하고 있어 이번 이름 변경을 보류했습니다. Finder에서 폴더를 열어 두 항목을 비교하고, 필요한 경우 하나를 다른 이름으로 바꾸세요. 이후 자동으로 재시도합니다.");
    } else if ([cause isEqual:@"readonly"]) {
        category = @"readonly"; required = YES;
        title = Text(korean, @"Location is read-only", @"읽기 전용 위치");
        detail = Text(korean, @"This location is read-only. Use a copy in a writable location, or check the drive's write access in Finder.", @"이 위치는 읽기 전용입니다. 쓰기 가능한 위치의 사본을 사용하거나 Finder에서 드라이브의 쓰기 권한을 확인하세요.");
    } else if ([cause isEqual:@"storage"]) {
        category = @"storage"; required = YES;
        title = Text(korean, @"Insufficient disk space", @"디스크 여유 공간 부족");
        detail = Text(korean, @"The operation reported insufficient free disk space. Check Storage in Settings and free up space before trying again.", @"작업에서 디스크 여유 공간 부족이 확인되었습니다. 설정에서 저장 공간을 확인하고 여유 공간을 확보한 뒤 다시 시도해 주세요.");
    } else if ([cause isEqual:@"interrupted"]) {
        category = @"interrupted";
        title = Text(korean, @"Check interrupted — automatic retry", @"확인 중단 · 자동 재시도");
        detail = Text(korean, @"The folder check was interrupted before it finished. Cleanup will retry automatically.", @"폴더 확인 도중 작업이 중단되었습니다. 자동으로 재시도합니다.");
    } else if ([cause isEqual:@"timeout"]) {
        category = @"timeout";
        title = Text(korean, @"Response timed out — automatic retry", @"응답 시간 초과 · 자동 재시도");
        detail = cloud
            ? Text(korean, @"The last check timed out; the cause is still unknown. Other folders can continue processing; this one will retry automatically. If this repeats, check this account's connection in the sync app and open the folder in Finder.", @"마지막 확인에서 응답 시간이 초과되었으며 원인은 아직 알 수 없습니다. 다른 폴더는 계속 처리하며 이 위치는 자동으로 재시도합니다. 반복되면 동기화 앱에서 해당 계정의 연결 상태를 확인하고 Finder에서 이 폴더를 열어 보세요.")
            : Text(korean, @"This location took too long to respond. Other folders can continue processing; this one will retry automatically. If this repeats, check the drive or network connection.", @"이 위치의 응답 시간이 초과되었습니다. 다른 폴더는 계속 처리하며 이 위치는 자동으로 재시도합니다. 반복되면 드라이브나 네트워크 연결을 확인하세요.");
    } else if ([cause isEqual:@"busy"]) {
        category = @"busy";
        title = Text(korean, @"Temporarily busy — automatic retry", @"일시적으로 사용 중 · 자동 재시도");
        detail = Text(korean, @"This item was temporarily busy at the last attempt. Cleanup will retry automatically. For cloud files, wait for syncing to finish. If this repeats, try opening the item in Finder.", @"마지막 시도에서 이 항목을 일시적으로 사용할 수 없었습니다. 자동으로 재시도합니다. 클라우드 파일은 동기화가 끝날 때까지 기다려 주세요. 반복되면 Finder에서 항목을 열어 보세요.");
    } else if ([cause isEqual:@"unavailable"]) {
        category = @"unavailable";
        title = Text(korean, @"Waiting for the location to return", @"위치를 다시 사용할 때까지 대기");
        detail = Text(korean, @"The item or drive was unavailable at the last attempt. Reconnect it if needed, or wait for cloud syncing. Cleanup will check it again automatically.", @"마지막 시도에서 항목이나 드라이브를 사용할 수 없었습니다. 필요한 경우 다시 연결하거나 클라우드 동기화를 기다려 주세요. 자동으로 다시 확인합니다.");
    }
    if (repeated && ([category isEqual:@"timeout"] || [category isEqual:@"interrupted"] || [category isEqual:@"retry"])) {
        required = YES;
        // Directory attempts count all failures, not repetitions of this errno.
        if ([category isEqual:@"timeout"]) title = Text(korean, @"Repeated failures — last check timed out", @"실패 반복 · 마지막 확인 시간 초과");
        else if ([category isEqual:@"interrupted"]) title = Text(korean, @"Repeated failures — last check interrupted", @"실패 반복 · 마지막 확인 중단");
        else title = Text(korean, @"Repeated failure — check this location", @"실패 반복 · 해당 위치 확인");
        detail = [detail stringByAppendingFormat:@" %@", cloud
            ? Text(korean, @"Several attempts have failed. Open the sync app and check this account's status, then try opening this location in Finder.", @"여러 차례 시도한 뒤에도 실패했습니다. 동기화 앱에서 해당 계정의 상태를 확인한 뒤 Finder에서 이 위치를 열어 보세요.")
            : Text(korean, @"Several attempts have failed. Open this location in Finder and check the drive or network connection.", @"여러 차례 시도한 뒤에도 실패했습니다. Finder에서 이 위치를 열어 보고 드라이브나 네트워크 연결을 확인하세요.")];
    }
    return @{@"title":title, @"detail":detail, @"requiresAction":@(required), @"category":category};
}

static NSString *ActivityPhaseTitle(NSString *phase, BOOL korean) {
    NSDictionary *names=@{
        @"starting":Text(korean, @"Starting cleanup",@"정리를 시작하고 있습니다"),
        @"discovering_sources":Text(korean, @"Checking selected locations",@"선택한 위치 확인 중"),
        @"opening_directory":Text(korean, @"Opening folder metadata",@"폴더 정보 확인 중"),
        @"reading_metadata":Text(korean, @"Checking file metadata",@"파일 정보 확인 중"),
        @"processing":Text(korean, @"Checking a file name",@"파일 이름 확인 중"),
        @"normalizing":Text(korean, @"Checking a file name",@"파일 이름 확인 중"),
        @"directory_read":Text(korean, @"Reading folder metadata",@"폴더 목록 확인 중"),
        @"enumerating":Text(korean, @"Reading folder metadata",@"폴더 목록 확인 중"),
        @"scanning":Text(korean, @"Checking file names",@"파일 이름 확인 중"),
        @"metadata":Text(korean, @"Checking metadata",@"파일 정보 확인 중"),
        @"observation":Text(korean, @"Checking a file",@"파일 확인 중"),
        @"renamed":Text(korean, @"Name changed",@"이름 변경 완료"),
        @"restored":Text(korean, @"Original name restored",@"원래 이름으로 되돌림 완료"),
        @"deferred":Text(korean, @"Will check again",@"다시 확인할 항목"),
        @"error":Text(korean, @"Could not complete this check",@"검사를 마치지 못한 항목"),
        @"renaming":Text(korean, @"Renaming a file",@"파일 이름 변경 중"),
        @"restoring":Text(korean, @"Restoring the original name",@"원래 이름으로 되돌리는 중"),
        @"checking_storage":Text(korean, @"Checking available disk space",@"디스크 여유 공간 확인 중"),
        @"updating_history":Text(korean, @"Updating change history",@"변경 기록 갱신 중"),
        @"low_storage":Text(korean, @"Cleanup is waiting for disk space",@"디스크 공간 확보를 기다리고 있습니다"),
        @"waiting_for_events":Text(korean, @"Ready for new files",@"새 파일을 확인할 준비가 됐습니다"),
        @"idle":Text(korean, @"Ready for new files",@"새 파일을 확인할 준비가 됐습니다"),
        @"paused":Text(korean, @"Paused",@"일시 정지"),
        @"stopping":Text(korean, @"Stopping cleanup",@"정리를 중지하는 중"),
        @"stopped":Text(korean, @"Cleanup is stopped",@"정리가 중지되었습니다"),
        @"recovering":Text(korean, @"Checking an interrupted change",@"중단된 이름 변경 확인 중")
    };
    return names[phase]?:Text(korean, @"Activity details unavailable",@"활동 내용을 확인할 수 없습니다");
}

static NSNumber *ActivityErrno(id value) {
    if (![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID()) return nil;
    double number = [value doubleValue];
    return isfinite(number) && floor(number) == number && number >= INT32_MIN && number <= INT32_MAX ? value : nil;
}

static NSNumber *ActivityTimestamp(id value) {
    if (![value isKindOfClass:NSNumber.class] || CFGetTypeID((__bridge CFTypeRef)value) == CFBooleanGetTypeID()) return nil;
    double number = [value doubleValue];
    return isfinite(number) && number > 0 && number <= NSDate.distantFuture.timeIntervalSince1970 ? value : nil;
}

static NSString *ActivityTime(NSNumber *value, BOOL korean) {
    NSDateFormatter *formatter = [NSDateFormatter new];
    formatter.locale = [NSLocale localeWithLocaleIdentifier:korean ? @"ko_KR" : @"en_US"];
    formatter.dateStyle = NSDateFormatterShortStyle;
    formatter.timeStyle = NSDateFormatterMediumStyle;
    return [formatter stringFromDate:[NSDate dateWithTimeIntervalSince1970:value.doubleValue]];
}

NSDictionary *JasoActivityPresentation(NSDictionary *event, BOOL korean) {
    event = Dictionary(event);
    NSString *kind = [event[@"kind"] isKindOfClass:NSString.class] ? event[@"kind"] : @"";
    NSString *phase = [event[@"phase"] isKindOfClass:NSString.class] ? event[@"phase"] : @"";
    NSString *path = [event[@"path"] isKindOfClass:NSString.class] ? event[@"path"] : @"";
    BOOL historicalIssue = [@[@"error", @"deferred", @"waiting", @"wait"] containsObject:kind];
    BOOL safePath = path.isAbsolutePath && [path rangeOfString:@"\0"].location == NSNotFound &&
        ![path.pathComponents containsObject:@".."] && ![path.pathComponents containsObject:@"."] &&
        ![path hasSuffix:@"…"] && ![Flag(event[@"path_truncated"]) boolValue];
    // Unknown truncation metadata cannot establish a safe identity either.
    if (event[@"path_truncated"] && event[@"path_truncated"] != NSNull.null && !Flag(event[@"path_truncated"])) safePath = NO;
    NSString *title = ActivityPhaseTitle([@[@"renamed", @"restored"] containsObject:kind] ? kind : phase, korean);
    NSString *detail = Text(korean, @"This records the activity at the time shown.", @"표시된 시각에 기록된 활동입니다.");
    NSString *category = @"activity";
    if (historicalIssue) {
        NSString *reason = [event[@"reason"] isKindOfClass:NSString.class] ? event[@"reason"] : @"";
        NSNumber *error = ActivityErrno(event[@"errno"]);
        category = FailureCategory(path, error ? error.stringValue : reason, NO);
        NSDictionary *titles = @{
            @"cloud-only":Text(korean, @"Stored in the cloud", @"클라우드에 보관 중"),
            @"permission":Text(korean, @"Access was denied", @"접근 권한 오류"),
            @"authentication":Text(korean, @"Account authentication failed", @"계정 인증 오류"),
            @"cloud-refused":Text(korean, @"Cloud operation was not permitted", @"클라우드 작업이 허용되지 않음"),
            @"conflict":Text(korean, @"Rename deferred by a filename conflict", @"이름 충돌로 변경 보류"),
            @"readonly":Text(korean, @"Change blocked by a read-only location", @"읽기 전용 위치로 변경 보류"),
            @"storage":Text(korean, @"Insufficient disk space", @"디스크 여유 공간 부족"),
            @"interrupted":Text(korean, @"Check was interrupted", @"검사 도중 중단"),
            @"timeout":Text(korean, @"Response timed out", @"응답 시간 초과"),
            @"busy":Text(korean, @"Item was temporarily busy", @"당시 항목을 일시적으로 사용할 수 없었음"),
            @"unavailable":Text(korean, @"Item or location was unavailable", @"당시 항목 또는 위치를 사용할 수 없었음")};
        NSDictionary *details = @{
            @"cloud-only":Text(korean, @"At this check, the file was stored in the cloud. Its availability on this Mac had not been verified.", @"당시 파일은 클라우드에 보관되어 있었으며, 이 Mac에 저장되어 있는지는 확인되지 않았습니다."),
            @"permission":Text(korean, @"Access was denied during this check. The recorded error does not identify which access control denied it.", @"당시 검사에서 접근 권한 오류가 발생했습니다. 기록된 오류만으로는 어떤 접근 설정이 영향을 주었는지 알 수 없습니다."),
            @"authentication":Text(korean, @"An account authentication error prevented this operation at the time shown.", @"표시된 시각에 계정 인증 오류로 작업을 진행하지 못했습니다."),
            @"cloud-refused":Text(korean, @"The cloud operation was not permitted at the time shown. This error alone does not establish whether a lock, permissions, or another restriction caused it.", @"당시 클라우드 작업이 허용되지 않았습니다. 이 오류만으로는 잠금, 권한 또는 다른 제한 중 무엇이 원인이었는지 알 수 없습니다."),
            @"conflict":Text(korean, @"Another item already used the proposed name at this attempt, so the name change was deferred.", @"당시 정리 후 이름을 다른 항목이 사용하고 있어 이름 변경을 보류했습니다."),
            @"readonly":Text(korean, @"The location was read-only at this attempt, so the change could not be made.", @"당시 위치가 읽기 전용이어서 변경하지 못했습니다."),
            @"storage":Text(korean, @"The operation reported insufficient free disk space at the time shown.", @"당시 작업에서 디스크 여유 공간이 부족한 것으로 확인되었습니다."),
            @"interrupted":Text(korean, @"This check was interrupted before it finished.", @"당시 검사를 마치기 전에 작업이 중단되었습니다."),
            @"timeout":Text(korean, @"The operation did not respond in time during this check. The recorded error does not identify why.", @"당시 검사에서 응답 시간이 초과되었습니다. 기록된 오류만으로는 지연 원인을 알 수 없습니다."),
            @"busy":Text(korean, @"The item was temporarily busy or unavailable for this operation at the time shown.", @"표시된 시각에 항목을 일시적으로 사용 중이거나 이 작업에 사용할 수 없었습니다."),
            @"unavailable":Text(korean, @"The item or its location was unavailable at this check.", @"당시 검사에서 항목 또는 해당 위치를 사용할 수 없었습니다.")};
        title = titles[category]; detail = details[category];
        if (!title) {
            BOOL missing = !reason.length && !error;
            title = [kind isEqual:@"error"]
                ? Text(korean, missing ? @"Check incomplete · cause not recorded" : @"Check could not be completed", missing ? @"검사 미완료 · 원인 기록 없음" : @"검사를 마치지 못했습니다")
                : Text(korean, missing ? @"Check deferred · cause not recorded" : @"Check was deferred", missing ? @"확인 보류 · 원인 기록 없음" : @"확인을 보류했습니다");
            detail = missing
                ? Text(korean, @"The cause was not recorded for this older activity entry. Its current status cannot be inferred from this record.", @"이전 활동의 원인이 기록되지 않았습니다. 이 기록만으로 현재 상태를 알 수 없습니다.")
                : Text(korean, @"The operation could not finish at the time shown. The recorded message does not establish a more specific cause.", @"표시된 시각에 작업을 마치지 못했습니다. 기록된 메시지만으로는 더 구체적인 원인을 알 수 없습니다.");
            if (!missing) {
                // Preserve useful raw context even when the separately saved
                // errno is unknown. It must not override that typed evidence.
                if (error) detail = [detail stringByAppendingFormat:Text(korean, @"\nRecorded errno: %@", @"\n기록된 오류 번호: %@"), error];
                if (reason.length) {
                    NSString *message = reason;
                    if (message.length > 768) message = [[message substringToIndex:768] stringByAppendingString:@"…"];
                    detail = [detail stringByAppendingFormat:Text(korean, @"\nRecorded message: %@", @"\n기록된 메시지: %@"), message];
                }
            }
        }
        NSNumber *occurrences = Count(event[@"occurrences"]);
        NSNumber *first = ActivityTimestamp(event[@"first_at"]);
        NSNumber *last = ActivityTimestamp(event[@"at"]);
        if (occurrences.unsignedLongLongValue > 1 && ![category isEqual:@"cloud-only"]) {
            detail = [detail stringByAppendingFormat:Text(korean, @"\nRecorded %@ times.", @"\n%@회 기록되었습니다."), Number(occurrences)];
            if (first && last && first.doubleValue <= last.doubleValue)
                detail = [detail stringByAppendingFormat:Text(korean, @" First recorded: %@.", @" 최초 기록: %@."), ActivityTime(first, korean)];
        }
    }
    NSNumber *resolvedAt = ActivityTimestamp(event[@"resolved_at"]);
    NSNumber *observedAt = ActivityTimestamp(event[@"at"]);
    NSString *resolution = [event[@"resolution"] isKindOfClass:NSString.class] ? event[@"resolution"] : @"";
    NSDictionary *outcomes = @{
        @"checked":Text(korean, @"Checked successfully later", @"이후 검사 완료"),
        @"renamed":Text(korean, @"Name changed later", @"이후 이름 변경 완료"),
        @"restored":Text(korean, @"Original name restored later", @"이후 원래 이름으로 되돌림 완료"),
        @"absent":Text(korean, @"Item was absent at a later check", @"이후 확인에서 항목 없음"),
        @"no_longer_needed":Text(korean, @"Later check found no change needed", @"이후 확인에서 변경 불필요")};
    BOOL resolved = historicalIssue && outcomes[resolution] && resolvedAt && (!observedAt || resolvedAt.doubleValue >= observedAt.doubleValue);
    if (resolved) {
        detail = [NSString stringWithFormat:Text(korean, @"Original observation: %@\n%@\n\n%@\nResolved at: %@", @"당시 기록: %@\n%@\n\n%@\n해결 시각: %@"), title, detail, outcomes[resolution], ActivityTime(resolvedAt, korean)];
        title = outcomes[resolution];
    }
    return @{@"title":title, @"detail":detail, @"category":category, @"historicalIssue":@(historicalIssue), @"resolved":@(resolved),
        @"resolvedAt":resolved ? resolvedAt : NSNull.null, @"path":path, @"pathActionAllowed":@(safePath)};
}

static NSArray *RetryIssues(id records, BOOL directory, BOOL korean) {
    if (![records isKindOfClass:NSArray.class]) return @[];
    NSMutableArray *issues = [NSMutableArray array];
    for (id value in records) {
        NSDictionary *record = Dictionary(value);
        NSString *path = [record[@"path"] isKindOfClass:NSString.class] ? record[@"path"] : nil;
        if (!path.isAbsolutePath || [path rangeOfString:@"\0"].location != NSNotFound) continue;
        NSNumber *attempts = Count(record[@"attempts"]);
        NSDictionary *advice = FailureAdvice(path, record[@"reason"], Flag(record[@"locked"]).boolValue, attempts.unsignedLongLongValue > 1, korean);
        NSString *detail = advice[@"detail"];
        BOOL required = [advice[@"requiresAction"] boolValue];
        NSString *retryNote = Text(korean, @"The retry time is unavailable.", @"재시도 시각을 확인할 수 없습니다.");
        id retry = record[@"next_retry"];
        if ([retry isKindOfClass:NSNumber.class] && CFGetTypeID((__bridge CFTypeRef)retry) != CFBooleanGetTypeID() && isfinite([retry doubleValue]) && [retry doubleValue] > 0 && [retry doubleValue] <= NSDate.distantFuture.timeIntervalSince1970) {
            if ([retry doubleValue] <= NSDate.date.timeIntervalSince1970) {
                retryNote = Text(korean, @"Ready to retry; waiting for its turn.", @"재시도 시각이 되어 처리 순서를 기다리고 있습니다.");
            } else {
                NSDateFormatter *formatter = [NSDateFormatter new];
                formatter.locale = [NSLocale localeWithLocaleIdentifier:korean ? @"ko_KR" : @"en_US"];
                formatter.dateStyle = NSDateFormatterShortStyle; formatter.timeStyle = NSDateFormatterShortStyle;
                retryNote = [NSString stringWithFormat:Text(korean, @"Can retry from %@; the exact time depends on waiting work.", @"%@부터 재시도합니다. 대기 중인 작업에 따라 실제 시각은 달라질 수 있습니다."), [formatter stringFromDate:[NSDate dateWithTimeIntervalSince1970:[retry doubleValue]]]];
            }
        }
        NSString *operation = directory ? Text(korean, @"Folder check", @"폴더 확인") : Text(korean, @"Rename", @"이름 변경");
        if (![advice[@"category"] isEqual:@"cloud-only"])
            detail = [NSString stringWithFormat:@"%@\n%@%@ · %@", detail, operation,
                attempts ? [NSString stringWithFormat:Text(korean, @" · %@ failed attempts", @" · 실패 %@회"), Number(attempts)] : @"", retryNote];
        [issues addObject:@{@"title":advice[@"title"], @"path":path, @"detail":detail, @"tone":required ? @"warning" : @"working",
            @"actionTitle":Text(korean, @"Show in Finder", @"Finder에서 확인"), @"action":@"reveal", @"requiresAction":@(required), @"category":advice[@"category"]}];
        if (issues.count == 8) break;
    }
    return issues;
}

NSDictionary *JasoStatusPresentation(NSDictionary *snapshot, NSString *error, BOOL korean) {
    BOOL invalid = snapshot && !Dictionary(snapshot);
    BOOL failed = invalid || ([error isKindOfClass:NSString.class] && error.length);
    NSDictionary *status = failed ? nil : Dictionary(snapshot);
    NSDictionary *current = Dictionary(status[@"current"]);
    if (current) {
        // The worker projects active work separately from retained drive history.
        // Keep process, coverage, recovery and saved-index totals authoritative.
        NSMutableDictionary *display = [status mutableCopy];
        for (NSString *key in @[@"pending_jobs", @"deferred_jobs", @"deferred_renames", @"next_retry",
            @"directory_retry_count", @"directory_retry_items", @"rename_retry_items", @"pending_baseline_roots",
            @"baseline_complete", @"needs_revalidation"])
            if (current[key]) display[key] = current[key];
        status = display;
    }
    NSNumber *running = Flag(status[@"running"]), *paused = Flag(status[@"paused"]), *apply = Flag(status[@"apply"]);
    NSNumber *baseline = Flag(status[@"baseline_complete"]), *recovery = Flag(status[@"pending_recovery"]);
    NSNumber *revalidation = Flag(status[@"needs_revalidation"]);
    NSNumber *indexed = Count(status[@"indexed_entries"]), *queued = Count(status[@"pending_jobs"]);
    NSNumber *renames = Count(status[@"deferred_renames"]), *deferred = Count(status[@"deferred_jobs"]);
    NSNumber *directoryRetries = Count(status[@"directory_retry_count"]);
    NSNumber *errors = Count(status[@"errors"]);
    NSArray *roots = Paths(status[@"roots"]), *active = Paths(status[@"active_roots"]);
    NSArray *unavailable = FailurePaths(status[@"unavailable_roots"]);
    NSArray *catalogs = FailurePaths(status[@"catalog_unavailable"]);
    NSArray *pendingRoots = Paths(status[@"pending_baseline_roots"]);
    NSArray *disconnected = Paths(status[@"disconnected_roots"]);
    BOOL isRunning = running.boolValue, isPaused = isRunning && paused.boolValue;
    NSNumber *indexedFlag = Flag(status[@"indexed"]);
    BOOL preparing = isRunning && indexedFlag && !indexedFlag.boolValue;
    BOOL preview = apply && !apply.boolValue;
    BOOL initial = (baseline && !baseline.boolValue) || pendingRoots.count > 0;
    BOOL coverageKnown = [status[@"active_roots"] isKindOfClass:NSArray.class] &&
        Dictionary(status[@"unavailable_roots"]) && Dictionary(status[@"catalog_unavailable"]);
    BOOL complete = running && paused && apply && baseline && recovery && revalidation &&
        indexed && queued && renames && deferred && coverageKnown;
    id nextRetry = status[@"next_retry"];
    BOOL retryScheduled = [nextRetry isKindOfClass:NSNumber.class] &&
        CFGetTypeID((__bridge CFTypeRef)nextRetry) != CFBooleanGetTypeID() &&
        isfinite([nextRetry doubleValue]) && [nextRetry doubleValue] > 0;
    NSMutableArray *issues = [RetryIssues(status[@"rename_retry_items"], NO, korean) mutableCopy];
    [issues addObjectsFromArray:RetryIssues(status[@"directory_retry_items"], YES, korean)];
    NSUInteger intervention = 0;
    NSString *actionCategory = nil;
    for (NSDictionary *issue in issues) if ([issue[@"requiresAction"] boolValue]) {
        intervention++;
        if (!actionCategory) actionCategory = issue[@"category"];
        else if (![actionCategory isEqual:issue[@"category"]]) actionCategory = @"mixed";
    }
    BOOL hasRetries = renames.unsignedLongLongValue || directoryRetries.unsignedLongLongValue || retryScheduled || issues.count;
    BOOL currentProblems = unavailable.count || catalogs.count || intervention;
    NSString *issueSummary = @"";
    if (hasRetries) {
        issueSummary = intervention
            ? Text(korean, @"Some items need your attention. Follow each item's steps to check its account, connection, access, or name. Other work can continue while you check.", @"직접 확인할 항목이 있습니다. 항목별 안내에 따라 계정, 연결, 접근 상태 또는 이름을 확인하세요. 확인하는 동안 다른 항목은 계속 처리할 수 있습니다.")
            : Text(korean, @"These items will retry automatically. Each entry shows the reported problem and what to check if it persists.", @"표시된 항목은 자동으로 재시도합니다. 각 항목에서 발생한 문제와 반복될 때 확인할 사항을 안내합니다.");
        if (!isRunning || isPaused) issueSummary = [Text(korean, @"Start or resume cleanup to allow retries. ", @"재시도하려면 ‘작업 시작’ 또는 ‘계속 진행’을 선택하세요. ") stringByAppendingString:issueSummary];
        if (!issues.count) issueSummary = [issueSummary stringByAppendingFormat:@" %@", Text(korean, @"Refresh to load the missing item details.", @"항목별 상세 정보를 불러오려면 새로고침하세요.")];
        NSNumber *directoryCount = Count(status[@"directory_retry_count"]);
        if (directoryCount && renames) {
            unsigned long long total = directoryCount.unsignedLongLongValue + renames.unsignedLongLongValue;
            if (total > issues.count) issueSummary = [issueSummary stringByAppendingString:korean
                ? [NSString stringWithFormat:@" 재시도 대기 %@개 중 %lu개를 표시합니다(종류별 최대 8개).", Number(@(total)), (unsigned long)issues.count]
                : [NSString stringWithFormat:@" Showing %lu of %@ queued retry items (up to eight of each type).", (unsigned long)issues.count, Number(@(total))]];
        }
    }

    // Retry work is contextual information, separate from overall worker activity.
    // The saved detail lists are bounded; their unseen rows may need intervention.
    NSUInteger automaticIssues = issues.count - intervention;
    double totalRetries = renames.doubleValue + directoryRetries.doubleValue;
    BOOL completeDetails = renames && directoryRetries && totalRetries == issues.count;
    BOOL onlyCloudWaiting = completeDetails && issues.count > 0;
    for (NSDictionary *issue in issues) if (![issue[@"category"] isEqual:@"cloud-only"]) onlyCloudWaiting = NO;
    BOOL hasAutomaticDetails = automaticIssues || (hasRetries && !intervention) || totalRetries > issues.count;
    NSString *actionIssueSummary = intervention
        ? Text(korean, @"Follow each item's steps to check its account, connection, access, or name.", @"항목별 안내에 따라 계정, 연결, 접근 상태 또는 이름을 확인하세요.") : @"";
    NSString *automaticRetrySummary = @"", *automaticRetryDetail = @"";
    if (hasAutomaticDetails) {
        automaticRetrySummary = completeDetails
            ? [NSString stringWithFormat:Text(korean, @"Automatic retries · %@", @"자동 재시도 · %@개"), Number(@(automaticIssues))]
            : Text(korean, @"Retry details", @"재시도 내역");
        automaticRetryDetail = Text(korean, @"Cleanup retries these items automatically. The list below shows each item’s location, cause, and retry time.", @"이 항목은 자동으로 다시 처리합니다. 아래에서 위치, 원인과 재시도 시각을 확인할 수 있습니다.");
        if (onlyCloudWaiting) {
            automaticRetrySummary = [NSString stringWithFormat:Text(korean, @"Stored in the cloud · %@", @"클라우드에 보관 중 · %@개"), Number(@(automaticIssues))];
            automaticRetryDetail = Text(korean, @"Filename cleanup resumes when these files are available on this Mac.", @"이 Mac에 파일이 저장되면 이름을 정리합니다.");
            issueSummary = automaticRetryDetail;
        }
        if (!isRunning || isPaused) automaticRetryDetail = [Text(korean, @"Start or resume cleanup to allow retries. ", @"재시도하려면 ‘작업 시작’ 또는 ‘계속 진행’을 선택하세요. ") stringByAppendingString:automaticRetryDetail];
        if (!completeDetails) automaticRetryDetail = [automaticRetryDetail stringByAppendingString:
            Text(korean, @" Some retry details may be unavailable. Refresh to load the latest entries.", @" 일부 재시도 내역만 표시될 수 있습니다. 최신 항목을 불러오려면 새로고침하세요.")];
    }
    unsigned long long ordinaryQueued = queued.unsignedLongLongValue;
    if (directoryRetries) ordinaryQueued = ordinaryQueued > directoryRetries.unsignedLongLongValue
        ? ordinaryQueued - directoryRetries.unsignedLongLongValue : 0;

    NSString *title = Text(korean, @"Status incomplete", @"상태 정보 확인 중");
    NSString *subtitle = Text(korean, @"Refresh to load the remaining status information.", @"나머지 상태 정보를 불러오려면 새로고침하세요.");
    NSString *tone = @"neutral", *symbol = @"questionmark.circle";
    NSString *primaryTitle = @"", *primaryAction = @"";
    NSString *progress = Text(korean, @"File list progress is not available yet.", @"파일 목록 확인 진행 상황을 아직 불러오지 못했습니다.");

    if (failed) {
        title = Text(korean, @"Status unavailable", @"상태 확인 불가");
        subtitle = Text(korean, @"Could not read cleanup status. Choose Refresh to try again, or open diagnostics for details.", @"정리 상태를 읽지 못했습니다. 새로고침으로 다시 시도하거나 진단 정보에서 자세한 내용을 확인하세요.");
        tone = @"warning"; symbol = @"exclamationmark.triangle";
    } else if (!snapshot) {
        title = Text(korean, @"Loading status…", @"상태 불러오는 중…");
        subtitle = Text(korean, @"Reading cleanup activity and the saved file list.", @"정리 활동과 저장된 파일 목록을 확인하고 있습니다.");
        symbol = @"arrow.triangle.2.circlepath";
    } else {
        if (running && !isRunning) {
            primaryTitle = Text(korean, @"Start worker", @"작업 시작"); primaryAction = @"start";
        } else if (isPaused) {
            primaryTitle = Text(korean, @"Resume", @"계속 진행"); primaryAction = @"resume";
        } else if (isRunning && paused) {
            primaryTitle = Text(korean, @"Pause", @"일시정지"); primaryAction = @"pause";
        }

        if (running && !isRunning) {
            title = Text(korean, @"Stopped", @"중지됨");
            subtitle = Text(korean, @"Cleanup is stopped. Choose Start worker to watch folders and continue processing.", @"정리가 중지되었습니다. ‘작업 시작’을 선택하면 폴더를 살펴보고 정리를 이어갑니다.");
            symbol = @"stop.circle";
        } else if (isPaused) {
            title = Text(korean, @"Paused", @"일시정지됨");
            subtitle = Text(korean, @"New changes are being recorded. Choose Resume when you are ready to process the waiting work.", @"새 변경 사항을 기록하고 있습니다. 준비되면 ‘계속 진행’을 선택해 대기 중인 작업을 처리하세요.");
            symbol = @"pause.circle";
        } else if (recovery.boolValue) {
            title = Text(korean, @"Recovery pending", @"복구 대기 중");
            subtitle = Text(korean, @"A recorded name change is still unfinished. Processing can continue after it finishes or is recovered. If this persists, retain the recovery history and contact support.", @"기록된 이름 변경 작업이 아직 완료되지 않았습니다. 해당 작업의 완료 또는 복구 후 정리를 이어갈 수 있습니다. 계속 표시되면 복구 기록을 보관하고 지원팀에 문의하세요.");
            tone = @"warning"; symbol = @"exclamationmark.arrow.circlepath";
        } else if (!running || !paused || !apply) {
            // Keep the incomplete summary; missing controls cannot establish activity.
        } else if (preview) {
            title = Text(korean, @"Preview mode", @"미리보기 모드");
            subtitle = Text(korean, @"Jaso NFC is checking filenames. Open Manage folders… to review the names, then choose Start automatic cleanup when ready.", @"파일 이름을 확인하고 있습니다. ‘정리할 폴더…’에서 바뀔 이름을 살펴본 뒤 ‘자동 정리 시작’을 선택하세요.");
            tone = currentProblems ? @"warning" : @"neutral"; symbol = @"eye";
        } else if (preparing) {
            title = Text(korean, @"Preparing the index", @"인덱스 준비 중");
            subtitle = Text(korean, @"Preparing a place to store the file list before checking your folders.", @"폴더를 확인하기 전에 파일 목록을 저장할 공간을 준비하고 있습니다.");
            tone = @"working"; symbol = @"tray.and.arrow.down";
        } else if (currentProblems) {
            if (unavailable.count || catalogs.count) title = Text(korean, @"Check unavailable locations", @"접근할 수 없는 위치 확인 필요");
            else if ([actionCategory isEqual:@"permission"]) title = Text(korean, @"Check access permissions", @"접근 권한 확인 필요");
            else if ([actionCategory isEqual:@"locked"]) title = Text(korean, @"Check locked items", @"잠긴 항목 확인 필요");
            else if ([actionCategory isEqual:@"conflict"]) title = Text(korean, @"Resolve filename conflicts", @"파일 이름 충돌 확인 필요");
            else if ([actionCategory isEqual:@"readonly"]) title = Text(korean, @"Check read-only locations", @"읽기 전용 위치 확인 필요");
            else if ([actionCategory isEqual:@"authentication"]) title = Text(korean, @"Check account authentication", @"계정 인증 확인 필요");
            else title = Text(korean, @"Some items need a manual check", @"직접 확인할 항목 있음");
            subtitle = Text(korean, @"Follow the steps for each affected item below. Other work can continue while you check.", @"아래 항목별 안내에 따라 문제를 확인하세요. 확인하는 동안 다른 항목은 계속 처리할 수 있습니다.");
            tone = @"warning"; symbol = @"exclamationmark.triangle";
        } else if (isRunning && coverageKnown && active.count == 0 && disconnected.count) {
            title = Text(korean, @"No drives connected", @"연결된 드라이브 없음");
            subtitle = Text(korean, @"Choose the drives to include in Settings → Manage folders….", @"설정 → 정리할 폴더…에서 관리할 드라이브를 선택하세요.");
            tone = @"neutral"; symbol = @"externaldrive";
        } else if (revalidation.boolValue) {
            title = Text(korean, @"Rechecking locations", @"위치 다시 확인 중");
            subtitle = Text(korean, @"Rechecking saved file information before continuing cleanup.", @"정리를 이어가기 전에 저장된 파일 정보를 다시 확인하고 있습니다.");
            tone = @"working"; symbol = @"arrow.triangle.2.circlepath";
        } else if (isRunning && paused && initial) {
            title = Text(korean, @"Initial indexing", @"최초 인덱싱 중");
            subtitle = Text(korean, @"Building the first list of files and folders in your selected locations.", @"선택한 위치의 파일과 폴더를 확인해 첫 목록을 만들고 있습니다.");
            tone = @"working"; symbol = @"tray.and.arrow.down";
        } else if (isRunning && paused && ordinaryQueued > 0) {
            title = Text(korean, @"Processing changes", @"변경 사항 처리 중");
            subtitle = Text(korean, @"Checking waiting folders and updating the file list.", @"대기 중인 폴더를 확인하고 파일 목록을 갱신하고 있습니다.");
            tone = @"working"; symbol = @"arrow.triangle.2.circlepath";
        } else if (isRunning && coverageKnown && active.count == 0) {
            title = Text(korean, @"No active locations", @"감시 중인 위치 없음");
            subtitle = Text(korean, @"Open Manage folders… to check your selection, then check the connection and access for those locations.", @"‘정리할 폴더…’에서 선택한 폴더를 확인한 뒤 해당 위치의 연결과 접근 상태를 확인하세요.");
            tone = @"warning"; symbol = @"folder.badge.questionmark";
        } else if (complete && isRunning && baseline.boolValue) {
            title = Text(korean, @"Watching for changes", @"변경 사항 감지 중");
            subtitle = Text(korean, @"The first folder check is complete. Jaso NFC checks new changes as they arrive.", @"첫 폴더 확인을 마쳤습니다. 새 변경 사항이 들어오면 확인합니다.");
            tone = @"good"; symbol = @"checkmark.circle";
        }

        if (running && !isRunning) {
            progress = Text(korean, @"These counts come from the saved file list. Start cleanup to update them.", @"저장된 파일 목록의 수치입니다. 작업을 시작하면 갱신합니다.");
        } else if (isPaused) {
            progress = Text(korean, @"Choose Resume to process the waiting folders.", @"‘계속 진행’을 선택하면 대기 중인 폴더를 처리합니다.");
        } else if (preparing) {
            progress = Text(korean, @"Counts will appear when the file list is ready.", @"파일 목록이 준비되면 확인한 항목 수를 표시합니다.");
        } else if (initial) {
            progress = Text(korean, @"The initial index is being built. More folders may join the queue as they are discovered.", @"첫 파일 목록을 만들고 있습니다. 폴더를 추가로 발견하면 대기 작업이 늘어날 수 있습니다.");
        } else if (revalidation.boolValue) {
            progress = Text(korean, @"Rechecking file information may update these saved counts.", @"파일 정보를 다시 확인하면 저장된 수치가 바뀔 수 있습니다.");
        } else if (baseline.boolValue && queued) {
            progress = queued.unsignedLongLongValue
                ? Text(korean, @"Queued folders counts folder jobs, including work deferred for later. Each folder can contain several files.", @"대기 폴더는 나중에 처리할 작업을 포함한 폴더 작업 수입니다. 한 폴더에는 여러 파일이 들어 있을 수 있습니다.")
                : Text(korean, @"The first folder check is complete. There are currently no waiting folder jobs.", @"첫 폴더 확인을 마쳤으며 현재 대기 중인 폴더 작업이 없습니다.");
        }
    }

    NSArray *metrics = @[
        @{@"label":Text(korean, @"Indexed items", @"인덱스 항목"), @"value":Number(indexed),
          @"note":Text(korean, @"Files, folders, and links in the saved index", @"저장된 인덱스의 파일·폴더·링크 수")},
        @{@"label":Text(korean, @"Queued folders", @"대기 폴더"), @"value":Number(queued),
          @"note":Text(korean, @"Directory work, including deferred jobs", @"처리를 미룬 작업을 포함한 폴더 작업 수")},
        @{@"label":Text(korean, @"Rename retries", @"이름 변경 재시도"), @"value":Number(renames),
          @"note":Text(korean, @"Currently saved for another rename attempt", @"현재 이름 변경 재시도를 기다리는 항목 수")}];

    NSMutableArray *notices = [NSMutableArray array];
    if (recovery.boolValue) [notices addObject:Notice(Text(korean, @"Recovery pending", @"복구 대기 중"),
        Text(korean, @"A name change is recorded as unfinished. It may still be running or need recovery. If this persists, keep the recovery history and contact support with diagnostics.", @"이름 변경이 미완료 상태로 기록되어 있습니다. 진행 중이거나 복구가 필요한 작업일 수 있습니다. 계속 표시되면 복구 기록을 보관하고 진단 정보와 함께 지원팀에 문의하세요."), @"warning")];
    if (preview) [notices addObject:Notice(Text(korean, @"Preview is enabled", @"미리보기 사용 중"),
        Text(korean, @"Use Manage folders… to preview current and proposed names and start automatic cleanup.", @"‘정리할 폴더…’에서 현재 이름과 정리 후 이름을 확인하고 자동 정리를 시작하세요."), @"neutral")];
    if (revalidation.boolValue) [notices addObject:Notice(Text(korean, @"Location recheck required", @"위치 재확인 필요"),
        Text(korean, @"Rechecking saved drive information. The file list may update afterward.", @"저장된 드라이브 정보를 다시 확인하고 있습니다. 확인 후 파일 목록이 갱신될 수 있습니다."), @"working")];
    if (deferred.unsignedLongLongValue) [notices addObject:Notice(Text(korean, @"Deferred directory work", @"처리를 미룬 폴더 작업"),
        [NSString stringWithFormat:Text(korean, @"%@ folder jobs are waiting for queue capacity. They are included in Queued folders.", @"%@개 폴더 작업이 처리 순서에 들어갈 여유를 기다리고 있습니다. 이 작업은 대기 폴더 수에 포함됩니다."), Number(deferred)], @"neutral")];
    if (errors.unsignedLongLongValue) [notices addObject:Notice(Text(korean, @"Historical scan errors", @"과거 폴더 확인 오류"),
        [NSString stringWithFormat:Text(korean, @"%@ cumulative folder-check errors are recorded. Use Rename retries to see how many items are currently waiting for another rename attempt.", @"지금까지 폴더 확인 오류가 누적 %@건 기록되었습니다. 현재 다시 이름을 바꾸려고 기다리는 항목 수는 ‘이름 변경 재시도’에서 확인하세요."), Number(errors)], @"neutral")];
    if (status && !complete && !preparing) [notices addObject:Notice(Text(korean, @"Some status information is unavailable", @"일부 상태 정보 확인 불가"),
        Text(korean, @"A dash marks an unavailable value. Choose Refresh to check again, or open diagnostics for details.", @"대시는 확인할 수 없는 값입니다. 새로고침으로 다시 확인하거나 진단 정보에서 자세한 내용을 살펴보세요."), @"neutral")];

    NSMutableOrderedSet *allPaths = [NSMutableOrderedSet orderedSetWithArray:roots];
    for (NSArray *paths in @[active, unavailable, catalogs, pendingRoots]) [allPaths addObjectsFromArray:paths];
    NSMutableArray *locations = [NSMutableArray array];
    for (NSString *path in [allPaths.array sortedArrayUsingSelector:@selector(localizedStandardCompare:)]) {
        BOOL unavailableRoot = [unavailable containsObject:path], catalog = [catalogs containsObject:path];
        if ([disconnected containsObject:path] && ![active containsObject:path] && !unavailableRoot && !catalog) continue;
        NSString *state = Text(korean, @"Status unavailable", @"상태 확인 불가");
        NSString *detail = Text(korean, @"This is a saved location. Refresh to check whether it is being watched.", @"설정에 저장된 위치입니다. 현재 감시 상태를 확인하려면 새로고침하세요.");
        NSString *locationTone = @"neutral";
        BOOL requiresAction = NO;
        if (running && !isRunning) {
            state = Text(korean, @"Saved location", @"저장된 위치");
            detail = Text(korean, @"This location was saved before cleanup stopped. Start cleanup to watch it again.", @"정리 중지 전에 저장된 위치입니다. 작업을 시작하면 다시 감시합니다.");
        } else if (isRunning && [active containsObject:path]) {
            state = Text(korean, @"Watching", @"감시 중");
            detail = isPaused ? Text(korean, @"Changes are being recorded. Choose Resume to process them.", @"변경 사항을 기록하고 있습니다. 처리하려면 ‘계속 진행’을 선택하세요.")
                : Text(korean, @"Watching for new files and changes.", @"새 파일과 변경 사항을 살펴보고 있습니다.");
            locationTone = @"good";
        }
        if ([pendingRoots containsObject:path]) {
            detail = [detail stringByAppendingFormat:@" %@", Text(korean, @"The first file-list check for this location is still waiting.", @"이 위치의 첫 파일 목록 확인을 기다리고 있습니다.")];
            if (isRunning && !isPaused) locationTone = @"working";
        }
        if (unavailableRoot || catalog) {
            NSDictionary *rootAdvice = unavailableRoot ? FailureAdvice(path, Dictionary(status[@"unavailable_roots"])[path], NO, NO, korean) : nil;
            NSDictionary *catalogAdvice = catalog ? FailureAdvice(path, Dictionary(status[@"catalog_unavailable"])[path], NO, NO, korean) : nil;
            state = (rootAdvice ?: catalogAdvice)[@"title"];
            requiresAction = [rootAdvice[@"requiresAction"] boolValue] || [catalogAdvice[@"requiresAction"] boolValue];
            NSMutableArray *details = [NSMutableArray array];
            if (rootAdvice) [details addObject:[NSString stringWithFormat:@"%@ %@",
                Text(korean, @"Could not start watching this location.", @"이 위치의 감시를 시작하지 못했습니다."), rootAdvice[@"detail"]]];
            if (catalogAdvice) {
                [details addObject:Text(korean, @"Automatic location discovery is unavailable here. Restore access so newly added locations can be detected.", @"이 영역에서 새 위치를 자동으로 찾지 못하고 있습니다. 새로 추가한 위치를 찾을 수 있도록 접근 상태를 확인하세요.")];
                if (![catalogAdvice isEqual:rootAdvice]) [details addObject:catalogAdvice[@"detail"]];
            }
            detail = [details componentsJoinedByString:@" "];
            if (running && !isRunning) detail = [Text(korean, @"Last saved status: ", @"마지막으로 저장된 상태입니다. ") stringByAppendingString:detail];
            locationTone = @"warning";
        }
        NSString *locationTitle = path.lastPathComponent.length ? path.lastPathComponent : path;
        if (catalog && ([path isEqual:@"/Users"] || [path isEqual:@"users"])) locationTitle = Text(korean, @"User folders", @"사용자 폴더");
        if (catalog && ([path isEqual:@"/Volumes"] || [path isEqual:@"volumes"])) locationTitle = Text(korean, @"Mounted volumes", @"마운트된 볼륨");
        [locations addObject:@{@"title":locationTitle,
            @"path":path, @"state":state, @"detail":detail, @"tone":locationTone, @"requiresAction":@(requiresAction)}];
    }
    return @{@"title":title, @"subtitle":subtitle, @"tone":tone, @"symbol":symbol,
        @"metrics":metrics, @"locations":locations, @"notices":notices, @"issues":issues, @"issueSummary":issueSummary,
        @"actionIssueSummary":actionIssueSummary, @"automaticRetrySummary":automaticRetrySummary, @"automaticRetryDetail":automaticRetryDetail,
        @"primaryTitle":primaryTitle, @"primaryAction":primaryAction, @"progressNote":progress};
}
