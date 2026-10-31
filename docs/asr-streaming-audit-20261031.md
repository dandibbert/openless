# Streaming ASR and recording replay audit

Scope: shared Core cloud ASR implementations, recorder/provider orchestration,
history retranscription, failure retention, and the Tauri capsule bridge. This is
not a security or correctness audit of the entire application or native drivers.

## Confirmed defects addressed

- Soniox accepted cached text as success on peer disconnect. It also parsed
  `error_code` as a string although the documented API sends numeric HTTP codes.
  The failure could bypass retry, continue polishing/insertion, and delete the
  temporary WAV as if recognition had completed normally.
- Live provider failures had no immediate path to the dictation feedback sink.
  A disconnected session could keep a normal recording indicator until release.
  A live transcription fault now publishes an error and a session-bound Stop
  request; Tauri displays an error capsule instead of the recording indicator.
  Stop preserves failure audio and draft; Cancel would discard them.
- Soniox and AssemblyAI used fixed final-result waits; AssemblyAI and the first
  Soniox fix also used a single-write timeout for draining an entire queued WAV.
  Shared budgets now distinguish per-write stalls, backlog drain, and final
  processing. Queue/final budgets scale with audio duration, and the cloud wrapper
  no longer imposes an independent fixed ceiling on those provider waits.
- History retry and stable-mode replay feed whole PCM buffers synchronously.
  Soniox and Xfyun explicitly require near-realtime audio cadence. Their workers
  now pace frames; AssemblyAI also uses conservative realtime pacing. Individual
  socket writes remain bounded independently of a complete replay.
- Bailian, Qwen Realtime, StepFun, Xfyun, and Volcengine also promoted partial
  results to success on errors/disconnects. These paths now fail explicitly.
  Xfyun retains its documented normal server-close completion only after the
  client end marker and queue drain. Drafts remain available through preview.
- StepFun's local grace deadline could complete an open segment using interim
  text. An unsettled segment now cannot satisfy the success criterion.
- Volcengine and Xfyun continued sending the final marker after a short queue
  drain timeout, which could overtake audio still queued for transmission. They
  now wait with an audio-sized drain budget and fail rather than skip the queue.
- Bailian, Volcengine, and Xfyun workers logged failed audio writes but continued.
  They now terminate with an explicit failure; previously unbounded writes are
  bounded. Receive-loop EOF without a Close frame also terminates the result.
- A server finish before client end-of-audio could be accepted as complete.
  Soniox, AssemblyAI, Bailian and Qwen now check that the client end marker has
  reached the serialized send worker first.
- Retranscription disarmed its cancellation guard even when finalization failed.
  Failed finalization now keeps cleanup armed so sockets are cancelled.
- PCM frame extraction repeatedly drained the front of a Vec for each frame,
  shifting all remaining WAV data repeatedly. Shared chunk extraction now copies
  complete frames in one pass and retains only the incomplete tail.
- Soniox included protocol `<end>` / `<fin>` markers in transcript text. Those
  control tokens are now excluded. Idle audio gaps send protocol keepalives.

## Regression evidence

- Real TCP/WebSocket Soniox replay of a complete long recording, including a
  deliberately delayed final response beyond the legacy final-result deadline.
- Independent backlog/final budget and pacing tests with a virtual clock.
- Live disconnect notification before user release; numeric server errors;
  end-marker ordering; no successful result from an early disconnect.
- Backend failure path retains exact archived PCM and draft, marks failed
  history, and never calls polishing or text insertion.
- Provider partial-success regressions, Xfyun normal-vs-early close, and
  retranscription cancellation on failure.

## Remaining risks and follow-up work

- No production API keys or the affected Mac's WAV/logs were available in this
  checkout. Local gateway tests cannot establish the cause of a particular
  production timeout, TLS/proxy drop, microphone gap, or server overload.
- Most realtime adapters still use unbounded send channels. Very large history
  imports can duplicate a complete WAV in memory. The one-pass split removes
  quadratic copying but is not a complete backpressure/streaming-file solution.
- New duration-scaled budgets are bounded but conservative; a peer that stays
  connected without progressing may take too long to fail. Server processed-audio
  watermarks and a separately tested progress/stall watchdog are preferable to
  treating arbitrary traffic as proof of successful recognition.
- Soniox/Xfyun realtime replay intentionally takes approximately the audio
  duration. A separate file/batch transcription API should be considered for
  faster history recovery, with distinct models, credentials and tested cleanup.
- Gemini Live retains its model-specific size and finalization policy. Its
  lifecycle already rejects partial-success disconnects, but its replay drain
  budget and live notification integration need a separate protocol-specific
  review before claiming all providers have identical recovery behavior.
- Qwen/Bailian/StepFun/Volcengine pacing and provider-specific session duration
  limits need real-endpoint acceptance; this patch does not invent undocumented
  server speeds or automatically split arbitrary speech at an unsafe boundary.
- Provider failure notifications are best effort outside the normal dictation
  pipeline (QA, selection voice and other custom sinks use the default no-op
  failure callback unless wired). Tauri and Linux implement the Stop event;
  other native hosts need independent lifecycle acceptance.
- Tauri source and capsule mapping receive contract coverage here, but macOS
  device capture, native capsule visibility and installable DMG acceptance still
  require a macOS runner/device. An old installed build does not gain these fixes.

References: Soniox realtime error handling, connection keepalive and WebSocket
API; AssemblyAI streaming WebSocket API; protocol notes embedded in each adapter.
