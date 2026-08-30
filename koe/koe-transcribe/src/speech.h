// speech.h — pure C surface for the koe-transcribe Speech framework wrapper.
//
// Mirrors the shiguredo/audio-device-rs C boundary: opaque session structs,
// plain function declarations, and callback function pointers with a
// `void* user_data` context. bindgen turns this header into the Rust FFI.
//
// All string pointers (`locale`, `text`, `error_message`) are UTF-8 and are
// only valid for the duration of the call that delivers them; consumers must
// copy them synchronously.

#pragma once

#include <stdint.h>
#include <stddef.h>

#if defined(__cplusplus)
extern "C" {
#endif

// SFSpeechRecognizerAuthorizationStatus values.
enum koe_speech_authorization_status {
  KOE_SPEECH_AUTHORIZATION_NOT_DETERMINED = 0,
  KOE_SPEECH_AUTHORIZATION_DENIED = 1,
  KOE_SPEECH_AUTHORIZATION_RESTRICTED = 2,
  KOE_SPEECH_AUTHORIZATION_AUTHORIZED = 3,
};

// Whether a session recognizes on-device or on Apple's servers. The engine is
// resolved by the caller (via koe_speech_probe_*) before koe_speech_create.
enum koe_speech_engine {
  KOE_SPEECH_ENGINE_ON_DEVICE = 0,
  KOE_SPEECH_ENGINE_NETWORK = 1,
};

// Result of a koe_speech_probe_* call.
enum koe_speech_probe_result {
  KOE_SPEECH_PROBE_UNKNOWN = 0,
  KOE_SPEECH_PROBE_WORKS = 1,
  KOE_SPEECH_PROBE_RECOVERABLE = 2,
  KOE_SPEECH_PROBE_FAILED = 3,
};

// Discriminator for koe_speech_callback's first argument.
enum koe_speech_callback_type {
  KOE_SPEECH_CALLBACK_SEGMENT = 0,  // partial or final transcription; `segment` is valid
  KOE_SPEECH_CALLBACK_FINISHED = 1, // terminal: recognition ended; `done_ok` indicates success
  KOE_SPEECH_CALLBACK_ERROR = 2,    // warning/error; `error_message` is valid
};

// Error codes returned by koe_speech_create (other functions are void/bool).
enum koe_speech_error {
  KOE_SPEECH_ERROR_NONE = 0,
  KOE_SPEECH_ERROR_PERMISSION_DENIED = 1,
  KOE_SPEECH_ERROR_UNSUPPORTED_LOCALE = 2,
  KOE_SPEECH_ERROR_NOT_AVAILABLE = 3,
  KOE_SPEECH_ERROR_ENGINE_ERROR = 4,
  KOE_SPEECH_ERROR_INVALID_ARGUMENT = 5,
};

// Opaque recognition-probe handle; owned by caller until
// koe_speech_probe_cancel.
struct KoeSpeechProbe;

// Opaque recognition session; owned by caller until koe_speech_destroy.
struct KoeSpeechSession;

// One transcription segment (utterance-level timing).
typedef struct {
  const char* text;  // valid for the duration of the callback
  int64_t start_ms;
  int64_t end_ms;
  float confidence;
  int is_final;
} KoeSpeechSegment;

typedef void (*KoeSpeechLocaleCallback)(void* user_data, const char* locale_identifier);

typedef void (*KoeSpeechProbeCallback)(void* user_data, int probe_result,
                                       const char* error_message);

typedef void (*KoeSpeechCallback)(void* user_data, int callback_type,
                                  const KoeSpeechSegment* segment,
                                  const char* error_message, int done_ok);

// Current speech-recognition authorization status
// (KOE_SPEECH_AUTHORIZATION_*).
int koe_speech_authorization_status(void);

// Enumerates the locales supported by the Speech framework, invoking
// `callback` once per locale with its raw identifier (underscore form).
void koe_speech_supported_locales(KoeSpeechLocaleCallback callback, void* user_data);

// Starts a throwaway on-device recognition probe for `locale`; no audio is
// fed. `callback` fires at most once with a KOE_SPEECH_PROBE_* result and an
// optional UTF-8 error message (valid only for that call), after which the
// handle must be released with koe_speech_probe_cancel. Returns NULL when the
// recognizer cannot be created for the locale.
struct KoeSpeechProbe* koe_speech_probe_start(const char* locale,
                                              KoeSpeechProbeCallback callback,
                                              void* user_data);

// Cancels and frees a probe started by koe_speech_probe_start. Safe to call
// after `callback` fired; a no-op on NULL.
void koe_speech_probe_cancel(struct KoeSpeechProbe* probe);

// Creates and starts a recognition session for `locale` using `engine`
// (KOE_SPEECH_ENGINE_*), writing the handle to `*out_session`. `callback`
// fires on the session's result queue; `user_data` is passed through verbatim
// and must remain valid until koe_speech_destroy.
//
// Returns KOE_SPEECH_ERROR_NONE on success, otherwise one of the other
// KOE_SPEECH_ERROR_* values and leaves `*out_session` untouched.
int koe_speech_create(const char* locale, int engine, KoeSpeechCallback callback,
                      void* user_data, struct KoeSpeechSession** out_session);

// Releases a session: cancels the task, joins any in-flight callback, frees
// the session. A no-op on NULL.
void koe_speech_destroy(struct KoeSpeechSession* session);

// Appends `frames` interleaved f32 stereo frames (48 kHz) to the session's
// audio buffer. Returns 0 on success, nonzero on invalid input or after the
// session has been finished/destroyed.
int koe_speech_feed(struct KoeSpeechSession* session, const float* pcm, size_t frames);

// Signals end-of-audio and requests the final result. The recognition callback
// receives KOE_SPEECH_CALLBACK_FINISHED when done. Safe to call once; repeated
// calls are a no-op.
void koe_speech_finish(struct KoeSpeechSession* session);

// The engine in use by the session (KOE_SPEECH_ENGINE_*), or -1 when the
// session is invalid.
int koe_speech_engine(struct KoeSpeechSession* session);

// Pumps the current thread's run loop for at most `interval` seconds so
// pending callback queues can drain (recognizer results are delivered on a
// background queue, but a CLI main thread may need to yield).
void koe_speech_runloop_step(double interval);

#if defined(__cplusplus)
}
#endif
