// Objective-C exception safety wrappers for AXUIElement API calls.
//
// These functions wrap macOS accessibility API calls in @try/@catch blocks
// so that ObjC exceptions (NSException) are caught at the C level and never
// unwind through Rust frames. This is necessary because Rust's stable ABI
// aborts on foreign exceptions in extern "C" functions.

#import <ApplicationServices/ApplicationServices.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreText/CoreText.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#include <math.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

// ── Attribute Access ─────────────────────────────────────────────────────────

// Safe wrapper for AXUIElementCopyAttributeValue.
// Returns the AX error code, or -9999 if an ObjC exception was thrown.
int safe_ax_copy_attribute_value(
    AXUIElementRef element,
    CFStringRef attribute,
    CFTypeRef *value
) {
    @try {
        return AXUIElementCopyAttributeValue(element, attribute, value);
    } @catch (NSException *e) {
        *value = NULL;
        return -9999;
    }
}

// ── Batch Attribute Access ───────────────────────────────────────────────────

// Safe wrapper for AXUIElementCopyMultipleAttributeValues.
// Fetches multiple attributes in a single Mach IPC round-trip.
// Returns the AX error code, or -9999 if an ObjC exception was thrown.
int safe_ax_copy_multiple_attribute_values(
    AXUIElementRef element,
    CFArrayRef attributes,
    CFArrayRef *values
) {
    @try {
        // 0 = kAXCopyMultipleAttributeOptionStopOnError — don't stop on error
        return AXUIElementCopyMultipleAttributeValues(element, attributes, 0, values);
    } @catch (NSException *e) {
        *values = NULL;
        return -9999;
    }
}

// ── Action Names ─────────────────────────────────────────────────────────────

// Safe wrapper for AXUIElementCopyActionNames.
// Returns the AX error code, or -9999 if an ObjC exception was thrown.
int safe_ax_copy_action_names(
    AXUIElementRef element,
    CFArrayRef *names
) {
    @try {
        return AXUIElementCopyActionNames(element, names);
    } @catch (NSException *e) {
        *names = NULL;
        return -9999;
    }
}

// ── Perform Action ───────────────────────────────────────────────────────────

// Safe wrapper for AXUIElementPerformAction.
// Returns the AX error code, or -9999 if an ObjC exception was thrown.
int safe_ax_perform_action(
    AXUIElementRef element,
    CFStringRef action
) {
    @try {
        return AXUIElementPerformAction(element, action);
    } @catch (NSException *e) {
        return -9999;
    }
}

// ── Set Attribute ────────────────────────────────────────────────────────────

// Safe wrapper for AXUIElementSetAttributeValue.
// Returns the AX error code, or -9999 if an ObjC exception was thrown.
int safe_ax_set_attribute_value(
    AXUIElementRef element,
    CFStringRef attribute,
    CFTypeRef value
) {
    @try {
        return AXUIElementSetAttributeValue(element, attribute, value);
    } @catch (NSException *e) {
        return -9999;
    }
}

// Safe wrapper for AXUIElementIsAttributeSettable. Converts the MacTypes
// Boolean out-param to a C99 bool so the Rust side can use `*mut bool`.
int safe_ax_is_attribute_settable(
    AXUIElementRef element,
    CFStringRef attribute,
    bool *outSettable
) {
    Boolean settable = false;
    @try {
        AXError err = AXUIElementIsAttributeSettable(element, attribute, &settable);
        *outSettable = (settable != 0);
        return err;
    } @catch (NSException *e) {
        *outSettable = false;
        return -9999;
    }
}

// ── Window List ──────────────────────────────────────────────────────────────

// Safe wrapper for CGWindowListCopyWindowInfo.
// Returns NULL if an ObjC exception was thrown.
CFArrayRef safe_cg_window_list_copy(uint32_t option, uint32_t relativeToWindow) {
    @try {
        return CGWindowListCopyWindowInfo(option, relativeToWindow);
    } @catch (NSException *e) {
        return NULL;
    }
}

// Safe wrapper for CGPreflightScreenCaptureAccess (macOS 10.15+): whether this
// process has Screen Recording access, without prompting. Returns false if an
// ObjC exception was thrown.
bool safe_cg_preflight_screen_capture_access(void) {
    @try {
        return CGPreflightScreenCaptureAccess();
    } @catch (NSException *e) {
        return false;
    }
}

// ── Create Application Element ───────────────────────────────────────────────

// Safe wrapper for AXUIElementCreateApplication.
// Returns NULL if an ObjC exception was thrown.
AXUIElementRef safe_ax_create_application(int pid) {
    @try {
        return AXUIElementCreateApplication(pid);
    } @catch (NSException *e) {
        return NULL;
    }
}

// ── System-Wide Element / Focused Application ────────────────────────────────

// Safe wrapper for AXUIElementCreateSystemWide.
// The system-wide element is the entry point for global attributes such as
// kAXFocusedApplicationAttribute. Returns NULL if an ObjC exception was thrown.
AXUIElementRef safe_ax_create_system_wide(void) {
    @try {
        return AXUIElementCreateSystemWide();
    } @catch (NSException *e) {
        return NULL;
    }
}

// Safe wrapper for AXUIElementGetPid. Writes the owning process id of
// `element` to `*outPid`. Returns the AX error code, or -9999 if an ObjC
// exception was thrown.
int safe_ax_get_pid(AXUIElementRef element, int *outPid) {
    @try {
        pid_t pid = 0;
        AXError err = AXUIElementGetPid(element, &pid);
        *outPid = (int)pid;
        return err;
    } @catch (NSException *e) {
        *outPid = 0;
        return -9999;
    }
}

// ── Messaging Timeout ───────────────────────────────────────────────────────

// Safe wrapper for AXUIElementSetMessagingTimeout. Apple's header
// (AXUIElement.h) scopes it by the element it is called on: on the system-wide
// object it sets the global timeout for this process, and on any other object
// it sets the timeout for that object only, not for other objects equal to it.
// A value of 0 resets the system-wide object to the default, and makes any
// other object use the current global value. MacOSProvider::new sets the
// global value (AX_MESSAGING_TIMEOUT_SECS in ax.rs); the shell scan tightens
// the application elements it reads and resets them to 0 afterwards (see
// ShellProbeBound in ax.rs).
// Returns the AX error code, or -9999 if an ObjC exception was thrown.
int safe_ax_set_messaging_timeout(AXUIElementRef element, float timeoutInSeconds) {
    @try {
        return AXUIElementSetMessagingTimeout(element, timeoutInSeconds);
    } @catch (NSException *e) {
        return -9999;
    }
}

// ── AXValue Extraction ──────────────────────────────────────────────────────

// Safe wrapper for AXValueGetValue.
// Returns false if an ObjC exception was thrown.
Boolean safe_ax_value_get_value(AXValueRef value, AXValueType type, void *valuePtr) {
    @try {
        return AXValueGetValue(value, type, valuePtr);
    } @catch (NSException *e) {
        return false;
    }
}

// ── CoreFoundation Helpers ──────────────────────────────────────────────────

// Safe CFRetain — some apps return objects that throw on retain.
CFTypeRef safe_cf_retain(CFTypeRef cf) {
    @try {
        if (cf != NULL) {
            return CFRetain(cf);
        }
        return NULL;
    } @catch (NSException *e) {
        return NULL;
    }
}

// Safe CFRelease.
void safe_cf_release(CFTypeRef cf) {
    @try {
        if (cf != NULL) {
            CFRelease(cf);
        }
    } @catch (NSException *e) {
        // Swallow exception — leaking is better than crashing
    }
}

// Safe CFGetTypeID.
CFTypeID safe_cf_get_type_id(CFTypeRef cf) {
    @try {
        if (cf != NULL) {
            return CFGetTypeID(cf);
        }
        return 0;
    } @catch (NSException *e) {
        return 0;
    }
}

// Safe CFArrayGetCount.
CFIndex safe_cf_array_get_count(CFArrayRef arr) {
    @try {
        if (arr != NULL) {
            return CFArrayGetCount(arr);
        }
        return 0;
    } @catch (NSException *e) {
        return 0;
    }
}

// Safe CFArrayGetValueAtIndex.
CFTypeRef safe_cf_array_get_value(CFArrayRef arr, CFIndex idx) {
    @try {
        if (arr != NULL) {
            return CFArrayGetValueAtIndex(arr, idx);
        }
        return NULL;
    } @catch (NSException *e) {
        return NULL;
    }
}

// Safe CFBooleanGetValue.
Boolean safe_cf_boolean_get_value(CFTypeRef b) {
    @try {
        if (b != NULL) {
            return CFBooleanGetValue(b);
        }
        return false;
    } @catch (NSException *e) {
        return false;
    }
}

// Safe CFEqual. AXUIElementRefs compare by underlying accessibility object,
// which is what the container-selection membership check in ax.rs relies on.
Boolean safe_cf_equal(CFTypeRef a, CFTypeRef b) {
    @try {
        if (a != NULL && b != NULL) {
            return CFEqual(a, b);
        }
        return false;
    } @catch (NSException *e) {
        return false;
    }
}

// Safe CFHash. Consistent with CFEqual, so AXUIElementRefs to the same
// accessibility object hash alike; used to bucket candidates before the
// CFEqual comparison when de-duplicating search results. Returns 0 (one shared
// bucket, still correct) if an ObjC exception was thrown.
unsigned long safe_cf_hash(CFTypeRef cf) {
    @try {
        if (cf != NULL) {
            return CFHash(cf);
        }
        return 0;
    } @catch (NSException *e) {
        return 0;
    }
}

// Safe CFNumberGetValue.
Boolean safe_cf_number_get_value(CFNumberRef num, CFNumberType type, void *valuePtr) {
    @try {
        if (num != NULL) {
            return CFNumberGetValue(num, type, valuePtr);
        }
        return false;
    } @catch (NSException *e) {
        return false;
    }
}

// Safe CFDictionaryGetValue.
CFTypeRef safe_cf_dict_get_value(CFDictionaryRef dict, CFTypeRef key) {
    @try {
        if (dict != NULL && key != NULL) {
            return CFDictionaryGetValue(dict, key);
        }
        return NULL;
    } @catch (NSException *e) {
        return NULL;
    }
}

// Safe wrappers for the well-known CF type-ID functions. These are called
// frequently (once per type check) so we wrap each one to keep the call sites
// uniform with the rest of the safe_cf_* API. They're defined in system
// frameworks and shouldn't throw, but we wrap them for consistency and
// defence-in-depth.
CFTypeID safe_cf_string_get_type_id(void) {
    @try {
        return CFStringGetTypeID();
    } @catch (NSException *e) {
        return 0;
    }
}

CFTypeID safe_cf_url_get_type_id(void) {
    @try {
        return CFURLGetTypeID();
    } @catch (NSException *e) {
        return 0;
    }
}

/// The URL's string, borrowed from the URL (Get rule); NULL on failure.
CFStringRef safe_cf_url_get_string(CFTypeRef url) {
    @try {
        if (url != NULL) {
            return CFURLGetString((CFURLRef)url);
        }
        return NULL;
    } @catch (NSException *e) {
        return NULL;
    }
}

CFTypeID safe_cf_number_get_type_id(void) {
    @try {
        return CFNumberGetTypeID();
    } @catch (NSException *e) {
        return 0;
    }
}

CFTypeID safe_cf_boolean_get_type_id(void) {
    @try {
        return CFBooleanGetTypeID();
    } @catch (NSException *e) {
        return 0;
    }
}

CFTypeID safe_cf_array_get_type_id(void) {
    @try {
        return CFArrayGetTypeID();
    } @catch (NSException *e) {
        return 0;
    }
}

// Safe CFArrayCreate. Creates a new CFArray from an array of CFTypeRef
// pointers using kCFTypeArrayCallBacks (retains each value). Returns NULL
// on failure or if an ObjC exception was thrown.
CFArrayRef safe_cf_array_create(const void **values, CFIndex num_values) {
    @try {
        return CFArrayCreate(NULL, values, num_values, &kCFTypeArrayCallBacks);
    } @catch (NSException *e) {
        return NULL;
    }
}

// ── Process Trust ─────────────────────────────────────────────────────────────

// Safe wrapper for AXIsProcessTrusted.
Boolean safe_ax_is_process_trusted(void) {
    @try {
        return AXIsProcessTrusted();
    } @catch (NSException *e) {
        return false;
    }
}

// ── AXObserver Helpers (for EventProvider) ───────────────────────────────────

// Create an AXObserver for a given PID with a callback.
int safe_ax_observer_create(pid_t pid, AXObserverCallback callback, AXObserverRef *outObserver) {
    @try {
        return AXObserverCreate(pid, callback, outObserver);
    } @catch (NSException *e) {
        *outObserver = NULL;
        return -9999;
    }
}

// Add a notification to an AXObserver.
int safe_ax_observer_add_notification(AXObserverRef observer, AXUIElementRef element,
                                       CFStringRef notification, void *refcon) {
    @try {
        return AXObserverAddNotification(observer, element, notification, refcon);
    } @catch (NSException *e) {
        return -9999;
    }
}

// Remove a notification from an AXObserver.
int safe_ax_observer_remove_notification(AXObserverRef observer, AXUIElementRef element,
                                          CFStringRef notification) {
    @try {
        return AXObserverRemoveNotification(observer, element, notification);
    } @catch (NSException *e) {
        return -9999;
    }
}

// Get the RunLoop source for an AXObserver.
CFRunLoopSourceRef safe_ax_observer_get_run_loop_source(AXObserverRef observer) {
    @try {
        return AXObserverGetRunLoopSource(observer);
    } @catch (NSException *e) {
        return NULL;
    }
}

// Add a source to the current thread's RunLoop (default mode).
void safe_cf_run_loop_add_source(CFRunLoopSourceRef source) {
    @try {
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
    } @catch (NSException *e) {
        // swallow
    }
}

// Get the current thread's RunLoop.
CFRunLoopRef safe_cf_run_loop_get_current(void) {
    return CFRunLoopGetCurrent();
}

// Run the current thread's RunLoop (blocks until stopped).
void safe_cf_run_loop_run(void) {
    CFRunLoopRun();
}

// Stop a RunLoop (thread-safe, can be called from any thread).
void safe_cf_run_loop_stop(CFRunLoopRef rl) {
    @try {
        if (rl != NULL) {
            CFRunLoopStop(rl);
        }
    } @catch (NSException *e) {
        // swallow
    }
}

// Create an AXValue containing a CFRange (for AXSelectedTextRange).
CFTypeRef safe_ax_value_create_cf_range(CFIndex location, CFIndex length) {
    @try {
        CFRange range = CFRangeMake(location, length);
        return AXValueCreate(kAXValueCFRangeType, &range);
    } @catch (NSException *e) {
        return NULL;
    }
}

// Create an AXValue containing a CGPoint (for AXPosition).
CFTypeRef safe_ax_value_create_cg_point(double x, double y) {
    @try {
        CGPoint point = CGPointMake(x, y);
        return AXValueCreate(kAXValueCGPointType, &point);
    } @catch (NSException *e) {
        return NULL;
    }
}

// Create an AXValue containing a CGSize (for AXSize).
CFTypeRef safe_ax_value_create_cg_size(double width, double height) {
    @try {
        CGSize size = CGSizeMake(width, height);
        return AXValueCreate(kAXValueCGSizeType, &size);
    } @catch (NSException *e) {
        return NULL;
    }
}

// ── Screen Capture (ScreenCaptureKit) ────────────────────────────────────────

// Capture the primary display (or a sub-rect in logical screen points) into an
// RGBA8 buffer. The buffer is malloc'd and ownership transfers to the caller,
// who must free it via `safe_cg_free_pixels`.
//
// Uses ScreenCaptureKit's SCScreenshotManager. CGDisplayCreateImage family was
// obsoleted in macOS 15.0; SCK is the only supported path forward.
//
// `use_rect`: 0 = full display, non-zero = use rect_x/y/w/h (logical points,
//   in global screen space — same coords as Element.bounds).
// `out_pixels`: on success, points to `(*out_width) * (*out_height) * 4` bytes
//   of RGBA8888 data (R, G, B, A per pixel; row-major; tightly packed).
// `out_scale`: ratio of physical image pixels to logical input points
//   (1.0 on standard displays, 2.0 on typical Retina).
//
// Returns 0 on success; negative value on failure:
//   -1  SCShareableContent query failed / no displays
//   -2  SCScreenshotManager returned no image
//   -3  pixel buffer allocation failed
//   -4  bitmap context creation failed
//   -5  requested rect has zero / negative dimensions
//   -9999 ObjC exception
// `out_origin_x` / `out_origin_y` receive the **logical** screen coordinate
// that the returned image's pixel (0, 0) sits at: the requested rect's origin
// for a region capture, and the captured display's `frame.origin` for a full
// one. The display we capture is `content.displays[0]`, which is not
// necessarily the display at (0, 0), so a caller that assumed (0, 0) would
// place everything it maps onto these pixels off by that display's offset.
int safe_cg_capture_rgba(
    int use_rect,
    double rect_x, double rect_y, double rect_w, double rect_h,
    uint8_t **out_pixels,
    uint32_t *out_width,
    uint32_t *out_height,
    double *out_scale,
    double *out_origin_x,
    double *out_origin_y
) {
    @try {
        if (out_pixels) *out_pixels = NULL;
        if (out_width) *out_width = 0;
        if (out_height) *out_height = 0;
        if (out_scale) *out_scale = 1.0;
        if (out_origin_x) *out_origin_x = 0.0;
        if (out_origin_y) *out_origin_y = 0.0;

        if (use_rect && (rect_w <= 0.0 || rect_h <= 0.0)) {
            return -5;
        }

        // Step 1: fetch shareable content (async → sync via semaphore).
        __block SCShareableContent *content = nil;
        __block NSError *contentError = nil;
        dispatch_semaphore_t sem1 = dispatch_semaphore_create(0);
        [SCShareableContent getShareableContentWithCompletionHandler:
            ^(SCShareableContent *c, NSError *e) {
                content = c;
                contentError = e;
                dispatch_semaphore_signal(sem1);
            }];
        dispatch_semaphore_wait(sem1, DISPATCH_TIME_FOREVER);
        if (content == nil || content.displays.count == 0) {
            if (contentError) {
                NSLog(@"xa11y SCShareableContent error: %@", contentError);
            }
            return -1;
        }

        SCDisplay *display = content.displays[0];
        CGRect logical_bounds = display.frame;
        double disp_scale = (logical_bounds.size.width > 0.0)
            ? ((double)display.width / (double)logical_bounds.size.width)
            : 1.0;

        // Step 2: build the capture configuration.
        double src_x = use_rect ? rect_x : logical_bounds.origin.x;
        double src_y = use_rect ? rect_y : logical_bounds.origin.y;
        double src_w = use_rect ? rect_w : logical_bounds.size.width;
        double src_h = use_rect ? rect_h : logical_bounds.size.height;

        size_t phys_w = (size_t)(src_w * disp_scale + 0.5);
        size_t phys_h = (size_t)(src_h * disp_scale + 0.5);
        if (phys_w == 0 || phys_h == 0) return -5;

        SCContentFilter *filter = [[SCContentFilter alloc]
            initWithDisplay:display excludingWindows:@[]];
        SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
        config.width = phys_w;
        config.height = phys_h;
        config.pixelFormat = 'BGRA';   // kCVPixelFormatType_32BGRA
        config.colorSpaceName = kCGColorSpaceSRGB;
        config.showsCursor = NO;
        if (use_rect) {
            // SCK sourceRect is in the display's local (logical) coordinates.
            config.sourceRect = CGRectMake(
                src_x - logical_bounds.origin.x,
                src_y - logical_bounds.origin.y,
                src_w, src_h
            );
            config.destinationRect = CGRectMake(0, 0, (CGFloat)phys_w, (CGFloat)phys_h);
        }

        // Step 3: capture one image (async → sync via semaphore).
        __block CGImageRef image = NULL;
        __block NSError *captureError = nil;
        dispatch_semaphore_t sem2 = dispatch_semaphore_create(0);
        [SCScreenshotManager
            captureImageWithFilter:filter
                    configuration:config
                completionHandler:^(CGImageRef img, NSError *e) {
                    captureError = e;
                    if (img) {
                        image = CGImageRetain(img);
                    }
                    dispatch_semaphore_signal(sem2);
                }];
        dispatch_semaphore_wait(sem2, DISPATCH_TIME_FOREVER);
        if (image == NULL) {
            if (captureError) {
                NSLog(@"xa11y SCScreenshotManager error: %@", captureError);
            }
            return -2;
        }

        size_t w = CGImageGetWidth(image);
        size_t h = CGImageGetHeight(image);
        if (w == 0 || h == 0) {
            CGImageRelease(image);
            return -2;
        }

        // Step 4: blit into a fresh RGBA8 buffer.
        size_t row_bytes = w * 4;
        size_t buf_size = row_bytes * h;
        uint8_t *buf = (uint8_t *)malloc(buf_size);
        if (buf == NULL) {
            CGImageRelease(image);
            return -3;
        }
        memset(buf, 0, buf_size);

        CGColorSpaceRef cs = CGColorSpaceCreateDeviceRGB();
        // RGBA8888 big-endian — R, G, B, A in memory order.
        CGContextRef ctx = CGBitmapContextCreate(
            buf, w, h, 8, row_bytes, cs,
            kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big
        );
        CGColorSpaceRelease(cs);
        if (ctx == NULL) {
            free(buf);
            CGImageRelease(image);
            return -4;
        }
        CGContextDrawImage(ctx, CGRectMake(0, 0, (CGFloat)w, (CGFloat)h), image);
        CGContextRelease(ctx);
        CGImageRelease(image);

        *out_pixels = buf;
        *out_width = (uint32_t)w;
        *out_height = (uint32_t)h;
        *out_scale = disp_scale;
        // `src_x` / `src_y` are already the logical top-left of what was
        // captured: the caller's rect for a region, the display's frame origin
        // for a full capture.
        if (out_origin_x) *out_origin_x = src_x;
        if (out_origin_y) *out_origin_y = src_y;
        return 0;
    } @catch (NSException *e) {
        return -9999;
    }
}

// Release a buffer returned from `safe_cg_capture_rgba`.
void safe_cg_free_pixels(uint8_t *pixels) {
    if (pixels != NULL) {
        free(pixels);
    }
}

// ── Tables and text ranges (ax/rich.rs) ─────────────────────────────────────

// Safe wrapper for AXUIElementCopyParameterizedAttributeValue: the text range
// reads (AXStringForRange, AXBoundsForRange, ...) and AXCellForColumnAndRow.
// Returns the AX error code, or -9999 if an ObjC exception was thrown.
int safe_ax_copy_parameterized_attribute_value(
    AXUIElementRef element,
    CFStringRef attribute,
    CFTypeRef parameter,
    CFTypeRef *value
) {
    @try {
        return AXUIElementCopyParameterizedAttributeValue(element, attribute, parameter, value);
    } @catch (NSException *e) {
        *value = NULL;
        return -9999;
    }
}

// Safe wrapper for AXUIElementGetAttributeValueCount: the length of an array
// attribute (AXRows) without copying every element in it.
int safe_ax_get_attribute_value_count(
    AXUIElementRef element,
    CFStringRef attribute,
    CFIndex *count
) {
    @try {
        return AXUIElementGetAttributeValueCount(element, attribute, count);
    } @catch (NSException *e) {
        *count = 0;
        return -9999;
    }
}

// Safe wrapper for AXUIElementCopyAttributeValues: `max_values` elements of an
// array attribute from `index`, so one row of a large table is one row's work.
int safe_ax_copy_attribute_values(
    AXUIElementRef element,
    CFStringRef attribute,
    CFIndex index,
    CFIndex max_values,
    CFArrayRef *values
) {
    @try {
        return AXUIElementCopyAttributeValues(element, attribute, index, max_values, values);
    } @catch (NSException *e) {
        *values = NULL;
        return -9999;
    }
}

// Whether `value` is an AXUIElement. An app can answer an element-valued
// attribute with any CF type, and AX calls on a non-element are undefined.
bool safe_ax_is_element(CFTypeRef value) {
    @try {
        return value != NULL && CFGetTypeID(value) == AXUIElementGetTypeID();
    } @catch (NSException *e) {
        return false;
    }
}

// Read a CFRange out of an AXValue. False when `value` is not an AXValue
// holding a CFRange: `AXValueGetValue` is only defined for AXValues, and an
// app can answer a range attribute with any CF type.
bool safe_ax_value_get_range(CFTypeRef value, CFIndex *location, CFIndex *length) {
    @try {
        if (value == NULL || CFGetTypeID(value) != AXValueGetTypeID()) {
            return false;
        }
        if (AXValueGetType((AXValueRef)value) != kAXValueCFRangeType) {
            return false;
        }
        CFRange range;
        if (!AXValueGetValue((AXValueRef)value, kAXValueCFRangeType, &range)) {
            return false;
        }
        *location = range.location;
        *length = range.length;
        return true;
    } @catch (NSException *e) {
        return false;
    }
}

// Read a CGRect out of an AXValue, with the same type checks.
bool safe_ax_value_get_rect(CFTypeRef value, double *x, double *y, double *w, double *h) {
    @try {
        if (value == NULL || CFGetTypeID(value) != AXValueGetTypeID()) {
            return false;
        }
        if (AXValueGetType((AXValueRef)value) != kAXValueCGRectType) {
            return false;
        }
        CGRect rect;
        if (!AXValueGetValue((AXValueRef)value, kAXValueCGRectType, &rect)) {
            return false;
        }
        *x = rect.origin.x;
        *y = rect.origin.y;
        *w = rect.size.width;
        *h = rect.size.height;
        return true;
    } @catch (NSException *e) {
        return false;
    }
}

// Copy a CFString's UTF-16 code units. Returns the string's length in units,
// writing at most `capacity` of them to `buffer` (call with capacity 0 to
// size the buffer). -1 when `value` is not a CFString or an exception was
// thrown. Text offsets are UTF-16 units on every surface, and a decoded
// Rust `String` cannot hold the lone surrogate a range check has to see.
CFIndex safe_cf_string_copy_utf16(CFTypeRef value, uint16_t *buffer, CFIndex capacity) {
    @try {
        if (value == NULL || CFGetTypeID(value) != CFStringGetTypeID()) {
            return -1;
        }
        CFStringRef s = (CFStringRef)value;
        CFIndex length = CFStringGetLength(s);
        if (buffer != NULL && capacity > 0) {
            CFIndex n = length < capacity ? length : capacity;
            CFStringGetCharacters(s, CFRangeMake(0, n), (UniChar *)buffer);
        }
        return length;
    } @catch (NSException *e) {
        return -1;
    }
}

// The formatting of one run of an AX attributed string, decoded from the AX
// text attribute keys (AXFont, AXForegroundColor, AXUnderline, ...). Field
// conventions: -1 means not reported, booleans are 0 / 1.
typedef struct {
    // +1 retained CFString, or NULL. The caller releases it.
    CFStringRef family;
    double size;          // < 0: not reported
    int32_t weight;       // CSS-style 100..900, or -1
    int32_t italic;       // -1, 0, 1
    int32_t underline;    // -1, 0, 1
    int32_t strikethrough;// -1, 0, 1
    int32_t script;       // -2 not reported, -1 subscript, 0 baseline, 1 superscript
    int32_t misspelled;   // -1, 0, 1
    int32_t has_foreground;
    uint8_t foreground[4];
    int32_t has_background;
    uint8_t background[4];
} XA11yTextRun;

// A text colour's components, as the app set them. The AX attributed string
// carries a CGColor whose colour space does not survive the trip from the
// app (sRGB red arrives as components (1, 0, 0) labelled generic RGB, and
// converting that "back" to sRGB yields #ff2600), so the components are read
// as they are, not matched to another space. Grey is one component.
static bool xa11y_color_rgba(CFTypeRef value, uint8_t out[4]) {
    if (value == NULL || CFGetTypeID(value) != CGColorGetTypeID()) {
        return false;
    }
    CGColorRef color = (CGColorRef)value;
    size_t n = CGColorGetNumberOfComponents(color);
    const CGFloat *c = CGColorGetComponents(color);
    CGColorSpaceRef space = CGColorGetColorSpace(color);
    if (c == NULL || space == NULL) {
        return false;
    }
    double rgba[4];
    switch (CGColorSpaceGetModel(space)) {
    case kCGColorSpaceModelRGB:
        if (n != 4) {
            return false;
        }
        rgba[0] = c[0];
        rgba[1] = c[1];
        rgba[2] = c[2];
        rgba[3] = c[3];
        break;
    case kCGColorSpaceModelMonochrome:
        if (n != 2) {
            return false;
        }
        rgba[0] = rgba[1] = rgba[2] = c[0];
        rgba[3] = c[1];
        break;
    default:
        return false;
    }
    for (int i = 0; i < 4; i++) {
        double v = rgba[i] < 0 ? 0 : (rgba[i] > 1 ? 1 : rgba[i]);
        out[i] = (uint8_t)lround(v * 255.0);
    }
    return true;
}

// NSFontWeight's published constants, ultraLight .. black, against CSS
// 100 .. 900.
static const double xa11y_weight_marks[9] = {-0.8, -0.6, -0.4, 0.0, 0.23, 0.3, 0.4, 0.56, 0.62};

static int32_t xa11y_css_weight(double normalized) {
    int best = 0;
    for (int i = 1; i < 9; i++) {
        if (fabs(normalized - xa11y_weight_marks[i]) < fabs(normalized - xa11y_weight_marks[best])) {
            best = i;
        }
    }
    return (best + 1) * 100;
}

// Weight and italic of a resolved font, from its CoreText traits.
static int32_t xa11y_traits_of(CTFontRef font, int32_t *italic) {
    *italic = (CTFontGetSymbolicTraits(font) & kCTFontTraitItalic) ? 1 : 0;
    int32_t weight = -1;
    CFDictionaryRef traits = CTFontCopyTraits(font);
    if (traits != NULL) {
        CFNumberRef w = CFDictionaryGetValue(traits, kCTFontWeightTrait);
        double normalized = 0;
        if (w != NULL && CFNumberGetValue(w, kCFNumberDoubleType, &normalized)) {
            weight = xa11y_css_weight(normalized);
        }
        CFRelease(traits);
    }
    return weight;
}

// The system UI font's variants by PostScript name (".SFNS-Bold", ...), each
// with its CSS weight and italic flag. CoreText refuses to resolve these
// names directly (it substitutes Times and logs a warning), so they are
// built once from the UI font's descriptor and matched by exact name.
static NSDictionary<NSString *, NSArray<NSNumber *> *> *xa11y_ui_font_variants(void) {
    static NSDictionary *variants;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        NSMutableDictionary *m = [NSMutableDictionary new];
        // Nothing may unwind out of a dispatch_once block: the token would
        // stay "in progress" and every later call would deadlock. A failure
        // leaves the table partial, which reads as "weight not reported".
        @try {
        CTFontRef ui = CTFontCreateUIFontForLanguage(kCTFontUIFontSystem, 13.0, NULL);
        if (ui != NULL) {
            CTFontDescriptorRef base = CTFontCopyFontDescriptor(ui);
            for (int i = 0; i < 9 && base != NULL; i++) {
                for (int italic = 0; italic < 2; italic++) {
                    NSMutableDictionary *traits = [NSMutableDictionary new];
                    traits[(__bridge NSString *)kCTFontWeightTrait] = @(xa11y_weight_marks[i]);
                    if (italic) {
                        traits[(__bridge NSString *)kCTFontSymbolicTrait] = @(kCTFontTraitItalic);
                    }
                    NSDictionary *attrs = @{(__bridge NSString *)kCTFontTraitsAttribute : traits};
                    CTFontDescriptorRef d =
                        CTFontDescriptorCreateCopyWithAttributes(base, (__bridge CFDictionaryRef)attrs);
                    if (d == NULL) {
                        continue;
                    }
                    CTFontRef f = CTFontCreateWithFontDescriptor(d, 13.0, NULL);
                    CFRelease(d);
                    if (f == NULL) {
                        continue;
                    }
                    NSString *ps = CFBridgingRelease(CTFontCopyPostScriptName(f));
                    int32_t isItalic = 0;
                    int32_t weight = xa11y_traits_of(f, &isItalic);
                    CFRelease(f);
                    if (ps != nil && m[ps] == nil) {
                        m[ps] = @[ @(weight), @(isItalic) ];
                    }
                }
            }
            if (base != NULL) {
                CFRelease(base);
            }
            CFRelease(ui);
        }
        } @catch (NSException *e) {
        }
        variants = m;
    });
    return variants;
}

// The CSS weight of the font AX names, and whether it is italic. -1 when the
// name does not resolve to that exact font: CoreText substitutes a fallback
// for an unknown name, and the fallback's weight is not the run's.
static int32_t xa11y_font_traits(CFStringRef name, double size, int32_t *italic) {
    *italic = -1;
    if (name == NULL) {
        return -1;
    }
    NSString *n = (__bridge NSString *)name;
    if ([n hasPrefix:@"."]) {
        // A system UI font. AppKit names the regular and bold faces by alias
        // (".AppleSystemUIFont", ".AppleSystemUIFontBold"), which CoreText
        // resolves to the face it stands for; the other faces by PostScript
        // name (".SFNS-RegularItalic"), which only the variant table knows.
        NSArray<NSNumber *> *variant = xa11y_ui_font_variants()[n];
        if (variant != nil) {
            *italic = variant[1].intValue;
            return variant[0].intValue;
        }
        if (![n hasPrefix:@".AppleSystemUIFont"]) {
            return -1;
        }
        CTFontRef font = CTFontCreateWithName(name, size > 0 ? size : 12.0, NULL);
        if (font == NULL) {
            return -1;
        }
        NSString *resolved = CFBridgingRelease(CTFontCopyPostScriptName(font));
        int32_t weight = -1;
        if ([resolved hasPrefix:@"."]) {
            weight = xa11y_traits_of(font, italic);
        }
        CFRelease(font);
        return weight;
    }
    CTFontRef font = CTFontCreateWithName(name, size > 0 ? size : 12.0, NULL);
    if (font == NULL) {
        return -1;
    }
    int32_t weight = -1;
    CFStringRef resolved = CTFontCopyPostScriptName(font);
    if (resolved != NULL && CFStringCompare(resolved, name, 0) == kCFCompareEqualTo) {
        weight = xa11y_traits_of(font, italic);
    }
    if (resolved != NULL) {
        CFRelease(resolved);
    }
    CFRelease(font);
    return weight;
}

// Test hook for `xa11y_font_traits`: the weight / italic resolution is the
// part of run decoding that does not need a running app.
int32_t xa11y_test_font_traits(CFStringRef name, double size, int32_t *italic) {
    @try {
        return xa11y_font_traits(name, size, italic);
    } @catch (NSException *e) {
        *italic = -1;
        return -1;
    }
}

static int32_t xa11y_flag(NSDictionary *attrs, NSString *key, bool styled) {
    id v = attrs[key];
    if (v == nil) {
        // AX lists only the attributes applied to a run. Once the run reports
        // its styling at all (it has a font), a missing decoration is off.
        return styled ? 0 : -1;
    }
    if ([v isKindOfClass:[NSNumber class]]) {
        return [(NSNumber *)v intValue] != 0 ? 1 : 0;
    }
    return -1;
}

// Decode the attributes at index 0 of `attributed`, the answer to
// AXAttributedStringForRange. Returns 0 on success, 1 when the value is not
// an attributed string, -9999 on an exception.
int safe_ax_decode_text_run(CFTypeRef attributed, XA11yTextRun *out) {
    out->family = NULL;
    out->size = -1;
    out->weight = -1;
    out->italic = -1;
    out->underline = -1;
    out->strikethrough = -1;
    out->script = -2;
    out->misspelled = -1;
    out->has_foreground = 0;
    out->has_background = 0;
    @try {
        if (attributed == NULL || CFGetTypeID(attributed) != CFAttributedStringGetTypeID()) {
            return 1;
        }
        NSAttributedString *s = (__bridge NSAttributedString *)attributed;
        if (s.length == 0) {
            return 0;
        }
        NSDictionary *attrs = [s attributesAtIndex:0 effectiveRange:NULL];
        id font = attrs[@"AXFont"];
        bool styled = font != nil;
        if ([font isKindOfClass:[NSDictionary class]]) {
            NSDictionary *f = font;
            id family = f[@"AXFontFamily"];
            if ([family isKindOfClass:[NSString class]]) {
                out->family = (CFStringRef)CFBridgingRetain(family);
            }
            id size = f[@"AXFontSize"];
            if ([size isKindOfClass:[NSNumber class]]) {
                out->size = [(NSNumber *)size doubleValue];
            }
            id name = f[@"AXFontName"];
            if ([name isKindOfClass:[NSString class]]) {
                int32_t italic = -1;
                out->weight = xa11y_font_traits((__bridge CFStringRef)name, out->size, &italic);
                out->italic = italic;
            }
        }
        out->underline = xa11y_flag(attrs, @"AXUnderline", styled);
        out->strikethrough = xa11y_flag(attrs, @"AXStrikethrough", styled);
        int32_t misspelled = xa11y_flag(attrs, @"AXMisspelled", styled);
        int32_t marked = xa11y_flag(attrs, @"AXMarkedMisspelled", styled);
        out->misspelled = (misspelled == 1 || marked == 1) ? 1 : misspelled;
        id script = attrs[@"AXSuperscript"];
        if ([script isKindOfClass:[NSNumber class]]) {
            int v = [(NSNumber *)script intValue];
            out->script = v > 0 ? 1 : (v < 0 ? -1 : 0);
        } else if (script == nil && styled) {
            out->script = 0;
        }
        if (xa11y_color_rgba((__bridge CFTypeRef)attrs[@"AXForegroundColor"], out->foreground)) {
            out->has_foreground = 1;
        }
        if (xa11y_color_rgba((__bridge CFTypeRef)attrs[@"AXBackgroundColor"], out->background)) {
            out->has_background = 1;
        }
        return 0;
    } @catch (NSException *e) {
        if (out->family != NULL) {
            CFRelease(out->family);
            out->family = NULL;
        }
        return -9999;
    }
}

// ── Test Helpers ─────────────────────────────────────────────────────────────

// Throw an NSException (for testing that our Rust code handles it properly).
// Returns 0 if no exception was caught (should not happen), 1 if caught.
int test_throw_and_catch_nsexception(void) {
    @try {
        @throw [NSException exceptionWithName:@"TestException"
                                       reason:@"deliberate test throw"
                                     userInfo:nil];
        return 0;
    } @catch (NSException *e) {
        return 1;
    }
}
