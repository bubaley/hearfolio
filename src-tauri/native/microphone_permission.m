#import <AVFoundation/AVFoundation.h>
#import <Foundation/Foundation.h>

// 0 authorized, 1 denied, 2 restricted, 3 request unavailable/timed out.
// Called on a Rust blocking worker so the main run loop remains responsive.
int hearfolio_request_microphone_permission(void) {
    @autoreleasepool {
        AVAuthorizationStatus status = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
        if (status == AVAuthorizationStatusAuthorized) return 0;
        if (status == AVAuthorizationStatusDenied) return 1;
        if (status == AVAuthorizationStatusRestricted) return 2;
        dispatch_semaphore_t ready = dispatch_semaphore_create(0);
        __block BOOL allowed = NO;
        [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL granted) {
            allowed = granted;
            dispatch_semaphore_signal(ready);
        }];
        // The user may take time to choose; timeout is recoverable and never starts capture.
        if (dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 300 * NSEC_PER_SEC)) != 0) return 3;
        return allowed ? 0 : 1;
    }
}
