/*
 * T7 Shield dext 原型 —— 激活器（宿主容器进程）。
 * 用法：activate <包含 dext 的 .app 路径>
 * 激活结果打印到 stdout 后退出。
 */
#import <Foundation/Foundation.h>
#import <SystemExtensions/SystemExtensions.h>

static NSString * const kDextBundleID = @"com.t7shield.proto.t7-discovery";

@interface Activator : NSObject <OSSystemExtensionRequestDelegate>
@end

@implementation Activator

- (OSSystemExtensionReplacementAction)
    request:(OSSystemExtensionRequest *)request
actionForReplacingExtension:(OSSystemExtensionProperties *)existing
          withExtension:(OSSystemExtensionProperties *)ext
{
    NSLog(@"actionForReplacing: existing=%@ new=%@",
          existing.bundleIdentifier, ext.bundleIdentifier);
    return OSSystemExtensionReplacementActionReplace;
}

- (void)requestNeedsUserApproval:(OSSystemExtensionRequest *)request
{
    NSLog(@"EVENT: needs-user-approval（去系统设置批准，或 developer 模式自动通过）");
}

- (void)request:(OSSystemExtensionRequest *)request
    didFinishWithResult:(OSSystemExtensionRequestResult)result
{
    NSLog(@"EVENT: didFinishWithResult=%lu (%@)", (unsigned long)result,
          result == OSSystemExtensionRequestCompleted ? @"completed" : @"other");
    exit(0);
}

- (void)request:(OSSystemExtensionRequest *)request
    didFailWithError:(NSError *)error
{
    NSLog(@"EVENT: didFailWithError domain=%@ code=%ld userAction=%@",
          error.domain, (long)error.code,
          error.userInfo[@"OSSystemExtensionErrorUserAction"] ?: @"(none)");
    exit(1);
}

@end

int main(int argc, const char * argv[])
{
    @autoreleasepool {
        if (argc < 2) {
            NSLog(@"usage: %s <app-bundle-path>", argv[0]);
            return 2;
        }
        Activator * a = [[Activator alloc] init];
        OSSystemExtensionRequest * req =
            [OSSystemExtensionRequest activationRequestForExtension:kDextBundleID
                                                              queue:dispatch_get_main_queue()];
        req.delegate = a;
        [[OSSystemExtensionManager sharedManager] submitRequest:req];
        NSLog(@"submitted activation request for %@", kDextBundleID);
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 60 * NSEC_PER_SEC),
                       dispatch_get_main_queue(), ^{
            NSLog(@"EVENT: timeout waiting for activation result");
            exit(3);
        });
        dispatch_main();
    }
    return 0;
}
