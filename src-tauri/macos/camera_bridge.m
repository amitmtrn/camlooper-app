// App-side half of the macOS virtual camera. Compiled by build.rs, called from
// src/macos_virtual_camera.rs.
//
// Two jobs:
//   1. Ask macOS to activate the Camera Extension bundled in
//      CamLooper.app/Contents/Library/SystemExtensions (OSSystemExtensionRequest).
//   2. Find the extension's device through CoreMediaIO and write BGRA frames into its sink
//      stream. The extension forwards them to whoever is reading the camera.
//
// Objective-C rather than Rust because both APIs are delegate- and CoreFoundation-heavy,
// and this is ~300 lines that would otherwise be ~300 lines of unsafe bindings.

#import <AVFoundation/AVFoundation.h>
#import <AppKit/AppKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreMediaIO/CMIOHardware.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#import <SystemExtensions/SystemExtensions.h>

// Keep in step with the constants in src/macos_virtual_camera.rs.
enum {
    CL_OK = 0,
    CL_NEEDS_APPROVAL = 1,
    CL_FAILED = 2,
    CL_TIMEOUT = 3,
    CL_NOT_BUNDLED = 4,
    CL_DEVICE_NOT_FOUND = 5,
    CL_CAMERA_DENIED = 6,
};

static NSString *const kExtensionBundleID = @"com.camlooper.app.CameraExtension";
static NSString *const kActivatedVersionKey = @"CamLooperActivatedExtensionVersion";

static void cl_set_error(char *err, size_t errlen, NSString *message) {
    if (err == NULL || errlen == 0) return;
    const char *utf8 = message.UTF8String ?: "";
    strlcpy(err, utf8, errlen);
}

// ---------------------------------------------------------------------------------------
// Extension activation
// ---------------------------------------------------------------------------------------

/// "<short version>/<build>" of the extension inside this app bundle, or nil when the app
/// is not running from a bundle that contains one (`tauri dev`, an unsigned build).
static NSString *cl_bundled_extension_version(void) {
    NSString *path = [[NSBundle mainBundle].bundlePath
        stringByAppendingPathComponent:
            [NSString stringWithFormat:@"Contents/Library/SystemExtensions/%@.systemextension",
                                       kExtensionBundleID]];
    NSBundle *ext = [NSBundle bundleWithPath:path];
    if (ext == nil) return nil;
    NSString *shortVersion = [ext objectForInfoDictionaryKey:@"CFBundleShortVersionString"];
    NSString *build = [ext objectForInfoDictionaryKey:@"CFBundleVersion"];
    if (shortVersion == nil || build == nil) return nil;
    return [NSString stringWithFormat:@"%@/%@", shortVersion, build];
}

@interface CLActivationDelegate : NSObject <OSSystemExtensionRequestDelegate>
@property(nonatomic, strong) dispatch_semaphore_t done;
@property(atomic) int status;
@property(atomic, copy) NSString *message;
@property(atomic, copy) NSString *version;
@end

@implementation CLActivationDelegate

- (OSSystemExtensionReplacementAction)request:(OSSystemExtensionRequest *)request
                  actionForReplacingExtension:(OSSystemExtensionProperties *)existing
                                withExtension:(OSSystemExtensionProperties *)ext {
    return OSSystemExtensionReplacementActionReplace;
}

- (void)requestNeedsUserApproval:(OSSystemExtensionRequest *)request {
    // The request stays alive and completes later, once the user flips the switch in
    // System Settings. Report back now so the UI can tell them to go and do that.
    self.status = CL_NEEDS_APPROVAL;
    dispatch_semaphore_signal(self.done);
}

- (void)request:(OSSystemExtensionRequest *)request
    didFinishWithResult:(OSSystemExtensionRequestResult)result {
    if (result == OSSystemExtensionRequestCompleted) {
        if (self.version != nil) {
            [[NSUserDefaults standardUserDefaults] setObject:self.version
                                                      forKey:kActivatedVersionKey];
        }
        self.status = CL_OK;
    } else {
        self.status = CL_FAILED;
        self.message = @"macOS will finish installing the camera extension after a restart.";
    }
    dispatch_semaphore_signal(self.done);
}

- (void)request:(OSSystemExtensionRequest *)request didFailWithError:(NSError *)error {
    self.status = CL_FAILED;
    self.message = [NSString stringWithFormat:@"%@ (%@ %ld)", error.localizedDescription,
                                              error.domain, (long)error.code];
    dispatch_semaphore_signal(self.done);
}

@end

// Held for the life of the process: a request that is waiting on user approval must not
// lose its delegate.
static CLActivationDelegate *gActivationDelegate = nil;

/// Whether the bundled extension still needs an activation request: it has never been
/// activated from this app, or the bundle now carries a different version.
int camlooper_extension_needs_activation(void) {
    @autoreleasepool {
        NSString *bundled = cl_bundled_extension_version();
        if (bundled == nil) return 0;
        NSString *activated =
            [[NSUserDefaults standardUserDefaults] stringForKey:kActivatedVersionKey];
        return [bundled isEqualToString:activated ?: @""] ? 0 : 1;
    }
}

/// 1 when this app bundle contains the camera extension (a signed install), 0 for
/// `tauri dev` and other builds that cannot install it.
int camlooper_extension_bundled(void) {
    @autoreleasepool {
        return cl_bundled_extension_version() != nil ? 1 : 0;
    }
}

/// Open the System Settings pane where the user approves the camera extension.
void camlooper_open_extension_settings(void) {
    @autoreleasepool {
        // macOS 15 moved extension approval to General > Login Items & Extensions; before
        // that it is a button at the bottom of Privacy & Security.
        NSString *url = [NSProcessInfo processInfo].operatingSystemVersion.majorVersion >= 15
                            ? @"x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
                            : @"x-apple.systempreferences:com.apple.preference.security?Security";
        dispatch_async(dispatch_get_main_queue(), ^{
            [[NSWorkspace sharedWorkspace] openURL:[NSURL URLWithString:url]];
        });
    }
}

/// Submit an activation request and wait up to `timeout_s` for macOS to answer.
/// Must not be called on the main thread: the delegate is called back there.
int camlooper_extension_activate(double timeout_s, char *err, size_t errlen) {
    @autoreleasepool {
        NSString *bundled = cl_bundled_extension_version();
        if (bundled == nil) {
            cl_set_error(err, errlen,
                         @"This build does not contain the camera extension. It is only "
                         @"embedded in a signed CamLooper.app.");
            return CL_NOT_BUNDLED;
        }

        CLActivationDelegate *delegate = [CLActivationDelegate new];
        delegate.done = dispatch_semaphore_create(0);
        delegate.status = CL_TIMEOUT;
        delegate.version = bundled;
        gActivationDelegate = delegate;

        dispatch_async(dispatch_get_main_queue(), ^{
            OSSystemExtensionRequest *request =
                [OSSystemExtensionRequest activationRequestForExtension:kExtensionBundleID
                                                                  queue:dispatch_get_main_queue()];
            request.delegate = delegate;
            [[OSSystemExtensionManager sharedManager] submitRequest:request];
        });

        dispatch_time_t deadline =
            dispatch_time(DISPATCH_TIME_NOW, (int64_t)(timeout_s * NSEC_PER_SEC));
        if (dispatch_semaphore_wait(delegate.done, deadline) != 0) {
            cl_set_error(err, errlen, @"macOS did not answer the extension request in time.");
            return CL_TIMEOUT;
        }
        if (delegate.message != nil) cl_set_error(err, errlen, delegate.message);
        return delegate.status;
    }
}

// ---------------------------------------------------------------------------------------
// CoreMediaIO sink
// ---------------------------------------------------------------------------------------

static NSLock *gSinkLock = nil;
static CMIODeviceID gDevice = 0;
static CMIOStreamID gStream = 0;
static CMSimpleQueueRef gQueue = NULL;
static CVPixelBufferPoolRef gPool = NULL;
static CMVideoFormatDescriptionRef gFormat = NULL;
static int gWidth = 0, gHeight = 0, gFps = 30;

static NSLock *cl_sink_lock(void) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        gSinkLock = [NSLock new];
    });
    return gSinkLock;
}

static NSArray<NSNumber *> *cl_object_ids(CMIOObjectID object, CMIOObjectPropertySelector selector) {
    CMIOObjectPropertyAddress address = {selector, kCMIOObjectPropertyScopeGlobal,
                                         kCMIOObjectPropertyElementMain};
    UInt32 size = 0;
    if (CMIOObjectGetPropertyDataSize(object, &address, 0, NULL, &size) != noErr || size == 0) {
        return @[];
    }
    NSMutableData *data = [NSMutableData dataWithLength:size];
    UInt32 used = 0;
    if (CMIOObjectGetPropertyData(object, &address, 0, NULL, size, &used, data.mutableBytes) !=
        noErr) {
        return @[];
    }
    NSMutableArray<NSNumber *> *ids = [NSMutableArray array];
    const UInt32 *raw = data.bytes;
    for (UInt32 i = 0; i < used / sizeof(UInt32); i++) {
        [ids addObject:@(raw[i])];
    }
    return ids;
}

static NSString *cl_object_name(CMIOObjectID object) {
    CMIOObjectPropertyAddress address = {kCMIOObjectPropertyName, kCMIOObjectPropertyScopeGlobal,
                                         kCMIOObjectPropertyElementMain};
    CFStringRef name = NULL;
    UInt32 used = 0;
    if (CMIOObjectGetPropertyData(object, &address, 0, NULL, sizeof(name), &used, &name) != noErr ||
        name == NULL) {
        return nil;
    }
    return CFBridgingRelease(name);
}

static CMIODeviceID cl_find_device(NSString *wanted) {
    for (NSNumber *device in cl_object_ids(kCMIOObjectSystemObject, kCMIOHardwarePropertyDevices)) {
        if ([cl_object_name(device.unsignedIntValue) isEqualToString:wanted]) {
            return device.unsignedIntValue;
        }
    }
    return 0;
}

/// The device's sink stream. CoreMediaIO reports direction from the host's point of view:
/// 1 is input (the camera's ordinary source stream), 0 is output (something we write to).
static CMIOStreamID cl_find_sink_stream(CMIODeviceID device) {
    NSArray<NSNumber *> *streams = cl_object_ids(device, kCMIODevicePropertyStreams);
    for (NSNumber *stream in streams) {
        CMIOObjectPropertyAddress address = {kCMIOStreamPropertyDirection,
                                             kCMIOObjectPropertyScopeGlobal,
                                             kCMIOObjectPropertyElementMain};
        UInt32 direction = 1, used = 0;
        if (CMIOObjectGetPropertyData(stream.unsignedIntValue, &address, 0, NULL, sizeof(direction),
                                      &used, &direction) == noErr &&
            direction == 0) {
            return stream.unsignedIntValue;
        }
    }
    return 0;
}

static void cl_queue_altered(__unused CMIOStreamID stream, __unused void *token,
                             __unused void *refcon) {
    // The extension took a buffer. Nothing to do: pushes are paced by ffmpeg.
}

static int cl_camera_access(char *err, size_t errlen) {
    AVAuthorizationStatus status =
        [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeVideo];
    if (status == AVAuthorizationStatusNotDetermined) {
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        [AVCaptureDevice requestAccessForMediaType:AVMediaTypeVideo
                                 completionHandler:^(__unused BOOL granted) {
                                     dispatch_semaphore_signal(done);
                                 }];
        dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 120 * NSEC_PER_SEC));
        status = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeVideo];
    }
    if (status != AVAuthorizationStatusAuthorized) {
        cl_set_error(err, errlen,
                     @"CamLooper is not allowed to use cameras. Enable it under System "
                     @"Settings > Privacy & Security > Camera, then try again.");
        return CL_CAMERA_DENIED;
    }
    return CL_OK;
}

/// 1 when a camera with this name is visible to CoreMediaIO.
int camlooper_device_present(const char *name) {
    @autoreleasepool {
        return cl_find_device(@(name)) != 0 ? 1 : 0;
    }
}

static void cl_sink_teardown_locked(void) {
    if (gDevice != 0 && gStream != 0) {
        CMIODeviceStopStream(gDevice, gStream);
    }
    if (gQueue != NULL) {
        CFRelease(gQueue);
        gQueue = NULL;
    }
    if (gPool != NULL) {
        CVPixelBufferPoolRelease(gPool);
        gPool = NULL;
    }
    if (gFormat != NULL) {
        CFRelease(gFormat);
        gFormat = NULL;
    }
    gDevice = 0;
    gStream = 0;
}

int camlooper_sink_open(const char *name, int width, int height, int fps, char *err,
                        size_t errlen) {
    @autoreleasepool {
        // CoreMediaIO hides every camera from a process without camera permission, so a
        // denial would otherwise look exactly like "extension not installed".
        int access = cl_camera_access(err, errlen);
        if (access != CL_OK) return access;

        NSLock *lock = cl_sink_lock();
        [lock lock];
        cl_sink_teardown_locked();

        CMIODeviceID device = cl_find_device(@(name));
        if (device == 0) {
            [lock unlock];
            cl_set_error(err, errlen, @"The CamLooper camera device is not registered.");
            return CL_DEVICE_NOT_FOUND;
        }
        CMIOStreamID stream = cl_find_sink_stream(device);
        if (stream == 0) {
            [lock unlock];
            cl_set_error(err, errlen,
                         @"The CamLooper camera has no sink stream. An older extension is "
                         @"probably still installed.");
            return CL_FAILED;
        }

        CMSimpleQueueRef queue = NULL;
        OSStatus status = CMIOStreamCopyBufferQueue(stream, cl_queue_altered, NULL, &queue);
        if (status != noErr || queue == NULL) {
            [lock unlock];
            cl_set_error(err, errlen,
                         [NSString stringWithFormat:@"CMIOStreamCopyBufferQueue failed (%d).",
                                                    (int)status]);
            return CL_FAILED;
        }

        NSDictionary *attributes = @{
            (id)kCVPixelBufferWidthKey : @(width),
            (id)kCVPixelBufferHeightKey : @(height),
            (id)kCVPixelBufferPixelFormatTypeKey : @(kCVPixelFormatType_32BGRA),
            (id)kCVPixelBufferIOSurfacePropertiesKey : @{},
        };
        CVPixelBufferPoolRef pool = NULL;
        CVPixelBufferPoolCreate(kCFAllocatorDefault, NULL, (__bridge CFDictionaryRef)attributes,
                                &pool);
        CMVideoFormatDescriptionRef format = NULL;
        CMVideoFormatDescriptionCreate(kCFAllocatorDefault, kCVPixelFormatType_32BGRA, width,
                                       height, NULL, &format);
        if (pool == NULL || format == NULL) {
            if (pool != NULL) CVPixelBufferPoolRelease(pool);
            if (format != NULL) CFRelease(format);
            CFRelease(queue);
            [lock unlock];
            cl_set_error(err, errlen, @"Could not allocate the frame buffer pool.");
            return CL_FAILED;
        }

        status = CMIODeviceStartStream(device, stream);
        if (status != noErr) {
            CVPixelBufferPoolRelease(pool);
            CFRelease(format);
            CFRelease(queue);
            [lock unlock];
            cl_set_error(err, errlen,
                         [NSString stringWithFormat:@"CMIODeviceStartStream failed (%d).",
                                                    (int)status]);
            return CL_FAILED;
        }

        gDevice = device;
        gStream = stream;
        gQueue = queue;
        gPool = pool;
        gFormat = format;
        gWidth = width;
        gHeight = height;
        gFps = fps > 0 ? fps : 30;
        [lock unlock];
        return CL_OK;
    }
}

/// Push one top-down BGRA frame of exactly width*height*4 bytes. Returns CL_OK when the
/// frame was queued or deliberately dropped because the extension has not drained the
/// previous one; CL_FAILED when the sink is not open or the frame is the wrong size.
int camlooper_sink_push(const uint8_t *bgra, size_t len) {
    @autoreleasepool {
        NSLock *lock = cl_sink_lock();
        [lock lock];
        if (gQueue == NULL || gPool == NULL || len != (size_t)gWidth * (size_t)gHeight * 4) {
            [lock unlock];
            return CL_FAILED;
        }
        if (CMSimpleQueueGetCount(gQueue) >= CMSimpleQueueGetCapacity(gQueue)) {
            [lock unlock];
            return CL_OK;
        }

        CVPixelBufferRef pixels = NULL;
        if (CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, gPool, &pixels) !=
                kCVReturnSuccess ||
            pixels == NULL) {
            [lock unlock];
            return CL_FAILED;
        }
        CVPixelBufferLockBaseAddress(pixels, 0);
        uint8_t *base = CVPixelBufferGetBaseAddress(pixels);
        size_t stride = CVPixelBufferGetBytesPerRow(pixels);
        size_t row = (size_t)gWidth * 4;
        if (stride == row) {
            memcpy(base, bgra, len);
        } else {
            for (int y = 0; y < gHeight; y++) {
                memcpy(base + (size_t)y * stride, bgra + (size_t)y * row, row);
            }
        }
        CVPixelBufferUnlockBaseAddress(pixels, 0);

        CMSampleTimingInfo timing = {
            .duration = CMTimeMake(1, gFps),
            .presentationTimeStamp = CMClockGetTime(CMClockGetHostTimeClock()),
            .decodeTimeStamp = kCMTimeInvalid,
        };
        CMSampleBufferRef sample = NULL;
        OSStatus status = CMSampleBufferCreateForImageBuffer(kCFAllocatorDefault, pixels, true,
                                                             NULL, NULL, gFormat, &timing,
                                                             &sample);
        CVPixelBufferRelease(pixels);
        if (status != noErr || sample == NULL) {
            [lock unlock];
            return CL_FAILED;
        }
        // On success the queue owns our +1 reference; the extension side releases it.
        if (CMSimpleQueueEnqueue(gQueue, sample) != noErr) {
            CFRelease(sample);
        }
        [lock unlock];
        return CL_OK;
    }
}

void camlooper_sink_close(void) {
    @autoreleasepool {
        NSLock *lock = cl_sink_lock();
        [lock lock];
        cl_sink_teardown_locked();
        [lock unlock];
    }
}
