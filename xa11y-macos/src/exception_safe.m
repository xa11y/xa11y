// Objective-C exception safety wrappers for AXUIElement API calls.
//
// These functions wrap macOS accessibility API calls in @try/@catch blocks
// so that ObjC exceptions (NSException) are caught at the C level and never
// unwind through Rust frames. This is necessary because Rust's stable ABI
// aborts on foreign exceptions in extern "C" functions.

#import <ApplicationServices/ApplicationServices.h>
#import <Foundation/Foundation.h>

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
