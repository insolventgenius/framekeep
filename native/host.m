#import <AppKit/AppKit.h>
#import <CoreImage/CoreImage.h>
#import <ImageIO/ImageIO.h>
#import <WebKit/WebKit.h>
#import <sqlite3.h>
#import <errno.h>
#import <stdio.h>
#import <time.h>
#import <stdint.h>
#import <stdbool.h>

extern void framekeep_init(const char *legacy_preferences);
extern char *framekeep_dispatch(const char *command);
extern char *framekeep_path(uint32_t generation, size_t id, const char *format);
extern void framekeep_free_string(char *string);
extern uint8_t *framekeep_raf_jpeg(const char *path, size_t *length);
extern void framekeep_free_bytes(uint8_t *bytes, size_t length);
extern const float *framekeep_lut_cube(const char *grade, size_t *dimension, bool *log2);
extern CGImageRef FramekeepCreateIcon(NSURL *url) CF_RETURNS_RETAINED;

static NSURL *FileURL(const char *path, BOOL directory) {
    return [NSURL fileURLWithFileSystemRepresentation:path isDirectory:directory relativeToURL:nil];
}

int framekeep_sqlite_bind_text(sqlite3_stmt *statement, int index, const char *text, int length) {
    return sqlite3_bind_text(statement, index, text, length, SQLITE_TRANSIENT);
}

int framekeep_rename_exclusive(const char *source, const char *output, bool *copyNeeded) {
    if (renamex_np(source, output, RENAME_EXCL) == 0) {
        *copyNeeded = false;
        return 0;
    }
    int error = errno;
    // Darwin's ENOTSUP (45) differs from EOPNOTSUPP and Rust does not classify it as Unsupported.
    *copyNeeded = error == ENOTSUP || error == EOPNOTSUPP || error == ENOSYS;
    return error;
}

bool framekeep_export_date(char output[11]) {
    time_t now = time(NULL);
    struct tm local;
    return localtime_r(&now, &local) && strftime(output, 11, "%m-%d-%Y", &local) == 10;
}

char *framekeep_trash(const char *path, char **destination) {
    @autoreleasepool {
        NSError *error = nil;
        NSURL *trashed = nil;
        if (![NSFileManager.defaultManager trashItemAtURL:FileURL(path, NO) resultingItemURL:&trashed error:&error]) {
            return strdup(error.localizedDescription.UTF8String ?: "Could not move the file to Trash.");
        }
        *destination = trashed ? strdup(trashed.fileSystemRepresentation) : NULL;
        return NULL;
    }
}

char *framekeep_restore(const char *path, const char *destination) {
    @autoreleasepool {
        NSError *error = nil;
        if (![NSFileManager.defaultManager moveItemAtURL:FileURL(path, NO) toURL:FileURL(destination, NO) error:&error]) {
            return strdup(error.localizedDescription.UTF8String ?: "Could not restore the file from Trash.");
        }
        return NULL;
    }
}

static NSURL *CatalogURL(NSNumber *generation, NSNumber *identifier, NSString *format) {
    char *path = framekeep_path(generation.unsignedIntValue, identifier.unsignedLongLongValue, format.UTF8String);
    if (!path) return nil;
    NSURL *url = FileURL(path, NO);
    framekeep_free_string(path);
    return url;
}

static CGImageSourceRef ImageSource(NSURL *url) {
    BOOL heif = [url.pathExtension caseInsensitiveCompare:@"hif"] == NSOrderedSame;
    NSDictionary *options = heif ? @{(__bridge NSString *)kCGImageSourceTypeIdentifierHint: @"public.heic"} : nil;
    return CGImageSourceCreateWithURL((__bridge CFURLRef)url, (__bridge CFDictionaryRef)options);
}

static CGImageRef Thumbnail(CGImageSourceRef source, NSUInteger edge) {
    if (!source) return NULL;
    CGImageRef result = CGImageSourceCreateThumbnailAtIndex(source, CGImageSourceGetPrimaryImageIndex(source), (__bridge CFDictionaryRef)@{
        (__bridge NSString *)kCGImageSourceCreateThumbnailFromImageAlways: @YES,
        (__bridge NSString *)kCGImageSourceCreateThumbnailWithTransform: @YES,
        (__bridge NSString *)kCGImageSourceThumbnailMaxPixelSize: @(edge),
    });
    CFRelease(source);
    return result;
}

static CGImageRef EmbeddedRAF(NSURL *url, NSUInteger edge) {
    size_t length = 0;
    uint8_t *data = framekeep_raf_jpeg(url.fileSystemRepresentation, &length);
    if (!data) return NULL;
    NSData *jpeg = [[NSData alloc] initWithBytesNoCopy:data length:length deallocator:^(void *bytes, NSUInteger size) {
        framekeep_free_bytes(bytes, size);
    }];
    return Thumbnail(CGImageSourceCreateWithData((__bridge CFDataRef)jpeg, NULL), edge);
}

static CIContext *SoftwareImageContext(void) {
    static CIContext *context;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        context = [CIContext contextWithOptions:@{kCIContextUseSoftwareRenderer: @YES, kCIContextCacheIntermediates: @NO}];
    });
    return context;
}

static CIContext *ImageContext(void) {
    static CIContext *context;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        context = [CIContext contextWithOptions:@{kCIContextCacheIntermediates: @NO}];
    });
    return context;
}

static CGImageRef RenderImage(CIImage *image) {
    if (!image || CGRectIsEmpty(image.extent) || CGRectIsInfinite(image.extent)) return NULL;
    CGColorSpaceRef color = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGImageRef result = [ImageContext() createCGImage:image fromRect:image.extent format:kCIFormatRGBA8 colorSpace:color];
    if (!result) {
        result = [SoftwareImageContext() createCGImage:image fromRect:image.extent format:kCIFormatRGBA8 colorSpace:color];
    }
    CGColorSpaceRelease(color);
    return result;
}

static CIImage *RAWImage(NSURL *url, NSUInteger edge, BOOL linear) {
    CIRAWFilter *filter = [CIRAWFilter filterWithImageURL:url];
    CGSize native = filter.nativeSize;
    if (MAX(native.width, native.height) <= 0) return nil;
    filter.scaleFactor = MIN(1., edge / MAX(native.width, native.height));
    if (linear) {
        filter.baselineExposure = 0;
        filter.boostAmount = 0;
        if (filter.isLocalToneMapSupported) filter.localToneMapAmount = 0;
        filter.gamutMappingEnabled = NO;
        filter.extendedDynamicRangeAmount = 1;
    }
    return filter.outputImage;
}

static double EditValue(NSDictionary *edits, NSString *key, double limit) {
    NSNumber *number = edits[key];
    return [number isKindOfClass:NSNumber.class] ? MAX(-limit, MIN(limit, number.doubleValue)) : 0;
}

static NSString *FujifilmGrade(NSDictionary *edits) {
    id grade = edits[@"grade"];
    return [grade isKindOfClass:NSString.class] && [grade hasPrefix:@"fuji_"] &&
           EditValue(edits, @"strength", 100) > 0 ? grade : nil;
}

static CIImage *ApplyFujifilmLUT(CIImage *linear, const float *values, size_t size, BOOL log2) {
    static CIColorKernel *kernel;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        // Core Image's extended linear sRGB uses Rec.709 primaries. Convert to
        // Fujifilm F-Gamut (Rec.2020) and encode with Fujifilm's published curves.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        kernel = [CIColorKernel kernelWithString:
            @"kernel vec4 fujifilmLog(__sample pixel, float fLog2) {"
             "  vec3 rgb = unpremultiply(pixel).rgb;"
             "  vec3 gamut = max(vec3(dot(rgb, vec3(0.627404, 0.329283, 0.043313)),"
             "                        dot(rgb, vec3(0.069097, 0.919540, 0.011362)),"
             "                        dot(rgb, vec3(0.016391, 0.088013, 0.895595))), vec3(0.0));"
             "  float a = mix(0.555556, 5.555556, fLog2);"
             "  float b = mix(0.009468, 0.064829, fLog2);"
             "  float c = mix(0.344676, 0.245281, fLog2);"
             "  float d = mix(0.790453, 0.384316, fLog2);"
             "  float e = mix(8.735631, 8.799461, fLog2);"
             "  float cut = mix(0.000890, 0.000889, fLog2);"
             "  vec3 toe = e * gamut + 0.092864;"
             "  vec3 upper = c * log(gamut * a + b) * 0.4342944819 + d;"
             "  vec3 logRGB = clamp(mix(toe, upper, step(vec3(cut), gamut)), 0.0, 1.0);"
             "  return premultiply(vec4(logRGB, pixel.a));"
             "}"];
#pragma clang diagnostic pop
    });
    CIImage *encoded = [kernel applyWithExtent:linear.extent arguments:@[linear, @(log2)]];
    if (!encoded) return nil;
    NSData *cube = [NSData dataWithBytesNoCopy:(void *)values
        length:size * size * size * 4 * sizeof(float) freeWhenDone:NO];
    return [encoded imageByApplyingFilter:@"CIColorCube" withInputParameters:@{
        @"inputCubeDimension": @(size), @"inputCubeData": cube,
    }];
}

static CIImage *ApplyEdits(CIImage *image, CIImage *linear, NSDictionary *edits, BOOL raw) {
    if (!image || edits.count == 0) return image;
    double exposure = EditValue(edits, @"exposure", 200) / 100;
    double contrast = EditValue(edits, @"contrast", 100) / 100;
    double saturation = EditValue(edits, @"saturation", 100) / 100;
    double warmth = EditValue(edits, @"warmth", 100) / 100;
    double highlight = EditValue(edits, @"highlights", 100) / 100;
    double shadow = EditValue(edits, @"shadows", 100) / 100;
    if (exposure) {
        NSDictionary *exposureSettings = @{@"inputEV": @(exposure)};
        image = [image imageByApplyingFilter:@"CIExposureAdjust" withInputParameters:exposureSettings];
        linear = [linear imageByApplyingFilter:@"CIExposureAdjust" withInputParameters:exposureSettings];
    }

    double strength = MAX(0, EditValue(edits, @"strength", 100)) / 100;
    NSString *grade = raw && [edits[@"grade"] isKindOfClass:NSString.class] ? edits[@"grade"] : nil;
    if ([grade isEqualToString:@"vivid"]) {
        contrast += 0.40 * strength;
        saturation += 0.34 * strength;
        highlight -= 0.16 * strength;
    } else if ([grade isEqualToString:@"warm_film"]) {
        warmth += 0.55 * strength;
        saturation -= 0.20 * strength;
        shadow += 0.30 * strength;
        highlight -= 0.28 * strength;
    } else if ([grade isEqualToString:@"cool_fade"]) {
        warmth -= 0.58 * strength;
        saturation -= 0.36 * strength;
        contrast -= 0.28 * strength;
        shadow += 0.35 * strength;
    } else if ([grade isEqualToString:@"monochrome"]) {
        saturation = (1 + saturation) * (1 - strength) - 1;
        contrast += 0.25 * strength;
    }
    NSString *fujifilm = raw ? FujifilmGrade(edits) : nil;
    if (fujifilm) {
        size_t dimension = 0;
        bool log2 = false;
        const float *values = framekeep_lut_cube(fujifilm.UTF8String, &dimension, &log2);
        if (values) {
            CIImage *graded = ApplyFujifilmLUT(linear ?: image, values, dimension, log2);
            if (!graded) return nil;
            image = strength == 1 ? graded : [graded imageByApplyingFilter:@"CIMix" withInputParameters:@{
                kCIInputBackgroundImageKey: image, @"inputAmount": @(strength),
            }];
        }
    }
    if (warmth) {
        image = [image imageByApplyingFilter:@"CITemperatureAndTint" withInputParameters:@{
            @"inputNeutral": [CIVector vectorWithX:6500 Y:0],
            @"inputTargetNeutral": [CIVector vectorWithX:6500 + 2200 * warmth Y:0],
        }];
    }
    if (contrast || saturation) {
        image = [image imageByApplyingFilter:@"CIColorControls" withInputParameters:@{
            kCIInputContrastKey: @(MAX(0.45, 1 + 0.45 * contrast)),
            kCIInputSaturationKey: @(MAX(0, 1 + saturation)),
        }];
    }
    if (shadow || highlight) {
        double low = 0.25 + 0.15 * MAX(-1, MIN(1, shadow));
        double high = 0.75 + 0.15 * MAX(-1, MIN(1, highlight));
        image = [image imageByApplyingFilter:@"CIToneCurve" withInputParameters:@{
            @"inputPoint0": [CIVector vectorWithX:0 Y:0],
            @"inputPoint1": [CIVector vectorWithX:0.25 Y:low],
            @"inputPoint2": [CIVector vectorWithX:0.50 Y:0.50],
            @"inputPoint3": [CIVector vectorWithX:0.75 Y:high],
            @"inputPoint4": [CIVector vectorWithX:1 Y:1],
        }];
    }
    static const int orientations[] = {1, 6, 3, 8};
    int rotation = (int)EditValue(edits, @"rotation", 3);
    if (rotation > 0) image = [image imageByApplyingOrientation:orientations[rotation]];
    return image;
}

static NSDictionary *RequestEdits(NSURL *url) {
    for (NSURLQueryItem *item in [NSURLComponents componentsWithURL:url resolvingAgainstBaseURL:NO].queryItems) {
        if (![item.name isEqualToString:@"edits"] || item.value.length > 1024) continue;
        NSData *data = [item.value dataUsingEncoding:NSUTF8StringEncoding];
        if (!data) return nil;
        id edits = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
        return [edits isKindOfClass:NSDictionary.class] ? edits : nil;
    }
    return nil;
}

@interface FKDecodedImage : NSObject
@property (nonatomic, readonly) CGImageRef image;
- (instancetype)initWithOwnedImage:(CGImageRef)image;
@end

@implementation FKDecodedImage
- (instancetype)initWithOwnedImage:(CGImageRef)image {
    if ((self = [super init])) _image = image;
    return self;
}
- (void)dealloc { CGImageRelease(_image); }
@end

@interface FKPreview : NSObject
@property NSData *data;
@property NSString *mime;
@end
@implementation FKPreview
@end

static NSData *EncodeImage(CGImageRef image, BOOL png, double quality) {
    if (!image) return nil;
    NSMutableData *buffer = NSMutableData.data;
    CGImageDestinationRef encoder = CGImageDestinationCreateWithData((__bridge CFMutableDataRef)buffer,
        png ? CFSTR("public.png") : CFSTR("public.jpeg"), 1, NULL);
    if (!encoder) return nil;
    NSDictionary *options = png ? @{} :
        @{(__bridge NSString *)kCGImageDestinationLossyCompressionQuality: @(quality)};
    CGImageDestinationAddImage(encoder, image, (__bridge CFDictionaryRef)options);
    BOOL success = CGImageDestinationFinalize(encoder);
    CFRelease(encoder);
    return success ? buffer : nil;
}

static FKDecodedImage *DecodePreview(NSURL *url, BOOL raw, BOOL thumbnail) {
    NSUInteger edge = thumbnail ? 360 : 2400;
    CGImageRef image = raw && !thumbnail ? RenderImage(RAWImage(url, edge, NO)) : NULL;
    if (raw && !image) image = EmbeddedRAF(url, edge);
    if (!image) image = Thumbnail(ImageSource(url), edge);
    return image ? [[FKDecodedImage alloc] initWithOwnedImage:image] : nil;
}

static FKPreview *MakePreview(FKDecodedImage *decoded, NSURL *source, BOOL raw, BOOL thumbnail, NSDictionary *edits) {
    CGImageRef original = decoded.image;
    CGImageRef image = original;
    if (edits.count) {
        CIImage *linear = raw && FujifilmGrade(edits) ? RAWImage(source, thumbnail ? 360 : 2400, YES) : nil;
        image = RenderImage(ApplyEdits([CIImage imageWithCGImage:original], linear, edits, raw));
    }
    CGImageAlphaInfo alpha = CGImageGetAlphaInfo(original);
    BOOL transparent = !raw && (alpha == kCGImageAlphaPremultipliedFirst || alpha == kCGImageAlphaPremultipliedLast
                                || alpha == kCGImageAlphaFirst || alpha == kCGImageAlphaLast);
    NSData *data = EncodeImage(image, transparent, thumbnail ? 0.82 : 0.94);
    if (image && image != original) CGImageRelease(image);
    if (!data) return nil;
    FKPreview *preview = [FKPreview new];
    preview.data = data;
    preview.mime = transparent ? @"image/png" : @"image/jpeg";
    return preview;
}

static NSData *ExportImage(NSURL *url, BOOL raw, BOOL jpeg, NSDictionary *edits) {
    CIImage *image = nil;
    if (raw) {
        image = RAWImage(url, NSUIntegerMax, NO);
        if (!image) {
            CGImageRef decoded = EmbeddedRAF(url, NSUIntegerMax);
            if (decoded) {
                image = [CIImage imageWithCGImage:decoded];
                CGImageRelease(decoded);
            }
        }
    } else {
        CGImageSourceRef source = ImageSource(url);
        if (source) {
            image = [CIImage imageWithCGImageSource:source index:CGImageSourceGetPrimaryImageIndex(source)
                options:@{kCIImageApplyOrientationProperty: @YES}];
            CFRelease(source);
        }
    }
    CIImage *linear = raw && FujifilmGrade(edits) ? RAWImage(url, NSUIntegerMax, YES) : nil;
    image = ApplyEdits(image, linear, edits, raw);
    if (jpeg && image) {
        CIImage *white = [[CIImage imageWithColor:[CIColor colorWithRed:1 green:1 blue:1]] imageByCroppingToRect:image.extent];
        image = [image imageByCompositingOverImage:white];
    }
    CGImageRef rendered = RenderImage(image);
    NSData *data = EncodeImage(rendered, !jpeg, 0.95);
    if (rendered) CGImageRelease(rendered);
    return data;
}

char *framekeep_render_image(const char *path, bool raw, bool jpeg, const char *json, const char *output) {
    @autoreleasepool {
        NSData *encoded = [NSData dataWithBytes:json length:strlen(json)];
        NSDictionary *edits = [NSJSONSerialization JSONObjectWithData:encoded options:0 error:nil];
        NSString *grade = raw ? FujifilmGrade(edits) : nil;
        size_t dimension = 0;
        bool log2 = false;
        if (grade && !framekeep_lut_cube(grade.UTF8String, &dimension, &log2)) {
            return strdup("The selected Fujifilm LUT is missing. Restore the LUT files or choose a different grade.");
        }
        NSData *image = ExportImage(FileURL(path, NO), raw, jpeg, edits);
        if (!image) return strdup("This photo could not be decoded or rendered.");
        NSError *error = nil;
        if (![image writeToURL:FileURL(output, NO) options:NSDataWritingAtomic error:&error]) {
            return strdup(error.localizedDescription.UTF8String ?: "Could not write the image copy.");
        }
        return NULL;
    }
}

@interface FKScheme : NSObject <WKURLSchemeHandler>
@end

@implementation FKScheme {
    NSMapTable<id<WKURLSchemeTask>, NSOperation *> *_active;
    NSOperationQueue *_large;
    NSOperationQueue *_small;
    NSCache<NSString *, FKPreview *> *_cache;
    NSCache<NSString *, FKDecodedImage *> *_decoded;
}

- (instancetype)init {
    if ((self = [super init])) {
        _active = [NSMapTable strongToStrongObjectsMapTable];
        _large = [NSOperationQueue new];
        _large.maxConcurrentOperationCount = 2;
        _large.qualityOfService = NSQualityOfServiceUserInitiated;
        _small = [NSOperationQueue new];
        _small.maxConcurrentOperationCount = 3;
        _small.qualityOfService = NSQualityOfServiceUtility;
        _cache = [NSCache new];
        _cache.totalCostLimit = 48 * 1024 * 1024;
        _decoded = [NSCache new];
        _decoded.totalCostLimit = 90 * 1024 * 1024;
    }
    return self;
}

- (void)webView:(WKWebView *)view startURLSchemeTask:(id<WKURLSchemeTask>)task {
    NSURL *request = task.request.URL;
    NSArray<NSString *> *parts = request.pathComponents;
    if (![request.host isEqualToString:@"library"] || parts.count != 5 ||
        ![@[@"png", @"raf", @"hif", @"jpg"] containsObject:parts[3]] || ![@[@"small", @"large"] containsObject:parts[4]]) {
        [task didFailWithError:[NSError errorWithDomain:NSURLErrorDomain code:NSURLErrorBadURL userInfo:nil]];
        return;
    }

    BOOL small = [parts[4] isEqualToString:@"small"];
    NSString *key = request.absoluteString;
    NSBlockOperation *operation = [NSBlockOperation new];
    __weak NSBlockOperation *weakOperation = operation;
    [operation addExecutionBlock:^{
        @autoreleasepool {
            if (weakOperation.isCancelled) return;
            FKPreview *preview = [self->_cache objectForKey:key];
            if (!preview) {
                NSURL *source = CatalogURL(@(parts[1].longLongValue), @(parts[2].longLongValue), parts[3]);
                BOOL raw = [parts[3] isEqualToString:@"raf"];
                if (source && !weakOperation.isCancelled) {
                    NSString *decodeKey = request.path;
                    FKDecodedImage *decoded = [self->_decoded objectForKey:decodeKey];
                    if (!decoded) {
                        decoded = DecodePreview(source, raw, small);
                        if (decoded) [self->_decoded setObject:decoded forKey:decodeKey
                            cost:CGImageGetWidth(decoded.image) * CGImageGetHeight(decoded.image) * 4];
                    }
                    if (decoded && !weakOperation.isCancelled) {
                        preview = MakePreview(decoded, source, raw, small, RequestEdits(request));
                        if (preview) [self->_cache setObject:preview forKey:key cost:preview.data.length];
                    }
                }
            }
            dispatch_async(dispatch_get_main_queue(), ^{
                if (![self->_active objectForKey:task]) return;
                [self->_active removeObjectForKey:task];
                if (!preview) {
                    [task didFailWithError:[NSError errorWithDomain:NSURLErrorDomain code:NSURLErrorCannotDecodeContentData userInfo:nil]];
                    return;
                }
                [task didReceiveResponse:[[NSURLResponse alloc] initWithURL:request MIMEType:preview.mime
                    expectedContentLength:preview.data.length textEncodingName:nil]];
                [task didReceiveData:preview.data];
                [task didFinish];
            });
        }
    }];
    [_active setObject:operation forKey:task];
    [(small ? _small : _large) addOperation:operation];
}

- (void)webView:(WKWebView *)view stopURLSchemeTask:(id<WKURLSchemeTask>)task {
    [[_active objectForKey:task] cancel];
    [_active removeObjectForKey:task];
}
@end

@interface FKHost : NSObject <NSApplicationDelegate, WKScriptMessageHandlerWithReply, NSDraggingSource>
@property NSURL *ui;
@property NSString *initialFolder;
@property NSWindow *window;
@property WKWebView *web;
@property dispatch_queue_t worker;
@property id monitor;
@property NSDictionary *dragTarget;
@property NSDictionary *pressedTarget;
@property WKNavigation *navigation;
@end

void framekeep_raw_trash_progress(size_t processed, size_t total) {
    dispatch_async(dispatch_get_main_queue(), ^{
        FKHost *host = (FKHost *)NSApp.delegate;
        if (![host isKindOfClass:FKHost.class]) return;
        NSString *script = [NSString stringWithFormat:@"window.framekeepRawTrashProgress?.(%zu,%zu)", processed, total];
        [host.web evaluateJavaScript:script completionHandler:nil];
    });
}

@implementation FKHost

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    NSAppearance *appearance = [NSAppearance appearanceNamed:NSAppearanceNameDarkAqua];
    NSApp.appearance = appearance;
    NSApp.activationPolicy = NSApplicationActivationPolicyRegular;

    NSBundle *bundle = NSBundle.mainBundle;
    NSString *iconName = [bundle objectForInfoDictionaryKey:@"CFBundleIconFile"];
    NSURL *iconURL = iconName.length ? [bundle URLForResource:iconName withExtension:nil] : nil;
    NSImage *icon = iconURL ? [[NSImage alloc] initWithContentsOfURL:iconURL] : nil;
    if (!icon) {
        NSURL *source = [self.ui.URLByDeletingLastPathComponent URLByAppendingPathComponent:@"Small Robot.png"];
        CGImageRef rendered = FramekeepCreateIcon(source);
        if (rendered) {
            icon = [[NSImage alloc] initWithCGImage:rendered size:NSZeroSize];
            CGImageRelease(rendered);
        }
    }
    if (icon) NSApp.applicationIconImage = icon;

    [self makeMenus];
    self.worker = dispatch_queue_create("app.framekeep.library", DISPATCH_QUEUE_SERIAL);

    WKWebViewConfiguration *configuration = [WKWebViewConfiguration new];
    [configuration setURLSchemeHandler:[FKScheme new] forURLScheme:@"framekeep-preview"];
    [configuration.userContentController addScriptMessageHandlerWithReply:self
        contentWorld:WKContentWorld.pageWorld name:@"framekeep"];
    self.web = [[WKWebView alloc] initWithFrame:NSZeroRect configuration:configuration];
    if (@available(macOS 13.3, *)) {
        self.web.inspectable = NSProcessInfo.processInfo.environment[@"FRAMEKEEP_INSPECT"] != nil;
    }
    self.web.underPageBackgroundColor = [NSColor colorWithWhite:0.04 alpha:1];
    self.web.allowsBackForwardNavigationGestures = NO;

    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 1310, 858)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskMiniaturizable |
                  NSWindowStyleMaskResizable | NSWindowStyleMaskFullSizeContentView
        backing:NSBackingStoreBuffered defer:NO];
    self.window.title = @"Framekeep";
    self.window.titleVisibility = NSWindowTitleHidden;
    self.window.titlebarAppearsTransparent = YES;
    self.window.backgroundColor = self.web.underPageBackgroundColor;
    self.window.minSize = NSMakeSize(760, 590);
    self.window.contentView = self.web;
    [self.window center];
    [self.window setFrameAutosaveName:@"Framekeep.Main"];
    [self.window makeKeyAndOrderFront:nil];
    [self.window makeFirstResponder:self.web];
    [NSApp activateIgnoringOtherApps:YES];

    __weak FKHost *weakSelf = self;
    self.monitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskLeftMouseDown |
        NSEventMaskLeftMouseDragged | NSEventMaskLeftMouseUp handler:^NSEvent *(NSEvent *event) {
            return [weakSelf mouse:event];
        }];
    self.navigation = [self.web loadFileURL:[self.ui URLByAppendingPathComponent:@"index.html"]
                    allowingReadAccessToURL:self.ui];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)sender { return YES; }

- (void)applicationWillTerminate:(NSNotification *)notification {
    if (self.monitor) [NSEvent removeMonitor:self.monitor];
    [self.web.configuration.userContentController removeScriptMessageHandlerForName:@"framekeep" contentWorld:WKContentWorld.pageWorld];
}

- (void)makeMenus {
    NSMenu *main = [NSMenu new];
    NSMenuItem *appItem = [NSMenuItem new];
    [main addItem:appItem];
    NSMenu *app = [[NSMenu alloc] initWithTitle:@"Framekeep"];
    appItem.submenu = app;
    [app addItemWithTitle:@"Quit Framekeep" action:@selector(terminate:) keyEquivalent:@"q"];
    NSMenuItem *fileItem = [NSMenuItem new];
    [main addItem:fileItem];
    NSMenu *file = [[NSMenu alloc] initWithTitle:@"File"];
    fileItem.submenu = file;
    [file addItemWithTitle:@"Open Folder…" action:@selector(openFromMenu:) keyEquivalent:@"o"].target = self;
    [file addItemWithTitle:@"Rescan Folder" action:@selector(refreshFromMenu:) keyEquivalent:@"r"].target = self;
    [file addItem:NSMenuItem.separatorItem];
    [file addItemWithTitle:@"Undo" action:@selector(undoFromMenu:) keyEquivalent:@"z"].target = self;
    [file addItemWithTitle:@"Close Window" action:@selector(performClose:) keyEquivalent:@"w"];
    NSApp.mainMenu = main;
}

- (void)openFromMenu:(id)sender { [self.web evaluateJavaScript:@"window.framekeepMenu('open')" completionHandler:nil]; }
- (void)refreshFromMenu:(id)sender { [self.web evaluateJavaScript:@"window.framekeepMenu('refresh')" completionHandler:nil]; }
- (void)undoFromMenu:(id)sender { [self.web evaluateJavaScript:@"window.framekeepMenu('undo')" completionHandler:nil]; }

- (void)dispatchCommand:(NSDictionary *)command reply:(void (^)(id, NSString *))reply {
    NSData *encoded = [NSJSONSerialization dataWithJSONObject:command options:0 error:nil];
    if (!encoded) {
        reply(nil, @"Invalid library command.");
        return;
    }
    NSString *json = [[NSString alloc] initWithData:encoded encoding:NSUTF8StringEncoding];
    dispatch_async(self.worker, ^{
        @autoreleasepool {
            char *result = framekeep_dispatch(json.UTF8String);
            NSData *data = [NSData dataWithBytes:result length:strlen(result)];
            framekeep_free_string(result);
            NSDictionary *response = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
            dispatch_async(dispatch_get_main_queue(), ^{
                if ([response[@"ok"] boolValue]) {
                    reply(response[@"value"], nil);
                } else {
                    reply(nil, response[@"error"] ?: @"The library did not respond.");
                }
            });
        }
    });
}

- (void)userContentController:(WKUserContentController *)controller didReceiveScriptMessage:(WKScriptMessage *)message
                 replyHandler:(void (^)(id, NSString *))reply {
    if (!message.frameInfo.isMainFrame || ![message.body isKindOfClass:NSDictionary.class]) {
        reply(nil, @"Invalid request.");
        return;
    }
    NSDictionary *command = message.body;
    NSString *op = command[@"op"];
    if ([op isEqualToString:@"dragTarget"]) {
        self.dragTarget = [command[@"rect"] isKindOfClass:NSDictionary.class] ? command : nil;
        reply(@YES, nil);
    } else if ([op isEqualToString:@"pick"]) {
        NSOpenPanel *panel = [NSOpenPanel openPanel];
        panel.canChooseFiles = NO;
        panel.canChooseDirectories = YES;
        panel.showsHiddenFiles = YES;
        panel.canCreateDirectories = NO;
        panel.allowsMultipleSelection = NO;
        panel.prompt = @"Open Folder";
        panel.message = @"Framekeep will scan this folder and its subfolders for PNG, RAF, and HIF photos.";
        NSURL *current = [command[@"root"] isKindOfClass:NSString.class] ? [NSURL fileURLWithPath:command[@"root"]] : nil;
        panel.directoryURL = current ?: [NSFileManager.defaultManager URLsForDirectory:NSPicturesDirectory inDomains:NSUserDomainMask].firstObject;
        [panel beginSheetModalForWindow:self.window completionHandler:^(NSModalResponse result) {
            if (result != NSModalResponseOK) {
                reply(NSNull.null, nil);
                return;
            }
            [self dispatchCommand:@{@"op": @"open", @"folder": panel.URL.path} reply:reply];
        }];
    } else if ([op isEqualToString:@"startup"]) {
        NSDictionary *startup = self.initialFolder ? @{@"op": @"startup", @"folder": self.initialFolder} : @{@"op": @"startup"};
        [self dispatchCommand:startup reply:reply];
    } else if ([@[@"refresh", @"review", @"favorite", @"format", @"adjust", @"saveNewPhoto",
                  @"recycleAll", @"trashKeptRaws", @"undo", @"undoTrash", @"lutOptions"] containsObject:op ?: @""]) {
        [self dispatchCommand:command reply:reply];
    } else {
        reply(nil, @"Unknown library command.");
    }
}

- (NSEvent *)mouse:(NSEvent *)event {
    if (event.window != self.window || self.window.attachedSheet) return event;
    NSPoint point = [self.web convertPoint:event.locationInWindow fromView:nil];
    CGFloat y = self.web.isFlipped ? point.y : NSHeight(self.web.bounds) - point.y;

    if (event.type == NSEventTypeLeftMouseDown) {
        self.pressedTarget = nil;
        if (y < 30 && point.x > 85) {
            [self.window performWindowDragWithEvent:event];
            return nil;
        }
        NSDictionary *rect = self.dragTarget[@"rect"];
        if (rect && NSPointInRect(NSMakePoint(point.x, y), NSMakeRect([rect[@"x"] doubleValue], [rect[@"y"] doubleValue],
                [rect[@"width"] doubleValue], [rect[@"height"] doubleValue]))) {
            self.pressedTarget = self.dragTarget;
        }
    } else if (event.type == NSEventTypeLeftMouseUp) {
        self.pressedTarget = nil;
    } else if (self.pressedTarget) {
        NSDictionary *target = self.pressedTarget;
        self.pressedTarget = nil;
        NSURL *url = CatalogURL(target[@"generation"], target[@"id"], target[@"format"]);
        if (!url) return event;
        NSDraggingItem *item = [[NSDraggingItem alloc] initWithPasteboardWriter:url];
        NSImage *image = [NSWorkspace.sharedWorkspace iconForFile:url.path];
        [item setDraggingFrame:NSMakeRect(point.x - 25, point.y - 25, 50, 50) contents:image];
        [self.web beginDraggingSessionWithItems:@[item] event:event source:self];
        return nil;
    }
    return event;
}

- (NSDragOperation)draggingSession:(NSDraggingSession *)session sourceOperationMaskForDraggingContext:(NSDraggingContext)context {
    return context == NSDraggingContextOutsideApplication ? NSDragOperationCopy : NSDragOperationNone;
}
@end

void framekeep_run(const char *folder, const char *ui) {
    @autoreleasepool {
        NSApplication *application = NSApplication.sharedApplication;
        __attribute__((objc_precise_lifetime)) FKHost *host = [FKHost new];
        NSURL *bundledUI = [NSBundle.mainBundle.resourceURL URLByAppendingPathComponent:@"ui" isDirectory:YES];
        host.ui = [NSFileManager.defaultManager fileExistsAtPath:[bundledUI URLByAppendingPathComponent:@"index.html"].path]
            ? bundledUI : FileURL(ui, YES);
        host.initialFolder = folder ? FileURL(folder, YES).path : nil;
        NSURL *support = [NSFileManager.defaultManager URLsForDirectory:NSApplicationSupportDirectory inDomains:NSUserDomainMask].firstObject;
        NSURL *preferences = [[support URLByAppendingPathComponent:@"Framekeep" isDirectory:YES] URLByAppendingPathComponent:@"preferences.json"];
        NSString *override = NSProcessInfo.processInfo.environment[@"FRAMEKEEP_PREFERENCES"];
        if (override.length) preferences = [NSURL fileURLWithPath:override];
        framekeep_init(preferences.fileSystemRepresentation);
        application.delegate = host;
        [application run];
    }
}
