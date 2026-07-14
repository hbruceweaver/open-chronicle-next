# Capture privacy behavior

Open Chronicle records a factual outcome for each scheduled attempt while keeping
denied content out of the evidence record.

## What may be recorded

For an approved exact foreground window, Chronicle may record application bundle ID,
process name, permitted window title, normalized content hash, on-device OCR, image
dimensions, and a local managed screenshot reference. OCR is always marked untrusted.
Its provenance states the Vision adapter/request revision, whether automatic language
detection was requested, and the configured/requested language list. That list is not
a detected-language claim. Failed OCR remains the coarse `failed` state with no OCR
payload.

Idle collection is limited to aggregate seconds since the last input event and the
configured threshold. Chronicle does not install an event tap and never records key or
button values, coordinates, clipboard data, typed text, or an input-event stream.

## What is never recorded for a denial

Password managers, sensitive system credential surfaces, Chronicle itself, user-
excluded applications, excluded title fragments, and secure-input surfaces produce a
closed protected reason plus policy version only. Pause, study expiry, lock/sleep,
permission loss, no exact window, and ambiguous mapping produce a closed no-evidence
reason only. These payloads cannot contain bundle ID, process name, PID, title, OCR,
image identity, bytes, or freeform details.

Mail and messaging applications are not silently blocked by default. Users may add
them to their visible exclusions based on their own work context.

The onboarding capture proof uses one memory-only token scoped to one
Chronicle-owned test window and fixed synthetic text. An invalid, reused, or
non-Chronicle-scoped token cannot fall through to ordinary capture. Proof attempts
never persist captured pixels or OCR text.

## Local evidence and retention

Screenshots and OCR remain local in MVP. Duplicate approved pixels reuse the latest
durable OCR/image evidence and skip Vision/HEIC work. Screenshot expiry removes the
managed image through an additive lifecycle but does not erase the factual event, OCR,
or chunk; evidence deletion is a separate explicit action.

MCP never receives screenshot bytes or managed filesystem paths. OCR and other content
remain subject to explicit, bounded, revocable disclosure grants described in
`docs/privacy-model.md`.
