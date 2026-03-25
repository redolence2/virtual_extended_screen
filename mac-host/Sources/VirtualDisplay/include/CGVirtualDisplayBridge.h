#ifndef CGVirtualDisplayBridge_h
#define CGVirtualDisplayBridge_h

#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>

NS_ASSUME_NONNULL_BEGIN

/// Opaque wrapper around CGVirtualDisplay (private API).
/// Provides safe create/destroy lifecycle with configurable resolution.
@interface CGVirtualDisplayBridge : NSObject

/// The CGDirectDisplayID assigned by the system after creation.
/// Returns kCGNullDirectDisplay (0) if not created.
@property (nonatomic, readonly) CGDirectDisplayID displayID;

/// Whether a virtual display is currently active.
@property (nonatomic, readonly) BOOL isActive;

/// The vendor/product/serial numbers set on the descriptor (for identification).
@property (nonatomic, readonly) uint32_t vendorID;
@property (nonatomic, readonly) uint32_t productID;
@property (nonatomic, readonly) uint32_t serialNumber;

/// Creates a virtual display with the given resolution and refresh rate.
/// @param width Pixel width (e.g. 1920 or 3840)
/// @param height Pixel height (e.g. 1080 or 2160)
/// @param refreshRate Refresh rate in Hz (e.g. 60)
/// @param error On failure, set to an NSError describing the issue
/// @return YES on success, NO on failure
- (BOOL)createWithWidth:(NSUInteger)width
                 height:(NSUInteger)height
            refreshRate:(NSUInteger)refreshRate
                  error:(NSError **)error;

/// Destroys the virtual display. Safe to call multiple times.
- (void)destroy;

/// Returns YES if CGVirtualDisplay API appears available on this OS version.
+ (BOOL)isAPIAvailable;

/// Returns the current macOS build version string (e.g. "23F79").
+ (NSString *)osBuildVersion;

@end

NS_ASSUME_NONNULL_END

#endif /* CGVirtualDisplayBridge_h */
