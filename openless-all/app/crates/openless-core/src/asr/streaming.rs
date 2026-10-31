//! Distinguish an individual stalled write from draining a recording's backlog.

use std::time::Duration;

const BYTES_PER_SECOND: u64 = 32_000;

/// A realtime-only service may need the entire remaining recording to upload.
/// Every socket write is still independently bounded by the provider adapter.
pub(crate) fn drain_timeout(pending_bytes: u64) -> Duration {
    Duration::from_millis(pending_bytes.saturating_mul(1_000) / BYTES_PER_SECOND)
        .saturating_add(Duration::from_secs(30))
}

/// Finalization may still include server processing of buffered audio. This is
/// not the microphone's recording limit, nor an individual TCP write timeout.
pub(crate) fn final_timeout(audio_bytes: u64) -> Duration {
    Duration::from_millis(audio_bytes.saturating_mul(2_000) / BYTES_PER_SECOND)
        .saturating_add(Duration::from_secs(60))
}

/// Never burst a replay into a realtime endpoint. Do not catch up after network
/// stalls by sending a run of overdue frames all at once.
pub(crate) async fn pace_audio(next_send: &mut Option<tokio::time::Instant>, bytes: usize) {
    if let Some(deadline) = *next_send {
        tokio::time::sleep_until(deadline).await;
    }
    *next_send = Some(
        tokio::time::Instant::now()
            + Duration::from_micros(bytes as u64 * 1_000_000 / BYTES_PER_SECOND),
    );
}

/// Split once rather than repeatedly shifting an entire WAV for every frame.
pub(crate) fn drain_chunks(buffer: &mut Vec<u8>, frame_bytes: usize) -> Vec<Vec<u8>> {
    let complete = buffer.len() / frame_bytes * frame_bytes;
    let chunks = buffer[..complete]
        .chunks(frame_bytes)
        .map(<[u8]>::to_vec)
        .collect();
    buffer.drain(..complete);
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_replay_is_not_limited_by_a_single_write_or_live_tail_budget() {
        assert_eq!(
            drain_timeout(90 * BYTES_PER_SECOND),
            Duration::from_secs(120)
        );
        assert_eq!(
            final_timeout(90 * BYTES_PER_SECOND),
            Duration::from_secs(240)
        );
        assert!(drain_timeout(20 * 60 * BYTES_PER_SECOND) > Duration::from_secs(20 * 60));
    }

    #[test]
    fn chunking_keeps_all_bytes_in_order_and_only_retains_the_tail() {
        let mut bytes: Vec<u8> = (0..=255).cycle().take(320_017).collect();
        let original = bytes.clone();
        let chunks = drain_chunks(&mut bytes, 3_200);
        assert!(chunks.iter().all(|chunk| chunk.len() == 3_200));
        assert_eq!(bytes.len(), 17);
        assert_eq!([chunks.concat(), bytes].concat(), original);
    }

    #[tokio::test(start_paused = true)]
    async fn final_response_after_legacy_deadline_is_still_received() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(45)).await;
            let _ = tx.send("complete recording");
        });
        assert_eq!(
            tokio::time::timeout(final_timeout(90 * BYTES_PER_SECOND), rx)
                .await
                .unwrap()
                .unwrap(),
            "complete recording"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn replay_pacing_does_not_burst_after_a_network_stall() {
        let mut next = None;
        let start = tokio::time::Instant::now();
        pace_audio(&mut next, 3_200).await;
        pace_audio(&mut next, 3_200).await;
        assert_eq!(start.elapsed(), Duration::from_millis(100));
        tokio::time::sleep(Duration::from_secs(5)).await;
        pace_audio(&mut next, 3_200).await;
        let after_stall = tokio::time::Instant::now();
        pace_audio(&mut next, 3_200).await;
        assert_eq!(after_stall.elapsed(), Duration::from_millis(100));
    }
}
