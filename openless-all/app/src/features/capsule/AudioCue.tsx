import { useEffect, useRef } from 'react';
import { createCapsuleSubscriptions } from '../../api/dictation';
import { getSettings } from '../../api/preferences';
import { getRuntimeEnvironment } from '../../runtime/environment';
import { playRecordStartCue, primeAudioCue, stopAudioCue } from '../../lib/audioCue';
import type { CapsuleState } from '../../api/contracts';

export function AudioCueListener() {
  const audioCueEnabledRef = useRef(true);
  const prevStateRef = useRef<CapsuleState>('idle');
  const { host, os } = getRuntimeEnvironment();
  const audioCueRuntimeEnabled = host === 'tauri' && os !== 'android';
  const macNativeCue = host === 'tauri' && os === 'mac';

  useEffect(() => {
    if (!audioCueRuntimeEnabled) return;
    const events = createCapsuleSubscriptions();
    let disposed = false;
    let owned = false;
    let preferenceRevision = 0;

    void (async () => {
      // Opening another WebView must never play a cue for an existing session.
      if ((await events.label()) !== 'capsule' || disposed) return;
      owned = true;
      prevStateRef.current = 'idle';
      // Never start WebKit's output client just to prime the start cue on macOS.
      if (!macNativeCue) primeAudioCue();
      if (
        !(await events.preferences((next) => {
          preferenceRevision += 1;
          if (next) audioCueEnabledRef.current = next.audioCueOnRecord !== false;
        }))
      )
        return;
      if (
        !(await events.state(({ state }) => {
          const previous = prevStateRef.current;
          prevStateRef.current = state;
          if (state === 'recording' && previous !== 'recording') {
            if (audioCueEnabledRef.current) {
              if (macNativeCue) {
                void import('@tauri-apps/api/core')
                  .then(({ invoke }) => invoke('play_native_audio_cue'))
                  .catch(() => {});
              } else {
                playRecordStartCue();
              }
            }
          } else if (state !== 'recording' && previous === 'recording') {
            if (macNativeCue) {
              void import('@tauri-apps/api/core')
                .then(({ invoke }) => invoke('stop_native_audio_cue'))
                .catch(() => {});
            } else {
              stopAudioCue();
            }
          }
        }))
      )
        return;
      const observed = preferenceRevision;
      const preferences = await getSettings();
      if (!disposed && observed === preferenceRevision) {
        audioCueEnabledRef.current = preferences.audioCueOnRecord !== false;
      }
    })().catch(() => {});

    return () => {
      disposed = true;
      events.dispose();
      if (owned) {
        if (macNativeCue) {
          void import('@tauri-apps/api/core')
            .then(({ invoke }) => invoke('stop_native_audio_cue'))
            .catch(() => {});
        } else {
          stopAudioCue();
        }
      }
    };
  }, [audioCueRuntimeEnabled, macNativeCue]);

  return null;
}
