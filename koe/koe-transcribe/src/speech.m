// speech.m — Objective-C implementation of the Speech framework wrapper.
//
// Designed after shiguredo/audio-device-rs's audio_coreaudio.m: the Rust side
// only ever calls plain C functions against opaque handles; all Objective-C
// objects live here, under ARC.
//
// Concurrency: a session may be fed from one thread while its result callback
// fires on a serial NSOperationQueue. A single mutex guards `state` and the
// callback/user_data pointers; `in_flight` + a condition variable let
// destroy/cancel wait until any callback currently executing has returned, so
// the Rust-owned user_data outlives the last delivery.

#import <AVFoundation/AVFoundation.h>
#import <CoreFoundation/CoreFoundation.h>
#import <Foundation/Foundation.h>
#import <Speech/Speech.h>
#import <pthread.h>

#include "speech.h"

static const double kKoeSampleRate = 48000.0;
static const AVAudioChannelCount kKoeChannels = 2;

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// Handles a short on-device recognizer probe. Delivers the verdict at most
/// once; owned (retained) by the caller until koe_speech_probe_cancel.
@interface KoeSpeechProbeImpl : NSObject {
@public
  pthread_mutex_t mutex;
  pthread_cond_t cond;
  int destroyed;
  int delivered;
  int in_flight;
  KoeSpeechProbeCallback callback;
  void* user_data;
  SFSpeechRecognizer* recognizer;
  SFSpeechAudioBufferRecognitionRequest* request;
  SFSpeechRecognitionTask* task;
  NSOperationQueue* result_queue;
}
@end

@implementation KoeSpeechProbeImpl
- (instancetype)init {
  self = [super init];
  if (self) {
    pthread_mutex_init(&mutex, NULL);
    pthread_cond_init(&cond, NULL);
  }
  return self;
}
- (void)dealloc {
  pthread_mutex_destroy(&mutex);
  pthread_cond_destroy(&cond);
}
@end

static void koe_probe_deliver(KoeSpeechProbeImpl* self,
                              SFSpeechRecognitionResult* result,
                              NSError* error) {
  pthread_mutex_lock(&self->mutex);
  if (self->destroyed || self->delivered) {
    pthread_mutex_unlock(&self->mutex);
    return;
  }
  self->delivered = 1;
  self->in_flight++;
  KoeSpeechProbeCallback cb = self->callback;
  void* ud = self->user_data;
  pthread_mutex_unlock(&self->mutex);

  if (cb) {
    if (error != nil) {
      NSString* message =
          error.localizedDescription ?: @"speech recognition error";
      cb(ud, KOE_SPEECH_PROBE_RECOVERABLE, message.UTF8String);
    } else if (result != nil) {
      cb(ud, KOE_SPEECH_PROBE_WORKS, NULL);
    } else {
      cb(ud, KOE_SPEECH_PROBE_FAILED, NULL);
    }
  }

  pthread_mutex_lock(&self->mutex);
  self->in_flight--;
  pthread_cond_signal(&self->cond);
  pthread_mutex_unlock(&self->mutex);
}

struct KoeSpeechProbe* koe_speech_probe_start(const char* locale,
                                              KoeSpeechProbeCallback callback,
                                              void* user_data) {
  if (locale == NULL || locale[0] == '\0' || callback == NULL) {
    return NULL;
  }
  NSString* locale_string = [NSString stringWithUTF8String:locale];
  if (locale_string == nil) {
    return NULL;
  }

  KoeSpeechProbeImpl* probe = [[KoeSpeechProbeImpl alloc] init];
  probe->callback = callback;
  probe->user_data = user_data;

  probe->recognizer = [[SFSpeechRecognizer alloc]
      initWithLocale:[NSLocale localeWithLocaleIdentifier:locale_string]];
  if (probe->recognizer == nil || !probe->recognizer.isAvailable) {
    return NULL;
  }

  probe->result_queue = [[NSOperationQueue alloc] init];
  probe->result_queue.maxConcurrentOperationCount = 1;
  [probe->recognizer setQueue:probe->result_queue];

  probe->request = [[SFSpeechAudioBufferRecognitionRequest alloc] init];
  probe->request.requiresOnDeviceRecognition = YES;
  probe->request.shouldReportPartialResults = NO;

  __weak KoeSpeechProbeImpl* weak_probe = probe;
  probe->task = [probe->recognizer
      recognitionTaskWithRequest:probe->request
                   resultHandler:^(SFSpeechRecognitionResult* result,
                                   NSError* error) {
                     KoeSpeechProbeImpl* strong = weak_probe;
                     if (strong != nil) {
                       koe_probe_deliver(strong, result, error);
                     }
                   }];

  return (__bridge_retained struct KoeSpeechProbe*)probe;
}

void koe_speech_probe_cancel(struct KoeSpeechProbe* probe) {
  if (probe == NULL) {
    return;
  }
  KoeSpeechProbeImpl* self =
      (__bridge_transfer KoeSpeechProbeImpl*)probe;
  pthread_mutex_lock(&self->mutex);
  self->destroyed = 1;
  [self->task cancel];
  self->task = nil;
  pthread_mutex_unlock(&self->mutex);

  pthread_mutex_lock(&self->mutex);
  while (self->in_flight > 0) {
    pthread_cond_wait(&self->cond, &self->mutex);
  }
  pthread_mutex_unlock(&self->mutex);
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A streaming recognition session. State: 0 running, 1 finishing (audio feed
/// closed, results may still be delivered), 2 destroyed.
///
/// `feed_lock` serializes appends: the safe API exposes `feed(&self)` on a
/// `Sync` type, and `SFSpeechAudioBufferRecognitionRequest` append ordering is
/// only guaranteed from a single queue. `destroy` acquires `feed_lock` after
/// joining deliveries so a session is never freed while a feed is in flight.
@interface KoeSpeechSessionImpl : NSObject {
@public
  pthread_mutex_t mutex;
  pthread_cond_t cond;
  pthread_mutex_t feed_lock;
  int state;
  int in_flight;
  KoeSpeechCallback callback;
  void* user_data;
  SFSpeechRecognizer* recognizer;
  SFSpeechAudioBufferRecognitionRequest* request;
  SFSpeechRecognitionTask* task;
  NSOperationQueue* result_queue;
  int engine;
}
- (void)koe_handle_result:(SFSpeechRecognitionResult*)result
                    error:(NSError*)error;
@end

@implementation KoeSpeechSessionImpl
- (instancetype)init {
  self = [super init];
  if (self) {
    pthread_mutex_init(&mutex, NULL);
    pthread_mutex_init(&feed_lock, NULL);
    pthread_cond_init(&cond, NULL);
  }
  return self;
}
- (void)dealloc {
  pthread_mutex_destroy(&mutex);
  pthread_mutex_destroy(&feed_lock);
  pthread_cond_destroy(&cond);
}

- (void)koe_handle_result:(SFSpeechRecognitionResult*)result
                    error:(NSError*)error {
  pthread_mutex_lock(&mutex);
  if (state == 2) {
    pthread_mutex_unlock(&mutex);
    return;
  }
  in_flight++;
  KoeSpeechCallback cb = self->callback;
  void* ud = self->user_data;
  pthread_mutex_unlock(&mutex);

  if (cb) {
    if (result != nil) {
      SFTranscription* best = result.bestTranscription;
      if (best != nil) {
        NSString* text = best.formattedString;
        if (text.length > 0) {
          KoeSpeechSegment segment = [self koe_segment:result
                                                   text:text
                                              segments:best.segments];
          cb(ud, KOE_SPEECH_CALLBACK_SEGMENT, &segment, NULL, 0);
        }
      }
      if (result.isFinal) {
        cb(ud, KOE_SPEECH_CALLBACK_FINISHED, NULL, NULL, 1);
      }
    }
    if (error != nil) {
      NSString* message =
          error.localizedDescription ?: @"speech recognition error";
      cb(ud, KOE_SPEECH_CALLBACK_ERROR, NULL, message.UTF8String, 0);
      cb(ud, KOE_SPEECH_CALLBACK_FINISHED, NULL, message.UTF8String, 0);
    }
  }

  pthread_mutex_lock(&mutex);
  in_flight--;
  pthread_cond_signal(&cond);
  pthread_mutex_unlock(&mutex);
}

- (KoeSpeechSegment)koe_segment:(SFSpeechRecognitionResult*)result
                           text:(NSString*)text
                       segments:(NSArray<SFTranscriptionSegment*>*)segments {
  KoeSpeechSegment segment;
  segment.text = text.UTF8String;
  segment.start_ms = 0;
  segment.end_ms = 0;
  segment.confidence = 0.0f;
  segment.is_final = result.isFinal ? 1 : 0;

  int64_t start_ms = 0;
  int64_t end_ms = 0;
  float confidence_sum = 0.0f;
  uint32_t count = 0;
  for (SFTranscriptionSegment* source in segments) {
    int64_t timestamp = (int64_t)llround(source.timestamp * 1000.0);
    int64_t duration = (int64_t)llround(source.duration * 1000.0);
    int64_t source_end = timestamp + duration;
    if (count == 0 || timestamp < start_ms) {
      start_ms = timestamp;
    }
    if (source_end > end_ms) {
      end_ms = source_end;
    }
    confidence_sum += source.confidence;
    count++;
  }
  if (end_ms < start_ms) {
    end_ms = start_ms;
  }
  segment.start_ms = start_ms;
  segment.end_ms = end_ms;
  segment.confidence = count > 0 ? confidence_sum / (float)count : 0.0f;
  return segment;
}
@end

static KoeSpeechSessionImpl* koe_session_impl(struct KoeSpeechSession* session) {
  return (__bridge KoeSpeechSessionImpl*)session;
}

int koe_speech_create(const char* locale, int engine,
                      KoeSpeechCallback callback, void* user_data,
                      struct KoeSpeechSession** out_session) {
  if (locale == NULL || locale[0] == '\0' || callback == NULL ||
      out_session == NULL) {
    return KOE_SPEECH_ERROR_INVALID_ARGUMENT;
  }
  if (engine != KOE_SPEECH_ENGINE_ON_DEVICE &&
      engine != KOE_SPEECH_ENGINE_NETWORK) {
    return KOE_SPEECH_ERROR_INVALID_ARGUMENT;
  }
  if ([SFSpeechRecognizer authorizationStatus] !=
      SFSpeechRecognizerAuthorizationStatusAuthorized) {
    return KOE_SPEECH_ERROR_PERMISSION_DENIED;
  }
  NSString* locale_string = [NSString stringWithUTF8String:locale];
  if (locale_string == nil) {
    return KOE_SPEECH_ERROR_INVALID_ARGUMENT;
  }

  KoeSpeechSessionImpl* session = [[KoeSpeechSessionImpl alloc] init];
  session->engine = engine;
  session->callback = callback;
  session->user_data = user_data;

  session->recognizer = [[SFSpeechRecognizer alloc]
      initWithLocale:[NSLocale localeWithLocaleIdentifier:locale_string]];
  if (session->recognizer == nil || !session->recognizer.isAvailable) {
    return KOE_SPEECH_ERROR_UNSUPPORTED_LOCALE;
  }

  session->result_queue = [[NSOperationQueue alloc] init];
  session->result_queue.maxConcurrentOperationCount = 1;
  [session->recognizer setQueue:session->result_queue];

  session->request = [[SFSpeechAudioBufferRecognitionRequest alloc] init];
  session->request.requiresOnDeviceRecognition =
      (engine == KOE_SPEECH_ENGINE_ON_DEVICE);
  session->request.shouldReportPartialResults = YES;
  session->request.addsPunctuation = YES;

  __weak KoeSpeechSessionImpl* weak_session = session;
  session->task = [session->recognizer
      recognitionTaskWithRequest:session->request
                   resultHandler:^(SFSpeechRecognitionResult* result,
                                   NSError* error) {
                     KoeSpeechSessionImpl* strong = weak_session;
                     if (strong != nil) {
                       [strong koe_handle_result:result error:error];
                     }
                   }];

  *out_session = (__bridge_retained struct KoeSpeechSession*)session;
  return KOE_SPEECH_ERROR_NONE;
}

void koe_speech_destroy(struct KoeSpeechSession* session) {
  if (session == NULL) {
    return;
  }
  KoeSpeechSessionImpl* self = (__bridge_transfer KoeSpeechSessionImpl*)session;
  pthread_mutex_lock(&self->mutex);
  self->state = 2;
  self->callback = NULL;
  self->user_data = NULL;
  [self->task cancel];
  self->task = nil;
  self->request = nil;
  self->recognizer = nil;
  pthread_mutex_unlock(&self->mutex);

  pthread_mutex_lock(&self->mutex);
  while (self->in_flight > 0) {
    pthread_cond_wait(&self->cond, &self->mutex);
  }
  pthread_mutex_unlock(&self->mutex);

  // A concurrent `koe_speech_feed` may still be appending on a strongly
  // retained request; wait for it before releasing the session/mutexes.
  pthread_mutex_lock(&self->feed_lock);
  pthread_mutex_unlock(&self->feed_lock);
}

int koe_speech_feed(struct KoeSpeechSession* session, const float* pcm,
                    size_t frames) {
  KoeSpeechSessionImpl* self = koe_session_impl(session);
  if (self == NULL || pcm == NULL || frames == 0 ||
      frames > (size_t)UINT32_MAX) {
    return -1;
  }
  pthread_mutex_lock(&self->feed_lock);
  SFSpeechAudioBufferRecognitionRequest* request;
  pthread_mutex_lock(&self->mutex);
  if (self->state != 0) {
    pthread_mutex_unlock(&self->mutex);
    pthread_mutex_unlock(&self->feed_lock);
    return -1;
  }
  request = self->request;  // strong local keeps it alive below
  pthread_mutex_unlock(&self->mutex);
  if (request == nil) {
    pthread_mutex_unlock(&self->feed_lock);
    return -1;
  }

  AVAudioFrameCount frame_count = (AVAudioFrameCount)frames;
  AVAudioFormat* format = [[AVAudioFormat alloc]
      initWithCommonFormat:AVAudioPCMFormatFloat32
                sampleRate:kKoeSampleRate
                  channels:kKoeChannels
               interleaved:NO];
  if (format == nil) {
    pthread_mutex_unlock(&self->feed_lock);
    return -1;
  }
  AVAudioPCMBuffer* buffer =
      [[AVAudioPCMBuffer alloc] initWithPCMFormat:format frameCapacity:frame_count];
  if (buffer == nil) {
    pthread_mutex_unlock(&self->feed_lock);
    return -1;
  }
  buffer.frameLength = frame_count;

  float* const* channels = (float* const*)buffer.floatChannelData;
  if (channels == NULL) {
    pthread_mutex_unlock(&self->feed_lock);
    return -1;
  }
  for (AVAudioFrameCount i = 0; i < frame_count; i++) {
    channels[0][i] = pcm[i * 2];
    channels[1][i] = pcm[i * 2 + 1];
  }
  [request appendAudioPCMBuffer:buffer];
  pthread_mutex_unlock(&self->feed_lock);
  return 0;
}

void koe_speech_finish(struct KoeSpeechSession* session) {
  KoeSpeechSessionImpl* self = koe_session_impl(session);
  if (self == NULL) {
    return;
  }
  SFSpeechRecognitionTask* task;
  SFSpeechAudioBufferRecognitionRequest* request;
  pthread_mutex_lock(&self->mutex);
  if (self->state != 0) {
    pthread_mutex_unlock(&self->mutex);
    return;
  }
  self->state = 1;
  task = self->task;
  request = self->request;
  pthread_mutex_unlock(&self->mutex);
  if (request != nil) {
    [request endAudio];
  }
  if (task != nil) {
    [task finish];
  }
}

int koe_speech_engine(struct KoeSpeechSession* session) {
  KoeSpeechSessionImpl* self = koe_session_impl(session);
  if (self == NULL) {
    return -1;
  }
  pthread_mutex_lock(&self->mutex);
  int engine = self->engine;
  pthread_mutex_unlock(&self->mutex);
  return engine;
}

// ---------------------------------------------------------------------------
// System queries
// ---------------------------------------------------------------------------

int koe_speech_authorization_status(void) {
  switch ([SFSpeechRecognizer authorizationStatus]) {
    case SFSpeechRecognizerAuthorizationStatusNotDetermined:
      return KOE_SPEECH_AUTHORIZATION_NOT_DETERMINED;
    case SFSpeechRecognizerAuthorizationStatusDenied:
      return KOE_SPEECH_AUTHORIZATION_DENIED;
    case SFSpeechRecognizerAuthorizationStatusRestricted:
      return KOE_SPEECH_AUTHORIZATION_RESTRICTED;
    case SFSpeechRecognizerAuthorizationStatusAuthorized:
      return KOE_SPEECH_AUTHORIZATION_AUTHORIZED;
    default:
      return KOE_SPEECH_AUTHORIZATION_NOT_DETERMINED;
  }
}

void koe_speech_supported_locales(KoeSpeechLocaleCallback callback,
                                  void* user_data) {
  if (callback == NULL) {
    return;
  }
  NSSet<NSLocale*>* locales = [SFSpeechRecognizer supportedLocales];
  for (NSLocale* locale in locales) {
    const char* identifier = locale.localeIdentifier.UTF8String;
    if (identifier != NULL) {
      callback(user_data, identifier);
    }
  }
}

void koe_speech_runloop_step(double interval) {
  CFRunLoopRunInMode(kCFRunLoopDefaultMode, interval, true);
}
