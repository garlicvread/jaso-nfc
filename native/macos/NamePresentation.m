#import "NamePresentation.h"

static unichar StandaloneLetter(unichar value) {
    // Modern joining letters are displayed with their standalone counterparts.
    // Composed syllables and every other character retain their original value.
    if(value>=0x1100 && value<=0x1112)
        return [@"ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ" characterAtIndex:value-0x1100];
    if(value>=0x1161 && value<=0x1175)
        return [@"ㅏㅐㅑㅒㅓㅔㅕㅖㅗㅘㅙㅚㅛㅜㅝㅞㅟㅠㅡㅢㅣ" characterAtIndex:value-0x1161];
    if(value>=0x11A8 && value<=0x11C2)
        return [@"ㄱㄲㄳㄴㄵㄶㄷㄹㄺㄻㄼㄽㄾㄿㅀㅁㅂㅄㅅㅆㅇㅈㅊㅋㅌㅍㅎ" characterAtIndex:value-0x11A8];
    return value;
}

static NSString *DisplayName(id value, BOOL *separated) {
    if(![value isKindOfClass:NSString.class])return @"";
    NSString *name=value;
    NSMutableString *display=name.mutableCopy;
    for(NSUInteger index=0;index<name.length;index++) {
        unichar original=[name characterAtIndex:index];
        unichar letter=StandaloneLetter(original);
        if(letter==original)continue;
        [display replaceCharactersInRange:NSMakeRange(index,1) withString:[NSString stringWithCharacters:&letter length:1]];
        *separated=YES;
    }
    return display.copy;
}

NSDictionary *JasoNamePresentation(NSDictionary *record) {
    BOOL separated=NO;
    NSString *before=DisplayName(record[@"old_name"],&separated);
    NSString *after=DisplayName(record[@"new_name"],&separated);
    return @{@"before":before,@"after":after,@"separated":@(separated)};
}

NSString *JasoHistoryRowIdentifier(NSDictionary *record, NSString *side) {
    NSString *identifier=[record[@"id"] isKindOfClass:NSString.class]?record[@"id"]:@"";
    if(!identifier.length)identifier=[record[@"operation_id"] isKindOfClass:NSString.class]?record[@"operation_id"]:@"";
    NSString *identity;
    if(identifier.length)identity=[@"id:" stringByAppendingString:identifier];
    else {
        NSMutableArray *values=NSMutableArray.new;
        for(NSString *key in @[@"old_path",@"new_path",@"old_name",@"new_name",@"timestamp",@"revision"]){
            id value=record[key];
            [values addObject:[value isKindOfClass:NSString.class]?value:([value isKindOfClass:NSNumber.class]?[value stringValue]:@"")];
        }
        NSData *data=[NSJSONSerialization dataWithJSONObject:values options:0 error:nil];
        identity=[@"record:" stringByAppendingString:[[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding]];
    }
    return [NSString stringWithFormat:@"history-row-%@:%@",side,identity];
}
