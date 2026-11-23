#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>
#include <stdio.h>

CGImageRef FramekeepCreateIcon(NSURL *url) CF_RETURNS_RETAINED;

CGImageRef FramekeepCreateIcon(NSURL *url) {
    CGImageSourceRef source = CGImageSourceCreateWithURL((__bridge CFURLRef)url, NULL);
    if (!source) return NULL;
    CGImageRef artwork = CGImageSourceCreateImageAtIndex(source, 0, NULL);
    CFRelease(source);
    if (!artwork) return NULL;

    const size_t canvasSize = 512;
    const CGFloat inset = 48;
    const CGFloat edge = canvasSize - 2 * inset;
    const CGFloat radius = 92;
    CGColorSpaceRef colorSpace = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef context = CGBitmapContextCreate(NULL, canvasSize, canvasSize, 8, 0,
        colorSpace, (CGBitmapInfo)kCGImageAlphaPremultipliedLast);
    CGColorSpaceRelease(colorSpace);
    if (!context) {
        CGImageRelease(artwork);
        return NULL;
    }

    CGContextClearRect(context, CGRectMake(0, 0, canvasSize, canvasSize));
    CGPathRef tile = CGPathCreateWithRoundedRect(CGRectMake(inset, inset, edge, edge),
        radius, radius, NULL);
    CGContextAddPath(context, tile);
    CGPathRelease(tile);
    CGContextClip(context);

    CGFloat width = CGImageGetWidth(artwork);
    CGFloat height = CGImageGetHeight(artwork);
    CGFloat scale = edge / MIN(width, height);
    width *= scale;
    height *= scale;
    CGContextSetInterpolationQuality(context, kCGInterpolationHigh);
    CGContextDrawImage(context, CGRectMake((canvasSize - width) / 2,
        (canvasSize - height) / 2, width, height), artwork);

    CGImageRef icon = CGBitmapContextCreateImage(context);
    CGContextRelease(context);
    CGImageRelease(artwork);
    return icon;
}

#if defined(FRAMEKEEP_ICON_TOOL)
int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc != 3) {
            fputs("Usage: render-icon <source.png> <output.icns>\n", stderr);
            return 1;
        }
        NSURL *input = [NSURL fileURLWithFileSystemRepresentation:argv[1] isDirectory:NO relativeToURL:nil];
        NSURL *output = [NSURL fileURLWithFileSystemRepresentation:argv[2] isDirectory:NO relativeToURL:nil];
        CGImageRef icon = FramekeepCreateIcon(input);
        if (!icon) {
            fprintf(stderr, "Could not render icon from %s\n", argv[1]);
            return 1;
        }
        CGImageDestinationRef destination = CGImageDestinationCreateWithURL(
            (__bridge CFURLRef)output, CFSTR("com.apple.icns"), 1, NULL);
        BOOL saved = NO;
        if (destination) {
            CGImageDestinationAddImage(destination, icon, NULL);
            saved = CGImageDestinationFinalize(destination);
            CFRelease(destination);
        }
        CGImageRelease(icon);
        if (!saved) {
            fprintf(stderr, "Could not write icon to %s\n", argv[2]);
            return 1;
        }
        return 0;
    }
}
#endif
