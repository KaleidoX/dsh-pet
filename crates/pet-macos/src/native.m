#import <AVFoundation/AVFoundation.h>
#import <VideoToolbox/VideoToolbox.h>
#import <CoreVideo/CoreVideo.h>
#import <Metal/Metal.h>

@interface PetDecoder : NSObject
@property AVAssetReader *reader;
@property AVAssetReaderTrackOutput *output;
@property NSMutableArray *frames;
@property VTDecompressionSessionRef session;
@property double duration;
@property NSMutableArray *samples;
@property NSMutableArray<NSNumber *> *times;
@property BOOL ended;
@property OSStatus status;
@end
@implementation PetDecoder
- (void)dealloc {
    if (_session) { VTDecompressionSessionInvalidate(_session); CFRelease(_session); }
    [_reader cancelReading];
}
@end

static void decoded(void *ref, void *source, OSStatus status, VTDecodeInfoFlags flags, CVImageBufferRef image, CMTime pts, CMTime duration) {
    (void)source; (void)flags; (void)duration;
    PetDecoder *d = (__bridge PetDecoder *)ref;
    if (status) d.status = status;
    if (image && !status) [d.frames addObject:@[@(CMTimeGetSeconds(pts)), (__bridge id)image]];
}
static void error_text(char *out, size_t len, NSString *message) {
    snprintf(out, len, "%s", message.UTF8String);
}
void *pet_decoder_open(const char *path, double *duration, char *error, size_t len) {
    @autoreleasepool {
        PetDecoder *d = [PetDecoder new];
        AVURLAsset *asset = [AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:@(path)] options:nil];
        AVAssetTrack *track = [asset tracksWithMediaType:AVMediaTypeVideo].firstObject;
        if (!track || ![track hasMediaCharacteristic:AVMediaCharacteristicContainsAlphaChannel]) {
            error_text(error, len, @"MOV has no HEVC alpha video track"); return NULL;
        }
        NSError *err = nil;
        d.reader = [[AVAssetReader alloc] initWithAsset:asset error:&err];
        d.output = [[AVAssetReaderTrackOutput alloc] initWithTrack:track outputSettings:nil];
        d.output.alwaysCopiesSampleData = NO;
        if (!d.reader || ![d.reader canAddOutput:d.output]) { error_text(error,len,err.description ?: @"cannot open video track"); return NULL; }
        [d.reader addOutput:d.output];
        if (![d.reader startReading]) { error_text(error,len,d.reader.error.description); return NULL; }
        CMVideoFormatDescriptionRef format = (__bridge CMVideoFormatDescriptionRef)track.formatDescriptions.firstObject;
        NSDictionary *spec = @{(__bridge NSString *)kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder:@YES};
        NSDictionary *attrs = @{(__bridge NSString *)kCVPixelBufferPixelFormatTypeKey:@(kCVPixelFormatType_32BGRA),
            (__bridge NSString *)kCVPixelBufferMetalCompatibilityKey:@YES,
            (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey:@{}};
        VTDecompressionOutputCallbackRecord callback = {decoded, (__bridge void *)d};
        VTDecompressionSessionRef session = NULL;
        OSStatus status = VTDecompressionSessionCreate(NULL,format,(__bridge CFDictionaryRef)spec,(__bridge CFDictionaryRef)attrs,&callback,&session);
        d.session = session;
        CFTypeRef hardware = NULL;
        if (!status) status = VTSessionCopyProperty(session,kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,NULL,&hardware);
        BOOL enabled = hardware && CFEqual(hardware,kCFBooleanTrue);
        if (hardware) CFRelease(hardware);
        if (status || !enabled) { error_text(error,len,[NSString stringWithFormat:@"VideoToolbox hardware decoder unavailable (status %d, hardware %d)",(int)status,enabled]); return NULL; }
        d.frames = [NSMutableArray new];
        d.samples = [NSMutableArray new];
        d.times = [NSMutableArray new];
        CMSampleBufferRef sample;
        while ((sample = [d.output copyNextSampleBuffer])) {
            if (CMSampleBufferGetNumSamples(sample) > 0) {
                double pts = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sample));
                if (!isfinite(pts)) { CFRelease(sample); error_text(error,len,@"invalid video PTS"); return NULL; }
                [d.samples addObject:(__bridge id)sample];
                [d.times addObject:@(pts)];
            }
            CFRelease(sample);
        }
        if (d.reader.status == AVAssetReaderStatusFailed || !d.samples.count) {
            error_text(error,len,d.reader.error.description ?: @"empty video track"); return NULL;
        }
        [d.times sortUsingSelector:@selector(compare:)];
        d.duration = CMTimeGetSeconds(track.timeRange.duration);
        *duration = d.duration;
        return (__bridge_retained void *)d;
    }
}
// Returns one retained CVPixelBuffer in presentation order. The small compressed
// clip is retained at open; its exact PTS index handles B frames and edit offsets
// without assuming a decoder reorder depth or retaining all decoded surfaces.
int pet_decoder_next(void *decoder, void **buffer, double *pts, char *error, size_t len) {
    @autoreleasepool {
        PetDecoder *d = (__bridge PetDecoder *)decoder;
        for (;;) {
            [d.frames sortWithOptions:NSSortStable usingComparator:^NSComparisonResult(NSArray *a, NSArray *b) { return [a[0] compare:b[0]]; }];
            NSArray *first = d.frames.firstObject;
            if (first && (d.times.count && [first[0] isEqualToNumber:d.times.firstObject])) {
                *pts = [first[0] doubleValue];
                *buffer = (void *)CFRetain((__bridge CFTypeRef)first[1]);
                [d.frames removeObjectAtIndex:0];
                [d.times removeObjectAtIndex:0];
                return 1;
            }
            if (d.ended) {
                if (d.times.count || d.frames.count) { error_text(error,len,@"decoded PTS do not match the sample index"); return -1; }
                return 0;
            }
            CMSampleBufferRef sample = d.samples.count ? (CMSampleBufferRef)CFRetain((__bridge CFTypeRef)d.samples.firstObject) : NULL;
            if (sample) [d.samples removeObjectAtIndex:0];
            if (!sample) {
                if (d.reader.status == AVAssetReaderStatusFailed) { error_text(error,len,d.reader.error.description); return -1; }
                OSStatus status = VTDecompressionSessionFinishDelayedFrames(d.session);
                if (!status) status = VTDecompressionSessionWaitForAsynchronousFrames(d.session);
                if (status || d.status) { error_text(error,len,@"VideoToolbox flush failed"); return -1; }
                d.ended = YES;
                continue;
            }
            if (CMSampleBufferGetNumSamples(sample) == 0) { CFRelease(sample); continue; }
            // No asynchronous/temporal flags: callback completes before return.
            OSStatus status = VTDecompressionSessionDecodeFrame(d.session,sample,0,NULL,NULL);
            CFRelease(sample);
            if (status || d.status) { error_text(error,len,[NSString stringWithFormat:@"VideoToolbox decode failed: %d/%d",(int)status,(int)d.status]); return -1; }
        }
    }
}
void pet_decoder_close(void *decoder) { @autoreleasepool { (void)CFBridgingRelease(decoder); } }
void pet_buffer_release(void *buffer) { CFRelease(buffer); }
size_t pet_buffer_width(void *buffer) { return CVPixelBufferGetWidth(buffer); }
size_t pet_buffer_height(void *buffer) { return CVPixelBufferGetHeight(buffer); }
void *pet_texture_cache(void *device) {
    CVMetalTextureCacheRef cache = NULL;
    if (CVMetalTextureCacheCreate(NULL,NULL,(__bridge id<MTLDevice>)device,NULL,&cache)) return NULL;
    return cache;
}
void *pet_texture(void *cache, void *buffer, void **texture) {
    CVMetalTextureRef owner = NULL;
    CVReturn status = CVMetalTextureCacheCreateTextureFromImage(NULL,cache,buffer,NULL,MTLPixelFormatBGRA8Unorm,
        CVPixelBufferGetWidth(buffer),CVPixelBufferGetHeight(buffer),0,&owner);
    if (status || !owner) return NULL;
    *texture = (__bridge void *)CVMetalTextureGetTexture(owner);
    return owner;
}
