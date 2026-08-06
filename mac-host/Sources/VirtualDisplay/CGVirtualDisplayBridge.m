#import "CGVirtualDisplayBridge.h"
#import <objc/runtime.h>
#import <objc/message.h>
#import <dlfcn.h>

// CGVirtualDisplay is a private API. We access it via Objective-C runtime.
// Classes: CGVirtualDisplay, CGVirtualDisplayDescriptor, CGVirtualDisplayMode, CGVirtualDisplaySettings

static NSString *const kErrorDomain = @"com.resc.virtualdisplay";

// Night Shift detection via CBBlueLightClient (CoreBrightness private framework)
float RESCGetNightShiftStrength(void) {
    static BOOL frameworkLoaded = NO;
    if (!frameworkLoaded) {
        NSBundle *bundle = [NSBundle bundleWithPath:@"/System/Library/PrivateFrameworks/CoreBrightness.framework"];
        [bundle load];
        frameworkLoaded = YES;
    }

    Class cls = NSClassFromString(@"CBBlueLightClient");
    if (!cls) return 0;

    // Create fresh client each call — cached instances don't see Night Shift toggle
    id client = [[cls alloc] init];
    SEL sel = NSSelectorFromString(@"getBlueLightStatus:");
    if (![client respondsToSelector:sel]) return 0;

    // Allocate oversized buffer for the status struct
    uint8_t statusBuf[128] = {0};

    // Use NSInvocation for safe method dispatch
    NSMethodSignature *sig = [client methodSignatureForSelector:sel];
    if (!sig) return 0;

    NSInvocation *inv = [NSInvocation invocationWithMethodSignature:sig];
    [inv setSelector:sel];
    void *ptr = statusBuf;
    [inv setArgument:&ptr atIndex:2];
    [inv invokeWithTarget:client];

    BOOL success = NO;
    [inv getReturnValue:&success];
    if (!success) return 0;

    // Struct layout (macOS 15 Sequoia):
    //   byte 0: enabled in settings (1 if Night Shift configured)
    //   byte 1: currently active (1 = ON, 0 = OFF)  <-- this is the toggle
    //   byte 2: schedule mode
    //   bytes 8+: schedule hours
    // Strength is not exposed in this struct; default to 0.5 when active.
    uint8_t active = statusBuf[1];
    return active ? 0.5f : 0.0f;
}

@interface CGVirtualDisplayBridge ()
@property (nonatomic, strong) id virtualDisplay;  // CGVirtualDisplay instance
@property (nonatomic, assign) CGDirectDisplayID cachedDisplayID;
@property (nonatomic, assign) uint32_t vendorID;
@property (nonatomic, assign) uint32_t productID;
@property (nonatomic, assign) uint32_t serialNumber;
@property (nonatomic, strong) dispatch_source_t retinaEnforceTimer;
@end

@implementation CGVirtualDisplayBridge

- (instancetype)init {
    self = [super init];
    if (self) {
        _cachedDisplayID = kCGNullDirectDisplay;
        // Use distinctive values for identification via CGDisplayVendorNumber etc.
        _vendorID = 0x0E5C;    // distinctive value for display enumeration
        _productID = 0x0001;
        _serialNumber = 0x52455343; // "RESC" — fixed so macOS remembers display arrangement
    }
    return self;
}

- (void)dealloc {
    [self destroy];
}

- (CGDirectDisplayID)displayID {
    if (self.virtualDisplay) {
        // Query live displayID from the CGVirtualDisplay object
        SEL sel = NSSelectorFromString(@"displayID");
        if ([self.virtualDisplay respondsToSelector:sel]) {
            NSMethodSignature *sig = [self.virtualDisplay methodSignatureForSelector:sel];
            NSInvocation *inv = [NSInvocation invocationWithMethodSignature:sig];
            [inv setSelector:sel];
            [inv setTarget:self.virtualDisplay];
            [inv invoke];
            CGDirectDisplayID result = 0;
            [inv getReturnValue:&result];
            self.cachedDisplayID = result;
            return result;
        }
    }
    return self.cachedDisplayID;
}

- (BOOL)isActive {
    return self.virtualDisplay != nil && self.displayID != kCGNullDirectDisplay;
}

+ (BOOL)isAPIAvailable {
    Class cls = NSClassFromString(@"CGVirtualDisplay");
    return cls != nil;
}

+ (NSString *)osBuildVersion {
    NSDictionary *sv = [NSDictionary dictionaryWithContentsOfFile:
                        @"/System/Library/CoreServices/SystemVersion.plist"];
    return sv[@"ProductBuildVersion"] ?: @"unknown";
}

- (BOOL)createWithWidth:(NSUInteger)width
                 height:(NSUInteger)height
            refreshRate:(NSUInteger)refreshRate
                  error:(NSError **)error {
    if (self.virtualDisplay) {
        if (error) {
            *error = [NSError errorWithDomain:kErrorDomain code:1
                                     userInfo:@{NSLocalizedDescriptionKey: @"Virtual display already exists. Destroy first."}];
        }
        return NO;
    }

    if (![CGVirtualDisplayBridge isAPIAvailable]) {
        if (error) {
            *error = [NSError errorWithDomain:kErrorDomain code:2
                                     userInfo:@{NSLocalizedDescriptionKey: @"CGVirtualDisplay API not available on this OS version."}];
        }
        return NO;
    }

    // 1. Create descriptor
    Class descriptorClass = NSClassFromString(@"CGVirtualDisplayDescriptor");
    id descriptor = [[descriptorClass alloc] init];
    if (!descriptor) {
        if (error) {
            *error = [NSError errorWithDomain:kErrorDomain code:3
                                     userInfo:@{NSLocalizedDescriptionKey: @"Failed to create CGVirtualDisplayDescriptor."}];
        }
        return NO;
    }

    // Set display name
    SEL setName = NSSelectorFromString(@"setName:");
    if ([descriptor respondsToSelector:setName]) {
        ((void (*)(id, SEL, id))objc_msgSend)(descriptor, setName, @"Remote Extended Screen");
    }

    // Set vendor/product/serial for identification
    SEL setVendor = NSSelectorFromString(@"setVendorID:");
    if ([descriptor respondsToSelector:setVendor]) {
        ((void (*)(id, SEL, uint32_t))objc_msgSend)(descriptor, setVendor, self.vendorID);
    }

    SEL setProduct = NSSelectorFromString(@"setProductID:");
    if ([descriptor respondsToSelector:setProduct]) {
        ((void (*)(id, SEL, uint32_t))objc_msgSend)(descriptor, setProduct, self.productID);
    }

    SEL setSerial = NSSelectorFromString(@"setSerialNum:");
    if ([descriptor respondsToSelector:setSerial]) {
        ((void (*)(id, SEL, uint32_t))objc_msgSend)(descriptor, setSerial, self.serialNumber);
    }

    // Set max pixel dimensions
    SEL setMaxWidth = NSSelectorFromString(@"setMaxPixelsWide:");
    if ([descriptor respondsToSelector:setMaxWidth]) {
        ((void (*)(id, SEL, NSUInteger))objc_msgSend)(descriptor, setMaxWidth, width);
    } else {
        NSLog(@"[RESC] WARNING: descriptor does not respond to setMaxPixelsWide:");
    }

    SEL setMaxHeight = NSSelectorFromString(@"setMaxPixelsHigh:");
    if ([descriptor respondsToSelector:setMaxHeight]) {
        ((void (*)(id, SEL, NSUInteger))objc_msgSend)(descriptor, setMaxHeight, height);
    } else {
        NSLog(@"[RESC] WARNING: descriptor does not respond to setMaxPixelsHigh:");
    }

    // Set physical size in millimeters (required for display registration)
    // Approximate 16:9 panel: landscape ~531x299mm, swapped to 299x531mm
    // when the requested mode is portrait (height > width) so macOS
    // registers the display with matching orientation and DPI.
    SEL setSizeInMM = NSSelectorFromString(@"setSizeInMillimeters:");
    if ([descriptor respondsToSelector:setSizeInMM]) {
        CGSize physicalSize = (height > width)
            ? CGSizeMake(299.0, 531.0)
            : CGSizeMake(531.0, 299.0);
        ((void (*)(id, SEL, CGSize))objc_msgSend)(descriptor, setSizeInMM, physicalSize);
        NSLog(@"[RESC] Set sizeInMillimeters: %.0fx%.0f", physicalSize.width, physicalSize.height);
    } else {
        NSLog(@"[RESC] WARNING: descriptor does not respond to setSizeInMillimeters:");
    }

    // Set queue for callbacks (use main queue for display management)
    SEL setQueue = NSSelectorFromString(@"setQueue:");
    if ([descriptor respondsToSelector:setQueue]) {
        ((void (*)(id, SEL, id))objc_msgSend)(descriptor, setQueue,
            dispatch_get_main_queue());
    }

    NSLog(@"[RESC] Descriptor configured: %lux%lu (sizeInMM follows orientation)", (unsigned long)width, (unsigned long)height);

    // 2. Create display mode — use alloc + designated init (NOT alloc+init+reinit)
    Class modeClass = NSClassFromString(@"CGVirtualDisplayMode");
    SEL modeInit = NSSelectorFromString(@"initWithWidth:height:refreshRate:");
    id mode = nil;
    if (modeClass && [modeClass instancesRespondToSelector:modeInit]) {
        mode = ((id (*)(id, SEL, NSUInteger, NSUInteger, double))objc_msgSend)(
            [modeClass alloc], modeInit, width, height, (double)refreshRate);
    }

    if (!mode) {
        if (error) {
            *error = [NSError errorWithDomain:kErrorDomain code:4
                                     userInfo:@{NSLocalizedDescriptionKey: @"Failed to create CGVirtualDisplayMode."}];
        }
        return NO;
    }

    // 3. Create settings
    Class settingsClass = NSClassFromString(@"CGVirtualDisplaySettings");
    id settings = [[settingsClass alloc] init];

    // Set the modes array. Retina (wantsHiDPI): a SINGLE full-size mode +
    // the hiDPI flag only yields derived low-res Retina modes (verified
    // live 2026-08-05: the largest @2x backing stopped at half the request,
    // so macOS rendered 1x and SCK upscaled — no sharpness gain).
    // Advertising BOTH the full-pixel mode and its half-point sibling lets
    // macOS pair them into a true Retina mode: points width/2 x height/2,
    // pixels width x height. That mode is selected post-creation below.
    SEL setModes = NSSelectorFromString(@"setModes:");
    if ([settings respondsToSelector:setModes]) {
        NSArray *modes = @[mode];
        if (self.wantsHiDPI && modeClass && [modeClass instancesRespondToSelector:modeInit]) {
            id modeHalf = ((id (*)(id, SEL, NSUInteger, NSUInteger, double))objc_msgSend)(
                [modeClass alloc], modeInit, width / 2, height / 2, (double)refreshRate);
            if (modeHalf) { modes = @[mode, modeHalf]; }
        }
        ((void (*)(id, SEL, id))objc_msgSend)(settings, setModes, modes);
    }

    // hiDPI per wantsHiDPI (NO = plain 1x display, the historical default)
    SEL setHiDPI = NSSelectorFromString(@"setHiDPI:");
    if ([settings respondsToSelector:setHiDPI]) {
        ((void (*)(id, SEL, BOOL))objc_msgSend)(settings, setHiDPI, self.wantsHiDPI);
        if (self.wantsHiDPI) { NSLog(@"[RESC] hiDPI enabled on virtual display settings"); }
    }

    // 4. Create the virtual display — alloc + designated init (NOT alloc+init+reinit)
    Class displayClass = NSClassFromString(@"CGVirtualDisplay");
    SEL displayInit = NSSelectorFromString(@"initWithDescriptor:");
    id display = nil;
    if (displayClass && [displayClass instancesRespondToSelector:displayInit]) {
        display = ((id (*)(id, SEL, id))objc_msgSend)([displayClass alloc], displayInit, descriptor);
    }

    if (!display) {
        if (error) {
            *error = [NSError errorWithDomain:kErrorDomain code:5
                                     userInfo:@{NSLocalizedDescriptionKey: @"Failed to create CGVirtualDisplay instance."}];
        }
        return NO;
    }

    // Apply settings
    SEL applySettings = NSSelectorFromString(@"applySettings:");
    if ([display respondsToSelector:applySettings]) {
        BOOL applied = ((BOOL (*)(id, SEL, id))objc_msgSend)(display, applySettings, settings);
        if (!applied) {
            NSLog(@"[RESC] WARNING: applySettings returned NO");
        }
    }

    self.virtualDisplay = display;

    CGDirectDisplayID did = self.displayID;
    NSLog(@"[RESC] Virtual display created: displayID=%u, vendor=0x%X, product=0x%X, serial=0x%X, size=%lux%lu@%luHz",
          did, self.vendorID, self.productID, self.serialNumber,
          (unsigned long)width, (unsigned long)height, (unsigned long)refreshRate);

    // Check if display appears in CG online list
    uint32_t count = 0;
    CGGetOnlineDisplayList(0, NULL, &count);
    CGDirectDisplayID *ids = malloc(sizeof(CGDirectDisplayID) * count);
    CGGetOnlineDisplayList(count, ids, &count);
    BOOL foundInCG = NO;
    for (uint32_t i = 0; i < count; i++) {
        if (ids[i] == did) { foundInCG = YES; break; }
    }
    free(ids);
    NSLog(@"[RESC] Display %u in CG online list: %@, total displays: %u", did, foundInCG ? @"YES" : @"NO", count);

    // Retina: select the paired mode (points width/2 x height/2 backed by
    // width x height PIXELS) so macOS renders 2x-crisp into the full
    // framebuffer. Mode registration is async after applySettings — retry
    // briefly until the Retina mode appears. NOTE: CGDisplayPixelsWide()
    // reports POINTS for Retina modes; only CGDisplayModeGetPixelWidth()
    // tells the truth about the backing store.
    if (self.wantsHiDPI) {
        for (int attempt = 0; attempt < 8; attempt++) {
            if ([self selectRetinaModeOnDisplay:did pixelWidth:width pixelHeight:height]) break;
            if (attempt == 7) NSLog(@"[RESC] WARNING: Retina mode (points %lux%lu, pixels %lux%lu) never appeared",
                                    (unsigned long)(width / 2), (unsigned long)(height / 2),
                                    (unsigned long)width, (unsigned long)height);
            usleep(250000);
        }
        [self startRetinaEnforcementForDisplay:did pixelWidth:width pixelHeight:height];
    }

    return YES;
}

// One pass: YES if the 2x pair (points w/2 x h/2, pixels w x h) is already the
// current mode or was just set. Silent when already correct.
- (BOOL)selectRetinaModeOnDisplay:(CGDirectDisplayID)did
                       pixelWidth:(size_t)width pixelHeight:(size_t)height {
    CGDisplayModeRef cur = CGDisplayCopyDisplayMode(did);
    if (cur) {
        BOOL ok = CGDisplayModeGetPixelWidth(cur) == width &&
                  CGDisplayModeGetWidth(cur) == width / 2;
        CGDisplayModeRelease(cur);
        if (ok) return YES;
    }
    BOOL selected = NO;
    NSDictionary *opts = @{(__bridge NSString *)kCGDisplayShowDuplicateLowResolutionModes: @YES};
    CFArrayRef all = CGDisplayCopyAllDisplayModes(did, (__bridge CFDictionaryRef)opts);
    if (all) {
        for (CFIndex i = 0; i < CFArrayGetCount(all); i++) {
            CGDisplayModeRef m = (CGDisplayModeRef)CFArrayGetValueAtIndex(all, i);
            if (CGDisplayModeGetPixelWidth(m) == width && CGDisplayModeGetPixelHeight(m) == height &&
                CGDisplayModeGetWidth(m) == width / 2 && CGDisplayModeGetHeight(m) == height / 2) {
                CGError err = CGDisplaySetDisplayMode(did, m, NULL);
                NSLog(@"[RESC] Retina mode selected: %zux%zu points / %zux%zu pixels (err=%d)",
                      CGDisplayModeGetWidth(m), CGDisplayModeGetHeight(m),
                      CGDisplayModeGetPixelWidth(m), CGDisplayModeGetPixelHeight(m), err);
                selected = (err == kCGErrorSuccess);
                break;
            }
        }
        CFRelease(all);
    }
    return selected;
}

// WindowServer restores stale saved modes asynchronously — seconds to minutes
// after creation (observed live: a streaming session reverted to 1080x1920@1x,
// blurring everything). Re-assert the 2x mode every 2s for the display's life.
- (void)startRetinaEnforcementForDisplay:(CGDirectDisplayID)did
                              pixelWidth:(size_t)width pixelHeight:(size_t)height {
    if (self.retinaEnforceTimer) return;
    dispatch_queue_t q = dispatch_get_global_queue(QOS_CLASS_UTILITY, 0);
    dispatch_source_t timer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, q);
    dispatch_source_set_timer(timer, dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC),
                              2 * NSEC_PER_SEC, NSEC_PER_SEC / 2);
    __weak __typeof__(self) weakSelf = self;
    dispatch_source_set_event_handler(timer, ^{
        __typeof__(self) s = weakSelf;
        if (!s) return;
        if (![s selectRetinaModeOnDisplay:did pixelWidth:width pixelHeight:height]) {
            NSLog(@"[RESC] WARNING: Retina re-assert failed (mode missing?) — will retry");
        }
    });
    dispatch_resume(timer);
    self.retinaEnforceTimer = timer;
}

- (void)destroy {
    if (self.retinaEnforceTimer) {
        dispatch_source_cancel(self.retinaEnforceTimer);
        self.retinaEnforceTimer = nil;
    }
    if (self.virtualDisplay) {
        NSLog(@"[RESC] Destroying virtual display: displayID=%u", self.cachedDisplayID);
        self.virtualDisplay = nil;
        self.cachedDisplayID = kCGNullDirectDisplay;
    }
}

@end
