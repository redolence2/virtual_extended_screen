#import "CGVirtualDisplayBridge.h"
#import <objc/runtime.h>
#import <objc/message.h>
#import <dlfcn.h>

// CGVirtualDisplay is a private API. We access it via Objective-C runtime.
// Classes: CGVirtualDisplay, CGVirtualDisplayDescriptor, CGVirtualDisplayMode, CGVirtualDisplaySettings

static NSString *const kErrorDomain = @"com.resc.virtualdisplay";

@interface CGVirtualDisplayBridge ()
@property (nonatomic, strong) id virtualDisplay;  // CGVirtualDisplay instance
@property (nonatomic, assign) CGDirectDisplayID cachedDisplayID;
@property (nonatomic, assign) uint32_t vendorID;
@property (nonatomic, assign) uint32_t productID;
@property (nonatomic, assign) uint32_t serialNumber;
@end

@implementation CGVirtualDisplayBridge

- (instancetype)init {
    self = [super init];
    if (self) {
        _cachedDisplayID = kCGNullDirectDisplay;
        // Use distinctive values for identification via CGDisplayVendorNumber etc.
        _vendorID = 0x0E5C;    // distinctive value for display enumeration
        _productID = 0x0001;
        _serialNumber = arc4random();  // random per instance for uniqueness
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
    // Approximate: 24" 16:9 monitor = ~531mm x 299mm
    SEL setSizeInMM = NSSelectorFromString(@"setSizeInMillimeters:");
    if ([descriptor respondsToSelector:setSizeInMM]) {
        CGSize physicalSize = CGSizeMake(531.0, 299.0);
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

    NSLog(@"[RESC] Descriptor configured: %lux%lu, sizeInMM=531x299", (unsigned long)width, (unsigned long)height);

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

    // Set the modes array
    SEL setModes = NSSelectorFromString(@"setModes:");
    if ([settings respondsToSelector:setModes]) {
        ((void (*)(id, SEL, id))objc_msgSend)(settings, setModes, @[mode]);
    }

    // Set hiDPI = NO (non-Retina, scale 1.0 as per spec)
    SEL setHiDPI = NSSelectorFromString(@"setHiDPI:");
    if ([settings respondsToSelector:setHiDPI]) {
        ((void (*)(id, SEL, BOOL))objc_msgSend)(settings, setHiDPI, NO);
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

    return YES;
}

- (void)destroy {
    if (self.virtualDisplay) {
        NSLog(@"[RESC] Destroying virtual display: displayID=%u", self.cachedDisplayID);
        self.virtualDisplay = nil;
        self.cachedDisplayID = kCGNullDirectDisplay;
    }
}

@end
