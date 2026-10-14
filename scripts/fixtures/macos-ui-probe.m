// Read-only, disposable-client surface inventory. Build as an ad-hoc dylib
// and load as a legacy fixture plugin; never ship as a WebView backend.
// PROBE_RESULT must name a file in the run-owned private scratch directory.
#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>
#import <mach-o/dyld.h>
#import <stdint.h>

#ifndef PROBE_RESULT
#error "Define PROBE_RESULT as the run-owned result path"
#endif

static void visit(NSView *view, NSMutableDictionary *classes,
                  NSUInteger *visited, NSUInteger depth) {
    if (!view || *visited >= 5000 || depth >= 64) return;
    (*visited)++;
    NSString *name = NSStringFromClass(view.class);
    classes[name] = @([classes[name] unsignedIntegerValue] + 1);
    for (NSView *child in view.subviews) visit(child, classes, visited, depth + 1);
}

static void inventory(NSUInteger attempt) {
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC),
                   dispatch_get_main_queue(), ^{
        @autoreleasepool {
            NSMutableDictionary *classes = [NSMutableDictionary dictionary];
            NSUInteger visited = 0;
            NSArray *windows = NSApp.windows;
            for (NSWindow *window in windows) visit(window.contentView, classes, &visited, 0);
            // Startup/self-exec may precede window creation. Observe a bounded
            // later main-queue turn instead of treating the early empty set as UI.
            if (visited == 0 && attempt < 15) {
                inventory(attempt + 1);
                return;
            }
            NSMutableArray *webImages = [NSMutableArray array];
            uint32_t imageCount = _dyld_image_count();
            for (uint32_t index = 0; index < imageCount && index < 4096; index++) {
                const char *name = _dyld_get_image_name(index);
                if (name && (strstr(name, "WebKit") || strstr(name, "JavaScriptCore")))
                    [webImages addObject:@(name)];
            }
            NSDictionary *result = @{
                @"main_thread": @([NSThread isMainThread]),
                @"windows": @(windows.count), @"views": @(visited),
                @"attempt": @(attempt), @"image_scan_truncated": @(imageCount > 4096),
                @"classes": classes, @"web_images": webImages,
                @"scope": @"current signed-out client windows only; no URL or DOM inferred"
            };
            NSData *data = [NSJSONSerialization dataWithJSONObject:result
                options:NSJSONWritingPrettyPrinted error:nil];
            [data writeToFile:@PROBE_RESULT options:NSDataWritingAtomic error:nil];
        }
    });
}

uint32_t ParsecPluginInit(void *unused) {
    (void)unused;
    inventory(1);
    return 0;
}
